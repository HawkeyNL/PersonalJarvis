//! Automatic web search (`research_web_search: "auto"`). After an ordinary
//! realtime answer the classifier marks as research, or that ends with the
//! [`WEB_SEARCH_MARKER`] line the chat model may add, Core may start one
//! Research run by itself. The model only asks; the search always uses the
//! owner's latest question, never the model's text. That question then
//! leaves the Home Node without the owner's click, so a deterministic
//! privacy gate keeps questions with personal or financial details on the
//! click (the offer), and a cap bounds how often it happens. Names and other free-text personal details
//! are not detectable; a question that only carries those still searches.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jarvis_llm::privacy::is_private;
use jarvis_market_data::wallet::{evm_checksum, WalletKind, ASSETS, TOKENS};
use rust_decimal::Decimal;

use crate::crypto::store::{StoredBalance, Wallet};
use crate::AppState;

const PER_HOUR: usize = 10;
const PER_DAY: usize = 40;
const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOLDINGS_TIMEOUT: Duration = Duration::from_secs(2);

/// The final line a chat reply carries to ask for an automatic web search;
/// `job_markers::take` takes it off. A marker anywhere else is ordinary text
/// and asks for nothing.
pub(crate) const WEB_SEARCH_MARKER: &str = "[[web_search]]";

/// The end of the text before the last non-blank line, and that line trimmed.
pub(crate) fn last_line(text: &str) -> (usize, &str) {
    let body = text.trim_end();
    let start = body.rfind('\n').map_or(0, |at| at + 1);
    (body[..start].trim_end().len(), body[start..].trim_start())
}

/// Most text [`MarkerHold`] holds back: room for every marker an answer may
/// carry at its longest (3 jobs, one of Jarvis's own, the web search, one
/// chat search, one roadmap). `job_markers::take` takes no marker beyond it, so what it takes
/// off was never streamed. A `[[roadmap` line is the exception: it is held
/// and taken at any length (the reply's own token cap bounds it), so an
/// oversized one is refused instead of shown.
pub(crate) const MAX_HOLD_BYTES: usize = 5 * crate::routes::jobs::BODY_LIMIT
    + crate::roadmap_markers::MAX_ROADMAP_LINE
    + crate::job_markers::MAX_CHAT_SEARCH_LINE;

/// Whether a trailing marker line, `from_end` bytes from the end of the
/// reply, is too far back to hold or take.
pub(crate) fn beyond_hold(from_end: usize, line: &str) -> bool {
    from_end > MAX_HOLD_BYTES && !line.starts_with("[[roadmap")
}

/// Streams reply deltas without trailing markers (this one, the job and the
/// roadmap markers): trailing whitespace and the trailing lines that start
/// with `[[`, or could, wait for more text, up to [`MAX_HOLD_BYTES`] (a
/// `[[roadmap` line always waits). What is held back when the stream ends is
/// never sent; the completed message carries the final text.
#[derive(Default)]
pub(crate) struct MarkerHold {
    text: String,
    sent: usize,
}

impl MarkerHold {
    /// The part of the reply that is now safe to send.
    pub(crate) fn push(&mut self, delta: &str) -> &str {
        self.text.push_str(delta);
        let mut end = self.text.trim_end().len();
        loop {
            let (keep, line) = last_line(&self.text[..end]);
            let marker = line.starts_with("[[") || "[[".starts_with(line);
            if line.is_empty() || !marker || beyond_hold(self.text.len() - keep, line) {
                break;
            }
            end = keep;
        }
        let end = end.max(self.sent);
        let ready = &self.text[self.sent..end];
        self.sent = end;
        ready
    }
}

/// One run at a time, at most `per_hour` per hour and `per_day` per day.
/// The default is the automatic web search's: [`PER_HOUR`] and [`PER_DAY`];
/// explicit Research runs never count.
pub struct RunCaps {
    per_hour: usize,
    per_day: usize,
    in_flight: AtomicBool,
    // ponytail: in memory, so the caps reset on a Core restart; persist the
    // start times if restarts ever become frequent.
    starts: Mutex<VecDeque<Instant>>,
}

impl Default for RunCaps {
    fn default() -> Self {
        Self::with_caps(PER_HOUR, PER_DAY)
    }
}

/// The single run slot; dropping it frees the slot.
pub(crate) struct AutoSlot(Arc<RunCaps>);

impl Drop for AutoSlot {
    fn drop(&mut self) {
        self.0.in_flight.store(false, Ordering::Release);
    }
}

impl RunCaps {
    pub fn with_caps(per_hour: usize, per_day: usize) -> Self {
        Self {
            per_hour,
            per_day,
            in_flight: AtomicBool::new(false),
            starts: Mutex::default(),
        }
    }

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
        if starts.len() >= self.per_day || last_hour >= self.per_hour {
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

/// Every word that names something the owner holds: the symbols of the last
/// IBKR sync (the snapshot runtime facts read) and [`wallet_words`]. Stored
/// wallets count even while crypto wallets are off: their rows stay. `None`,
/// so the gate fails closed, when a read fails or takes longer than
/// [`HOLDINGS_TIMEOUT`], or the IBKR snapshot does not list every held
/// symbol.
pub(crate) async fn held_symbols(state: &AppState) -> Option<Vec<String>> {
    let ibkr = async {
        if state.ibkr.is_none() {
            return Some(Vec::new());
        }
        every_held_symbol(crate::ibkr::store::load_portfolio(&state.db).await.ok())
    };
    let read = async {
        tokio::join!(
            ibkr,
            crate::crypto::store::list(&state.db),
            crate::crypto::store::balances(&state.db),
        )
    };
    let (ibkr, wallets, balances) = tokio::time::timeout(HOLDINGS_TIMEOUT, read).await.ok()?;
    merge(ibkr, wallets.ok().zip(balances.ok()))
}

/// The IBKR symbols plus the wallet words; `None` when either is unknown.
fn merge(
    ibkr: Option<Vec<String>>,
    wallets: Option<(Vec<Wallet>, Vec<StoredBalance>)>,
) -> Option<Vec<String>> {
    let (wallets, balances) = wallets?;
    let mut held = ibkr?;
    held.extend(wallet_words(&wallets, &balances));
    Some(held)
}

/// What the crypto wallets give away: every stored address (an EVM one also
/// in its EIP-55 form; key-like already, so a second line of defence); the native coin of every chain of every wallet,
/// pending ones too; and every allowlisted asset with a balance above zero.
/// Coins with their symbol and aliases (`ETH`, `ether`, `ethereum`).
fn wallet_words(wallets: &[Wallet], balances: &[StoredBalance]) -> Vec<String> {
    let natives = wallets
        .iter()
        .flat_map(|wallet| &wallet.chains)
        .filter_map(|chain| {
            TOKENS
                .iter()
                .find(|token| token.chain == *chain && token.contract.is_none())
                .map(|token| token.asset)
        });
    let held = balances
        .iter()
        .filter(|balance| balance.amount > Decimal::ZERO)
        .map(|balance| balance.asset.as_str());
    let mut words: Vec<String> = natives
        .chain(held)
        .filter_map(|key| ASSETS.iter().find(|asset| asset.key == key))
        .flat_map(|asset| std::iter::once(asset.symbol).chain(asset.aliases.iter().copied()))
        .map(str::to_owned)
        .collect();
    for wallet in wallets {
        words.push(wallet.address.clone());
        if wallet.kind == WalletKind::Evm {
            words.push(evm_checksum(&wallet.address));
        }
    }
    words.sort_unstable();
    words.dedup();
    words
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn wallets_add_addresses_natives_and_held_coins_and_fail_closed() {
        use jarvis_market_data::wallet::Chain;
        let wallet = |id: &str, kind, address: &str, chains| Wallet {
            id: id.into(),
            kind,
            address: address.into(),
            chains,
            label: Some("Zebra vault".into()),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            last_ok_at: None,
            error: None,
            other_tokens: None,
        };
        let balance = |wallet_id: &str, chain, asset: &str, amount: &str| StoredBalance {
            wallet_id: wallet_id.into(),
            chain,
            asset: asset.into(),
            amount: amount.parse().unwrap(),
        };
        let solana = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
        let wallets = [
            // Pending: no balances read yet, its chains' natives count.
            wallet(
                "a",
                WalletKind::Evm,
                "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
                vec![Chain::Base, Chain::Polygon],
            ),
            wallet("b", WalletKind::Solana, solana, vec![Chain::Solana]),
        ];
        let balances = [
            balance("b", Chain::Solana, "usdc", "12.5"),
            balance("b", Chain::Solana, "usdt", "0"),
            balance("b", Chain::Solana, "scam", "1000"),
        ];
        let words = wallet_words(&wallets, &balances);
        for expected in [
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            solana,
            "ETH",
            "ether",
            "ethereum",
            "POL",
            "matic",
            "polygon",
            "SOL",
            "solana",
            "USDC",
            "usd coin",
        ] {
            assert!(words.iter().any(|word| word == expected), "{expected}");
        }
        // A zero balance, an asset off the allowlist and the label are not held.
        for absent in ["USDT", "tether", "scam", "Zebra vault", "BTC"] {
            assert!(!words.iter().any(|word| word == absent), "{absent}");
        }

        let ibkr = Some(vec!["ASML".to_owned()]);
        let held = merge(ibkr.clone(), Some((wallets.to_vec(), balances.to_vec()))).unwrap();
        for private in [
            "Will ASML rise?",
            "Is Ether a good buy?",
            "news about usd coin",
            "What does 0x5AAEB6053F3E94C9B9A09F33669435E7EF1BEAED hold?",
            "Polygon upgrade",
        ] {
            assert!(is_private(private, &held), "{private}");
        }
        assert!(!is_private("Will cardano rise?", &held));
        // Addresses are key-like on their own already; the words are a
        // second line of defence.
        assert!(is_private(&format!("Who sent to {solana}?"), &[]));
        // A failed or late read on either side fails closed.
        assert_eq!(merge(None, Some((wallets.to_vec(), Vec::new()))), None);
        assert_eq!(merge(ibkr, None), None);
        assert_eq!(
            merge(Some(Vec::new()), Some((Vec::new(), Vec::new()))),
            Some(Vec::new())
        );
    }

    #[test]
    fn other_caps_count_the_same_way() {
        let caps = Arc::new(RunCaps::with_caps(2, 3));
        let start = Instant::now();
        assert!(caps.try_start_at(start).is_some());
        assert!(caps.try_start_at(start + HOUR / 4).is_some());
        assert!(caps.try_start_at(start + HOUR / 2).is_none());
        assert!(caps.try_start_at(start + HOUR).is_some());
        assert!(caps.try_start_at(start + 2 * HOUR).is_none());
    }

    #[test]
    fn one_run_at_a_time_within_hourly_and_daily_caps() {
        let auto = Arc::new(RunCaps::default());
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

    #[test]
    fn only_a_trailing_marker_line_is_taken() {
        let take = |text: &str| {
            let mut text = text.to_owned();
            let marked = crate::job_markers::take(&mut text).web_search;
            (marked, text)
        };
        assert_eq!(
            take("Short answer.\n[[web_search]]"),
            (true, "Short answer.".into())
        );
        assert_eq!(
            take("Short answer.  \n\n  [[web_search]] \n\n"),
            (true, "Short answer.".into())
        );
        assert_eq!(take("[[web_search]]"), (true, String::new()));
        assert_eq!(take(" \n[[web_search]]\n"), (true, String::new()));
        for unmarked in [
            "Short answer.",
            "",
            "Use [[web_search]] to search.\nDone.",
            "[[web_search]]\nMore text.",
            "Short answer. [[web_search]]",
            "Short answer.\n[[web_search]] please",
            "Short answer.\n[[WEB_SEARCH]]",
        ] {
            assert_eq!(take(unmarked), (false, unmarked.into()), "{unmarked:?}");
        }
    }

    #[test]
    fn streaming_never_sends_the_marker() {
        let stream = |deltas: &[&str]| {
            let mut hold = MarkerHold::default();
            deltas
                .iter()
                .map(|d| hold.push(d).to_owned())
                .collect::<String>()
        };
        assert_eq!(
            stream(&["Short ", "answer.\n", "[[web", "_search", "]]", "\n"]),
            "Short answer."
        );
        assert_eq!(stream(&["[[", "web_search]]"]), "");
        // Held text that is not the marker is sent once more text follows.
        assert_eq!(
            stream(&["Line one.\n", "[[web", " is not it.\nEnd."]),
            "Line one.\n[[web is not it.\nEnd."
        );
        assert_eq!(stream(&["Hello ", "world", "."]), "Hello world.");
        // A marker in the middle is ordinary text.
        assert_eq!(
            stream(&["A\n", "[[web_search]]", "\nB"]),
            "A\n[[web_search]]\nB"
        );
        // Multi-byte text is split on character boundaries only.
        assert_eq!(stream(&["Café ", "\u{e9}t\u{e9}\n", "[["]), "Café été");
        // Every trailing line that starts with `[[` waits: the job markers too.
        assert_eq!(
            stream(&[
                "Answer.\n",
                "[[job {\"kind\":",
                "\"disk_check\"}]]\n[[web_search]]\n",
                "[",
                "[job_self {}]]\n"
            ]),
            "Answer."
        );
        assert_eq!(
            stream(&["A\n[[x]]\n", "[[y]]\n", "B"]),
            "A\n[[x]]\n[[y]]\nB"
        );
        // Bounded: beyond the cap the oldest held lines are sent.
        let links = format!("A\n{}", "[[link]]\n".repeat(6000));
        let sent = stream(&[&links]);
        assert!(sent.len() < links.trim_end().len());
        assert!(sent.len() + MAX_HOLD_BYTES >= links.trim_end().len());
        // A roadmap line is never sent, however long, nor what it holds
        // back behind it; text before it still is.
        let roadmap = format!(
            "Plan.\n[[roadmap {{\"title\":\"T\",\"description\":\"{}\"}}]]\n[[web_search]]",
            "a".repeat(2 * MAX_HOLD_BYTES)
        );
        let chunks: Vec<&str> = roadmap
            .as_bytes()
            .chunks(1000)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect();
        let mut hold = MarkerHold::default();
        for chunk in &chunks {
            let ready = hold.push(chunk);
            assert!(
                !ready.contains("[[roadmap") && !ready.contains("[[web"),
                "{ready}"
            );
        }
        assert_eq!(stream(&chunks), "Plan.");
    }
}
