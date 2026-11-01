//! Promotion gate from paper trading to live.
//!
//! Live trading may only be considered after paper trading has shown, over
//! enough trades and enough time, that the strategy makes money **after every
//! modelled cost**: fees inside each trade, failed transactions and the AI
//! spend that drove it. A good week of luck should not pass.
//!
//! This only produces a report. It never enables execution: going live stays
//! an owner action with a signed mandate.

use std::collections::BTreeMap;

use rust_decimal::Decimal;
use time::{Duration, OffsetDateTime};

use crate::paper::{checked_sum, Ledger, LedgerError, Mark};

/// Thresholds for the gate. Owner-supplied; no defaults here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateRules {
    pub min_closed_trades: usize,
    pub min_duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateReport {
    pub closed_trades: usize,
    /// From the first counted trade's open to `now`.
    pub duration: Duration,
    /// Closed trades, plus open lots only if they are losing, minus failed-tx
    /// fees and AI cost.
    pub net_result_usd: Decimal,
    /// `net_result_usd / closed_trades`; zero without trades.
    pub net_expectancy_usd: Decimal,
    pub enough_trades: bool,
    pub enough_time: bool,
    pub positive_expectancy: bool,
    pub passed: bool,
}

/// Evaluate the paper record as of `now`. Only trades closed by `now` count
/// as evidence, and time is measured from the record itself, not from a
/// caller-supplied start. Open lots never help, but an open loss counts
/// against the result: a strategy that sells winners and sits on losers
/// must not pass. `marks` value those open lots as in [`Ledger::pnl`].
pub fn evaluate(
    ledger: &Ledger,
    marks: &BTreeMap<String, Mark>,
    now: OffsetDateTime,
    rules: &GateRules,
) -> Result<GateReport, LedgerError> {
    let counted = || ledger.closed_trades().iter().filter(|t| t.closed_at <= now);
    let closed_trades = counted().count();
    let duration = counted()
        .map(|t| t.opened_at)
        .min()
        .map_or(Duration::ZERO, |first| (now - first).max(Duration::ZERO));
    let realised = checked_sum(counted().map(|t| t.net_pnl_usd))?;
    let open_loss = ledger.pnl(marks)?.unrealised_usd.min(Decimal::ZERO);
    let net_result_usd = checked_sum(
        [
            realised,
            open_loss,
            -ledger.failed_tx_fees_usd(),
            -ledger.ai_cost_usd(),
        ]
        .into_iter(),
    )?;
    let net_expectancy_usd = if closed_trades == 0 {
        Decimal::ZERO
    } else {
        net_result_usd / Decimal::from(closed_trades)
    };
    let enough_trades = closed_trades >= rules.min_closed_trades;
    let enough_time = duration >= rules.min_duration;
    let positive_expectancy = net_expectancy_usd > Decimal::ZERO;
    Ok(GateReport {
        closed_trades,
        duration,
        net_result_usd,
        net_expectancy_usd,
        enough_trades,
        enough_time,
        positive_expectancy,
        passed: enough_trades && enough_time && positive_expectancy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{d, t0};
    use crate::types::Position;

    const RULES: GateRules = GateRules {
        min_closed_trades: 10,
        min_duration: Duration::days(7),
    };

    fn lot(mint: &str) -> Position {
        Position {
            mint: mint.to_owned(),
            tokens: d("100"),
            cost_usd: d("10"),
            opened_at: t0(),
            entry_liquidity_usd: d("100000"),
        }
    }

    /// `n` round trips opened at `t0` and closed a day later, each netting `pnl`.
    fn ledger(n: usize, pnl: &str) -> Ledger {
        let mut ledger = Ledger::default();
        for _ in 0..n {
            ledger.open(lot("A")).unwrap();
            ledger
                .close(
                    "A",
                    d("100"),
                    d("10") + d(pnl),
                    d("0"),
                    t0() + Duration::days(1),
                )
                .unwrap();
        }
        ledger
    }

    fn eval(l: &Ledger, now: OffsetDateTime) -> GateReport {
        evaluate(l, &BTreeMap::new(), now, &RULES).unwrap()
    }

    #[test]
    fn passes_only_when_all_three_hold() {
        let week = t0() + Duration::days(7);
        assert!(eval(&ledger(10, "0.5"), week).passed);

        let few = eval(&ledger(9, "0.5"), week);
        assert!(!few.passed && !few.enough_trades && few.enough_time);

        let short = eval(&ledger(10, "0.5"), week - Duration::seconds(1));
        assert!(!short.passed && !short.enough_time);

        let flat = eval(&ledger(10, "0"), week);
        assert!(!flat.passed && !flat.positive_expectancy);
    }

    #[test]
    fn costs_outside_trades_can_fail_the_gate() {
        let week = t0() + Duration::days(7);
        let mut l = ledger(10, "0.5"); // +5 USD from trades
        l.record_failed_tx(d("1")).unwrap();
        l.record_ai_cost(d("4")).unwrap();
        let report = eval(&l, week);
        assert_eq!(report.net_result_usd, d("0"));
        assert!(!report.passed);

        let mut l = ledger(10, "0.5");
        l.record_ai_cost(d("3")).unwrap();
        let report = eval(&l, week);
        assert_eq!(report.net_expectancy_usd, d("0.2"));
        assert!(report.passed);
    }

    #[test]
    fn open_losers_count_and_open_winners_do_not() {
        let week = t0() + Duration::days(7);
        // +5 from trades, but a lot bought for 10 with no usable mark is a -10.
        let mut l = ledger(10, "0.5");
        l.open(lot("RUG")).unwrap();
        let report = eval(&l, week);
        assert_eq!(report.net_result_usd, d("-5"));
        assert!(!report.passed);

        let mut l = ledger(10, "0.5");
        l.open(lot("UP")).unwrap();
        let marks = BTreeMap::from([(
            "UP".to_owned(),
            Mark {
                price_usd: d("10"),
                liquidity_usd: d("1000000000"),
            },
        )]);
        let report = evaluate(&l, &marks, week, &RULES).unwrap();
        assert_eq!(report.net_result_usd, d("5"), "an open winner adds nothing");
    }

    #[test]
    fn empty_backwards_or_future_records_fail() {
        let none = GateRules {
            min_closed_trades: 0,
            min_duration: Duration::ZERO,
        };
        let empty = evaluate(&Ledger::default(), &BTreeMap::new(), t0(), &none).unwrap();
        assert_eq!(empty.net_expectancy_usd, Decimal::ZERO);
        assert!(!empty.passed);

        // Asked about a moment before the trades closed: they do not count.
        let early = eval(&ledger(10, "1"), t0());
        assert_eq!(early.closed_trades, 0);
        assert_eq!(early.duration, Duration::ZERO);
        assert!(!early.passed);
    }
}
