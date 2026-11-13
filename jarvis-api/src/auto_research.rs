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

use crate::AppState;

const PER_HOUR: usize = 10;
const PER_DAY: usize = 40;
const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOLDINGS_TIMEOUT: Duration = Duration::from_secs(2);

/// The final line a chat reply carries to ask for an automatic web search.
pub(crate) const WEB_SEARCH_MARKER: &str = "[[web_search]]";

/// Removes a last line that is exactly [`WEB_SEARCH_MARKER`] (surrounding
/// whitespace allowed) and the whitespace before it; true when it did. A
/// marker anywhere else is ordinary text and asks for nothing.
pub(crate) fn take_marker(text: &mut String) -> bool {
    let (keep, line) = last_line(text);
    if line != WEB_SEARCH_MARKER {
        return false;
    }
    text.truncate(keep);
    true
}

/// The end of the text before the last non-blank line, and that line trimmed.
pub(crate) fn last_line(text: &str) -> (usize, &str) {
    let body = text.trim_end();
    let start = body.rfind('\n').map_or(0, |at| at + 1);
    (body[..start].trim_end().len(), body[start..].trim_start())
}

/// Most text [`MarkerHold`] holds back: room for every marker an answer may
/// carry at its longest (3 jobs, one of Jarvis's own, the web search). Trailing
/// `[[` lines beyond it are sent and only the completed message drops them.
const MAX_HOLD_BYTES: usize = 5 * crate::routes::jobs::BODY_LIMIT;

/// Streams reply deltas without trailing markers (this one and the job
/// markers): trailing whitespace and the trailing lines that start with `[[`,
/// or could, wait for more text, up to [`MAX_HOLD_BYTES`]. What is held back
/// when the stream ends is never sent; the completed message carries the
/// final text.
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
            if line.is_empty() || !marker || self.text.len() - keep > MAX_HOLD_BYTES {
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
pub(crate) async fn held_symbols(state: &AppState) -> Option<Vec<String>> {
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

    #[test]
    fn only_a_trailing_marker_line_is_taken() {
        let take = |text: &str| {
            let mut text = text.to_owned();
            let marked = take_marker(&mut text);
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
        let links = format!("A\n{}", "[[link]]\n".repeat(4000));
        let sent = stream(&[&links]);
        assert!(sent.len() < links.trim_end().len());
        assert!(sent.len() + MAX_HOLD_BYTES >= links.trim_end().len());
    }
}
