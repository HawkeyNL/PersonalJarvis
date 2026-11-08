//! Automatic web search (`research_web_search: "auto"`). After an ordinary
//! realtime answer the classifier marks as research, Core may start one
//! Research run by itself. The latest question then leaves the Home Node
//! without the owner's click, so a deterministic privacy gate keeps questions
//! with personal or financial details on the click (the offer), and a cap
//! bounds how often it happens. Names and other free-text personal details
//! are not detectable; a question that only carries those still searches.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::AppState;

const PER_HOUR: usize = 10;
const PER_DAY: usize = 40;
const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOLDINGS_TIMEOUT: Duration = Duration::from_secs(2);
/// How many words after a possessive still count as near.
const POSSESSIVE_REACH: usize = 3;
/// With the typographic apostrophe phone keyboards insert, too.
const POSSESSIVES: [&str; 7] = ["my", "our", "mijn", "m'n", "m’n", "ons", "onze"];
/// Prefixes, so plurals and compounds match too ("posities", "brokerage").
const PERSONAL_WORDS: [&str; 29] = [
    "portfolio",
    "positie",
    "position",
    "holding",
    "account",
    "rekening",
    "ibkr",
    "broker",
    "salaris",
    "salary",
    "wachtwoord",
    "password",
    "aandeel",
    "aandelen",
    "belegging",
    "hypotheek",
    "pensioen",
    "saldo",
    "spaar",
    "schuld",
    "share",
    "stock",
    "invest",
    "mortgage",
    "pension",
    "balance",
    "debt",
    "loan",
    "lening",
];
/// Currency words that make a number an amount (whole words, any case).
const CURRENCY_WORDS: [&str; 7] = ["eur", "euro", "euros", "usd", "dollar", "dollars", "gbp"];
/// A contiguous digit run this long is private (phone, account numbers).
const MIN_DIGIT_RUN: usize = 6;
/// Digits split by single separators (`4111 1111`, `06-1234`) are private
/// from this many digits on, or from [`MIN_DIGIT_RUN`] groups on (`1 2 3 4 5 6`).
const MIN_SEPARATED_DIGITS: usize = 8;
const KEY_PREFIXES: [&str; 3] = ["sk-", "ghp_", "AKIA"];
const MIN_KEY_RUN: usize = 24;

/// At most one automatic run at a time, at most [`PER_HOUR`] per hour and
/// [`PER_DAY`] per day. Explicit Research runs never count.
#[derive(Default)]
pub struct AutoResearch {
    in_flight: AtomicBool,
    // ponytail: in memory, so the caps reset on a Core restart; persist the
    // start times if restarts ever become frequent.
    starts: Mutex<VecDeque<Instant>>,
}

/// The single automatic run slot; dropping it frees the slot.
pub(crate) struct AutoSlot(Arc<AutoResearch>);

impl Drop for AutoSlot {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::Release);
    }
}

impl AutoResearch {
    /// Takes the slot and counts a run, or `None` when a run is in flight or
    /// a cap is reached.
    pub(crate) fn try_start(self: &Arc<Self>) -> Option<AutoSlot> {
        self.try_start_at(Instant::now())
    }

    fn try_start_at(self: &Arc<Self>, now: Instant) -> Option<AutoSlot> {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        // From here every early return drops the slot and frees it again.
        let slot = AutoSlot(self.clone());
        let mut starts = self.starts.lock().ok()?;
        while starts
            .front()
            .is_some_and(|start| now.duration_since(*start) >= DAY)
        {
            starts.pop_front();
        }
        let last_hour = starts
            .iter()
            .filter(|start| now.duration_since(**start) < HOUR)
            .count();
        if starts.len() >= PER_DAY || last_hour >= PER_HOUR {
            return None;
        }
        starts.push_back(now);
        drop(starts);
        Some(slot)
    }
}

/// The automatic slot when every check passes; `None` keeps today's offer.
/// The cheap checks run before the holdings read, the cap last so a refused
/// question never uses up a run.
pub(crate) async fn start(
    state: &AppState,
    research_advised: bool,
    question: &str,
) -> Option<AutoSlot> {
    if !research_advised
        || !state.model_routing.snapshot().research_auto()
        || !state.llm.can_research()
        || jarvis_llm::ResearchRequest::new(question).is_none()
    {
        return None;
    }
    let held = held_symbols(state).await?;
    if is_private(question, &held) {
        return None;
    }
    state.auto_research.try_start()
}

/// The symbols the owner holds per the last IBKR sync (the snapshot runtime
/// facts read). `None`, so the gate fails closed, when IBKR is linked but the
/// snapshot cannot be read or does not list every held symbol.
async fn held_symbols(state: &AppState) -> Option<Vec<String>> {
    if state.ibkr.is_none() {
        return Some(Vec::new());
    }
    let read = tokio::time::timeout(
        HOLDINGS_TIMEOUT,
        crate::ibkr::store::load_portfolio(&state.db),
    )
    .await;
    every_held_symbol(read.ok().and_then(Result::ok))
}

/// `snapshot` is `None` when the read failed or timed out, `Some(None)`
/// before the first sync.
fn every_held_symbol(
    snapshot: Option<Option<crate::ibkr::store::Portfolio>>,
) -> Option<Vec<String>> {
    match snapshot? {
        None => Some(Vec::new()),
        Some(portfolio) if portfolio.skipped.len() >= portfolio.skipped_count => Some(
            portfolio
                .positions
                .into_iter()
                .map(|holding| holding.symbol)
                .chain(portfolio.skipped.into_iter().map(|skipped| skipped.symbol))
                .collect(),
        ),
        Some(_) => None,
    }
}

/// Whether the question may carry personal or financial details and must
/// stay an offer: an email address, a long number, a currency amount, a
/// key-like token, a possessive near a money or account word, or a held
/// symbol.
pub(crate) fn is_private(question: &str, held_symbols: &[String]) -> bool {
    let lower = question.to_lowercase();
    has_email(question)
        || has_digit_run(question)
        || has_amount(&lower)
        || has_key(question)
        || has_possessive(&lower)
        || held_symbols
            .iter()
            .any(|symbol| has_word(&lower, &symbol.trim().to_lowercase()))
}

fn has_email(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        token.split_once('@').is_some_and(|(local, domain)| {
            local.chars().any(char::is_alphanumeric)
                && domain.split_once('.').is_some_and(|(host, tld)| {
                    !host.is_empty() && tld.chars().any(char::is_alphanumeric)
                })
        })
    })
}

/// 6+ contiguous digits, or digit groups joined by single ` `, `-`, `.` or
/// `/` with 8+ digits or 6+ groups in total (IBAN, card and phone numbers).
fn has_digit_run(text: &str) -> bool {
    let (mut total, mut groups, mut group, mut after_separator) = (0, 0, 0, false);
    for c in text.chars() {
        if c.is_ascii_digit() {
            groups += usize::from(group == 0);
            group += 1;
            total += 1;
            after_separator = false;
            if group >= MIN_DIGIT_RUN || total >= MIN_SEPARATED_DIGITS || groups >= MIN_DIGIT_RUN {
                return true;
            }
        } else if group > 0 && !after_separator && matches!(c, ' ' | '-' | '.' | '/') {
            after_separator = true;
            group = 0;
        } else {
            (total, groups, group, after_separator) = (0, 0, 0, false);
        }
    }
    false
}

/// A currency sign or word with a number anywhere in the question, or a
/// number with a `k` suffix ("80k").
fn has_amount(lower: &str) -> bool {
    let currency =
        lower.contains(['€', '$', '£']) || CURRENCY_WORDS.iter().any(|word| has_word(lower, word));
    (currency && lower.contains(|c: char| c.is_ascii_digit()))
        || runs(lower, char::is_alphanumeric).any(|word| {
            word.strip_suffix('k').is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
            })
        })
}

fn has_key(text: &str) -> bool {
    text.contains("-----BEGIN")
        || KEY_PREFIXES
            .iter()
            .any(|prefix| has_prefix_word(text, prefix))
        || runs(text, |c| c.is_ascii_alphanumeric()).any(|run| {
            run.len() >= MIN_KEY_RUN
                && run.contains(|c: char| c.is_ascii_digit())
                && run.contains(|c: char| c.is_ascii_alphabetic())
        })
}

fn has_possessive(lower: &str) -> bool {
    let words: Vec<&str> = runs(lower, |c| c.is_alphanumeric() || c == '\'' || c == '’').collect();
    words.iter().enumerate().any(|(index, word)| {
        POSSESSIVES.contains(word)
            && words[index + 1..]
                .iter()
                .take(POSSESSIVE_REACH)
                .any(|next| {
                    PERSONAL_WORDS
                        .iter()
                        .any(|personal| next.starts_with(personal))
                })
    })
}

/// Maximal runs of characters that satisfy `keep`.
fn runs(text: &str, keep: impl Fn(char) -> bool) -> impl Iterator<Item = &str> {
    text.split(move |c: char| !keep(c))
        .filter(|run| !run.is_empty())
}

/// `needle` (already lower case) as a whole word: no letter or digit
/// directly before or after it.
fn has_word(lower: &str, needle: &str) -> bool {
    !needle.is_empty()
        && lower.match_indices(needle).any(|(at, _)| {
            !lower[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && !lower[at + needle.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        })
}

/// `prefix` at the start of a word, case sensitive.
fn has_prefix_word(text: &str, prefix: &str) -> bool {
    text.match_indices(prefix).any(|(at, _)| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_gate_table() {
        let held = ["ASML".to_owned(), "BRK.B".to_owned()];
        for (question, private) in [
            ("weer Amsterdam morgen", false),
            ("What is the latest Rust release?", false),
            ("Wat is de koers van EUR/USD vandaag?", false),
            ("Wie won de Tour de France in 2024?", false),
            ("Is a risk-free rate the same as a bond yield?", false),
            ("Hoe werkt een Nederlandse bankrekening?", false),
            ("Mijn fiets is gestolen, wat nu?", false),
            ("mail jan.devries@example.nl over het weer", true),
            ("mijn telefoon is 0612345678", true),
            ("NL91ABNA0417164300 rente?", true),
            ("is €2500 per maand veel?", true),
            ("is 2500 EUR per maand veel?", true),
            ("$40 for a flight to Rome?", true),
            ("salary of 80k in Amsterdam", true),
            ("is sk-proj-abc123 still valid?", true),
            ("leaked ghp_abcdef?", true),
            ("what does AKIAIOSFODNN7EXAMPLE belong to?", true),
            ("-----BEGIN OPENSSH PRIVATE KEY-----", true),
            ("token a1b2c3d4e5f6g7h8i9j0k1l2m3", true),
            ("hoe doet mijn portfolio het?", true),
            ("Hoe staan mijn IBKR posities?", true),
            ("how is my brokerage account doing", true),
            ("is m'n wachtwoord veilig?", true),
            ("hoe staat m’n rekening ervoor?", true),
            ("my salary vs inflation", true),
            ("ASML nieuws vandaag", true),
            ("nieuws over asml?", true),
            ("koers brk.b", true),
            ("ASMLX nieuws", false),
            ("Rust 1.82.0 release notes", false),
            ("top 10 films van 2024", false),
            ("hoe laat is het in Tokyo om 9 uur?", false),
            ("waarom is ons land zo nat?", false),
            ("NL91 ABNA 0417 1643 00", true),
            ("is 4111 1111 1111 1111 een testkaart?", true),
            ("bel 06 1234 5678", true),
            ("bel +31 6 12 34 56 78", true),
            ("06-1234-5678 nummer van wie?", true),
            ("1 2 3 4 5 6", true),
            ("is 2500 euro per maand veel?", true),
            ("100 dollars to yen", true),
            ("is £300 a lot?", true),
            ("300 GBP in EUR", true),
            ("hoe staat mijn aandelen pakket?", true),
            ("onze hypotheek rente", true),
            ("is our mortgage too high", true),
            ("my pension fund returns", true),
            ("my stocks today", true),
        ] {
            assert_eq!(is_private(question, &held), private, "{question}");
        }
        // Without holdings a symbol is just a word.
        assert!(!is_private("ASML nieuws vandaag", &[]));
        assert!(!is_private("blank symbol", &[" ".to_owned()]));
    }

    #[test]
    fn holdings_fail_closed_unless_every_held_symbol_is_known() {
        use crate::ibkr::store::{Holding, Portfolio, Skipped};
        let portfolio = |skipped_count| Portfolio {
            as_of: time::OffsetDateTime::UNIX_EPOCH,
            net_liquidation: Default::default(),
            cash: Default::default(),
            positions: vec![Holding {
                conid: 1,
                symbol: "ASML".into(),
                name: None,
                asset_class: "STK".into(),
                currency: "EUR".into(),
                quantity: Default::default(),
                avg_cost: Default::default(),
                last_price: Default::default(),
                market_value: Default::default(),
                unrealized_pnl: None,
            }],
            skipped: vec![Skipped {
                symbol: "AAPL".into(),
                currency: Some("USD".into()),
            }],
            skipped_count,
        };
        // Read error or timeout.
        assert_eq!(every_held_symbol(None), None);
        // Linked, never synced: nothing is held yet.
        assert_eq!(every_held_symbol(Some(None)), Some(Vec::new()));
        assert_eq!(
            every_held_symbol(Some(Some(portfolio(1)))),
            Some(vec!["ASML".to_owned(), "AAPL".to_owned()])
        );
        // More skipped positions than the snapshot names.
        assert_eq!(every_held_symbol(Some(Some(portfolio(2)))), None);
    }

    #[test]
    fn one_run_at_a_time_within_hourly_and_daily_caps() {
        let auto = Arc::new(AutoResearch::default());
        let start = Instant::now();
        let held = auto.try_start_at(start).unwrap();
        // Busy: refused, and a refusal is not counted.
        assert!(auto.try_start_at(start).is_none());
        drop(held);
        for minute in 1..PER_HOUR as u64 {
            assert!(auto
                .try_start_at(start + Duration::from_secs(minute * 60))
                .is_some());
        }
        assert!(auto.try_start_at(start + HOUR / 2).is_none());
        // A new hour opens the slot again, until the day is full.
        let mut at = start + HOUR;
        let mut started = PER_HOUR;
        while started < PER_DAY {
            assert!(auto.try_start_at(at).is_some(), "run {started}");
            started += 1;
            at += HOUR / PER_HOUR as u32;
        }
        assert!(auto
            .try_start_at(start + DAY - Duration::from_secs(1))
            .is_none());
        // A day after the first runs, they no longer count.
        assert!(auto
            .try_start_at(start + DAY + Duration::from_secs(10 * 60))
            .is_some());
    }
}
