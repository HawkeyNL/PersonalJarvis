//! Promotion gate from paper trading to live.
//!
//! Live trading may only be considered after paper trading has shown, over
//! enough trades and enough time, that the strategy makes money **after every
//! modelled cost**: fees inside each trade, failed transactions and the AI
//! spend that drove it. A good week of luck should not pass.
//!
//! This only produces a report. It never enables execution: going live stays
//! an owner action with a signed mandate.
//!
//! The minimum trade count, duration and number of trading days have floors
//! fixed in code ([`MIN_CLOSED_TRADES`], [`MIN_DURATION`],
//! [`MIN_TRADING_DAYS`]). Configuration may raise them, never lower them.
//! Trading days are distinct UTC dates on which a round trip closed, so one
//! long hold plus a burst of quick trades cannot stand in for weeks of
//! trading. The ledger refuses out-of-order events, so these timestamps
//! cannot be back-dated.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use time::{Duration, UtcOffset};

use crate::paper::{checked_sum, CostModel, Ledger, LedgerError, Mark};

/// Code-fixed floor for [`GateRules::min_closed_trades`].
pub const MIN_CLOSED_TRADES: usize = 100;
/// Code-fixed floor for [`GateRules::min_duration`].
pub const MIN_DURATION: Duration = Duration::days(28);
/// Code-fixed floor for [`GateRules::min_trading_days`].
pub const MIN_TRADING_DAYS: usize = 7;

/// Gate thresholds from configuration. They can only be stricter than the
/// floors; [`evaluate`] applies the floors even if `validate` was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateRules {
    pub min_closed_trades: usize,
    pub min_duration: Duration,
    pub min_trading_days: usize,
}

/// A configured gate threshold below its code-fixed floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("gate rule `{field}` is below its code-fixed floor")]
pub struct BelowFloor {
    pub field: &'static str,
}

impl GateRules {
    pub fn validate(&self) -> Result<(), BelowFloor> {
        if self.min_closed_trades < MIN_CLOSED_TRADES {
            return Err(BelowFloor {
                field: "min_closed_trades",
            });
        }
        if self.min_duration < MIN_DURATION {
            return Err(BelowFloor {
                field: "min_duration",
            });
        }
        if self.min_trading_days < MIN_TRADING_DAYS {
            return Err(BelowFloor {
                field: "min_trading_days",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateReport {
    /// Completed round trips.
    pub closed_trades: usize,
    /// From the earliest round trip's open to the latest one's close.
    pub duration: Duration,
    /// Round trips, plus losing open positions (never winning ones), minus
    /// failed-tx fees and AI cost.
    pub net_result_usd: Decimal,
    /// `net_result_usd / closed_trades`; zero without trades.
    pub net_expectancy_usd: Decimal,
    pub enough_trades: bool,
    pub enough_time: bool,
    /// Distinct UTC dates on which a round trip closed.
    pub trading_days: usize,
    pub enough_days: bool,
    pub positive_expectancy: bool,
    pub passed: bool,
}

/// Evaluate the paper record. Everything comes from the ledger itself, not
/// from a caller-supplied clock or start time. Open positions never help,
/// but each losing one counts against the result (see
/// [`Ledger::open_losses`]): a strategy that sells winners and sits on
/// losers must not pass. `marks` and `costs` value those positions.
pub fn evaluate(
    ledger: &Ledger,
    marks: &BTreeMap<String, Mark>,
    costs: &CostModel,
    rules: &GateRules,
) -> Result<GateReport, LedgerError> {
    let trades = ledger.closed_trades();
    let closed_trades = trades.len();
    let first_open = trades.iter().map(|t| t.opened_at).min();
    let last_close = trades.iter().map(|t| t.closed_at).max();
    let duration = match (first_open, last_close) {
        (Some(first), Some(last)) => (last - first).max(Duration::ZERO),
        _ => Duration::ZERO,
    };
    let trading_days = trades
        .iter()
        .map(|t| t.closed_at.to_offset(UtcOffset::UTC).date())
        .collect::<BTreeSet<_>>()
        .len();
    let realised = checked_sum(trades.iter().map(|t| t.net_pnl_usd))?;
    let net_result_usd = checked_sum(
        [
            realised,
            ledger.open_losses(marks, costs)?,
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
    let enough_trades = closed_trades >= rules.min_closed_trades.max(MIN_CLOSED_TRADES);
    let enough_time = duration >= rules.min_duration.max(MIN_DURATION);
    let enough_days = trading_days >= rules.min_trading_days.max(MIN_TRADING_DAYS);
    let positive_expectancy = net_expectancy_usd > Decimal::ZERO;
    Ok(GateReport {
        closed_trades,
        duration,
        trading_days,
        enough_days,
        net_result_usd,
        net_expectancy_usd,
        enough_trades,
        enough_time,
        positive_expectancy,
        passed: enough_trades && enough_time && enough_days && positive_expectancy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{costs, d, t0};
    use crate::types::Position;
    use time::OffsetDateTime;

    const FLOORS: GateRules = GateRules {
        min_closed_trades: MIN_CLOSED_TRADES,
        min_duration: MIN_DURATION,
        min_trading_days: MIN_TRADING_DAYS,
    };

    fn lot(mint: &str, at: OffsetDateTime) -> Position {
        Position {
            mint: mint.to_owned(),
            tokens: d("100"),
            cost_usd: d("10"),
            opened_at: at,
            entry_liquidity_usd: d("100000"),
        }
    }

    /// `n` back-to-back round trips spread evenly from `t0` to `t0 + span`,
    /// each netting `pnl`.
    fn ledger(n: usize, pnl: &str, span: Duration) -> Ledger {
        let mut ledger = Ledger::default();
        let step = span / n as u32;
        for i in 0..n as u32 {
            ledger.open(lot("A", t0() + step * i)).unwrap();
            let close_at = if i + 1 == n as u32 {
                t0() + span
            } else {
                t0() + step * (i + 1)
            };
            ledger
                .close("A", d("100"), d("10") + d(pnl), d("0"), close_at)
                .unwrap();
        }
        ledger
    }

    fn eval(l: &Ledger) -> GateReport {
        evaluate(l, &BTreeMap::new(), &costs(), &FLOORS).unwrap()
    }

    #[test]
    fn passes_only_when_all_three_hold() {
        let n = MIN_CLOSED_TRADES;
        assert!(eval(&ledger(n, "0.5", MIN_DURATION)).passed);

        let few = eval(&ledger(n - 1, "0.5", MIN_DURATION));
        assert!(!few.passed && !few.enough_trades && few.enough_time);

        let short = eval(&ledger(n, "0.5", MIN_DURATION - Duration::seconds(1)));
        assert!(!short.passed && !short.enough_time);

        let flat = eval(&ledger(n, "0", MIN_DURATION));
        assert!(!flat.passed && !flat.positive_expectancy);
    }

    #[test]
    fn floors_cannot_be_lowered() {
        assert_eq!(FLOORS.validate(), Ok(()));
        let raised = GateRules {
            min_closed_trades: MIN_CLOSED_TRADES + 50,
            min_duration: MIN_DURATION + Duration::days(7),
            min_trading_days: MIN_TRADING_DAYS + 3,
        };
        assert_eq!(raised.validate(), Ok(()));
        let fewer = GateRules {
            min_closed_trades: MIN_CLOSED_TRADES - 1,
            ..FLOORS
        };
        assert_eq!(
            fewer.validate(),
            Err(BelowFloor {
                field: "min_closed_trades"
            })
        );
        let fewer_days = GateRules {
            min_trading_days: MIN_TRADING_DAYS - 1,
            ..FLOORS
        };
        assert_eq!(
            fewer_days.validate(),
            Err(BelowFloor {
                field: "min_trading_days"
            })
        );
        let shorter = GateRules {
            min_duration: MIN_DURATION - Duration::seconds(1),
            ..FLOORS
        };
        assert_eq!(
            shorter.validate(),
            Err(BelowFloor {
                field: "min_duration"
            })
        );
        // Even unvalidated low rules do not lower the bar.
        let lax = GateRules {
            min_closed_trades: 1,
            min_duration: Duration::ZERO,
            min_trading_days: 1,
        };
        let report = evaluate(
            &ledger(10, "1", Duration::days(1)),
            &BTreeMap::new(),
            &costs(),
            &lax,
        )
        .unwrap();
        assert!(!report.passed);
    }

    #[test]
    fn costs_outside_trades_can_fail_the_gate() {
        let n = MIN_CLOSED_TRADES;
        let mut l = ledger(n, "0.5", MIN_DURATION); // +50 USD from trades
        l.record_failed_tx(d("10")).unwrap();
        l.record_ai_cost(d("40")).unwrap();
        let report = eval(&l);
        assert_eq!(report.net_result_usd, d("0"));
        assert!(!report.passed);

        let mut l = ledger(n, "0.5", MIN_DURATION);
        l.record_ai_cost(d("30")).unwrap();
        let report = eval(&l);
        assert_eq!(report.net_expectancy_usd, d("0.2"));
        assert!(report.passed);
    }

    #[test]
    fn open_losers_count_and_open_winners_do_not_offset_them() {
        let mut l = ledger(MIN_CLOSED_TRADES, "0.5", MIN_DURATION); // +50
        let later = t0() + MIN_DURATION;
        l.open(lot("UP", later)).unwrap();
        l.open(lot("RUG", later)).unwrap();
        // UP is worth far more than RUG lost, but must not cancel it out.
        let marks = BTreeMap::from([(
            "UP".to_owned(),
            Mark {
                price_usd: d("10"),
                liquidity_usd: d("1000000000"),
            },
        )]);
        let report = evaluate(&l, &marks, &costs(), &FLOORS).unwrap();
        // RUG has no usable mark: its full cost of 10 counts as lost.
        assert_eq!(report.net_result_usd, d("40"));

        l.open(lot("RUG2", later)).unwrap();
        for _ in 0..4 {
            l.open(lot("RUG3", later)).unwrap();
        }
        let report = evaluate(&l, &marks, &costs(), &FLOORS).unwrap();
        assert_eq!(report.net_result_usd, d("-10"));
        assert!(!report.passed);
    }

    #[test]
    fn empty_record_fails() {
        let empty = eval(&Ledger::default());
        assert_eq!(empty.duration, Duration::ZERO);
        assert_eq!(empty.net_expectancy_usd, Decimal::ZERO);
        assert!(!empty.passed);
    }

    #[test]
    fn one_long_hold_plus_a_burst_does_not_pass() {
        // A single hold spans the whole period; every other trade happens in
        // one burst on its last day.
        let mut l = Ledger::default();
        l.open(lot("HOLD", t0())).unwrap();
        for i in 0..MIN_CLOSED_TRADES as i64 - 1 {
            let at = t0() + Duration::days(27) + Duration::minutes(i);
            l.open(lot("A", at)).unwrap();
            l.close("A", d("100"), d("11"), d("0"), at).unwrap();
        }
        l.close("HOLD", d("100"), d("11"), d("0"), t0() + MIN_DURATION)
            .unwrap();
        let report = eval(&l);
        assert!(report.enough_trades && report.enough_time && report.positive_expectancy);
        assert_eq!(report.trading_days, 2);
        assert!(!report.enough_days && !report.passed);
    }
}
