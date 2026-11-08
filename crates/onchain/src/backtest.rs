//! Replay a [`StrategyConfig`] over recorded observation series.
//!
//! Pure and deterministic: the same config, series and cost model always
//! give the same report. Observations of all tokens are replayed in time
//! order (ties by mint), so the per-day entry limit and the per-token
//! cooldown apply across tokens as they would live. An entry signal fills at
//! the token's *next* observation, never at the price that triggered it: live
//! trading always pays some latency between decision and fill.
//!
//! Fills use the size-aware paper fills of [`crate::paper`] and P&L the
//! paper [`Ledger`], so a backtest is never kinder than paper trading.
//! Failed transactions are not drawn at random: every fill also pays the
//! expected fees of failed attempts before it, `fees × p / (1 - p)` for a
//! failure rate `p`. A position still open when its series ends is sold at
//! the last observation, or written off if that cannot be priced.
//!
//! Of the hard safety filter, only the rules a price series can evaluate are
//! applied here: minimum liquidity and minimum pool age of the mandate's rules
//! tightened by the config. Everything else (authorities, extensions, LP,
//! holder concentration, sell route) is assumed checked when the series was
//! recorded.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::{Decimal, RoundingStrategy};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::config::{ConfigError, StrategyConfig};
use crate::filter::FilterRules;
use crate::paper::{self, checked_sum, CostModel, Fill, Ledger, LedgerError, Mark};
use crate::signals::{Observation, Series};
use crate::strategy::{self, ExitReason};
use crate::types::Position;

const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// One simulated round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestTrade {
    pub mint: String,
    pub opened_at: OffsetDateTime,
    pub closed_at: OffsetDateTime,
    /// `None`: the series ended with the position still open.
    pub reason: Option<ExitReason>,
    pub cost_usd: Decimal,
    pub net_pnl_usd: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BacktestReport {
    /// In close order.
    pub trades: Vec<BacktestTrade>,
    pub net_usd: Decimal,
    /// Mean net P&L per trade; zero without trades.
    pub expectancy_usd: Decimal,
    /// Share of trades with a positive net P&L, bps.
    pub win_rate_bps: u32,
    /// Largest drop of cumulative net P&L from its running high (from zero).
    pub max_drawdown_usd: Decimal,
    /// Gross wins over gross losses; `None` without losing trades.
    pub profit_factor: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BacktestError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cost model is invalid")]
    InvalidCosts,
    #[error("series for `{0}` is out of order or has a price or liquidity of zero or less")]
    InvalidSeries(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

struct Open {
    position: Position,
    peak_value_usd: Option<Decimal>,
}

/// Replay `config` over `series_by_token`. `base` is the mandate's filter
/// rules and `max_position_usd` its per-position limit, which the entry size
/// is a share of.
pub fn backtest(
    config: &StrategyConfig,
    base: &FilterRules,
    series_by_token: &BTreeMap<String, Series>,
    costs: &CostModel,
    max_position_usd: Decimal,
) -> Result<BacktestReport, BacktestError> {
    let filter = config.validate(base)?;
    if !costs.is_valid() {
        return Err(BacktestError::InvalidCosts);
    }
    for (mint, series) in series_by_token {
        let ordered = series.points.windows(2).all(|w| w[0].at < w[1].at);
        let positive = series
            .points
            .iter()
            .all(|p| p.price_usd > Decimal::ZERO && p.liquidity_usd > Decimal::ZERO);
        if !(ordered && positive) {
            return Err(BacktestError::InvalidSeries(mint.clone()));
        }
    }

    let mut events: Vec<(OffsetDateTime, &str, usize)> = series_by_token
        .iter()
        .flat_map(|(mint, s)| {
            s.points
                .iter()
                .enumerate()
                .map(move |(i, p)| (p.at, mint.as_str(), i))
        })
        .collect();
    events.sort();

    let rules = config.exit_rules();
    let mut ledger = Ledger::default();
    let mut open: BTreeMap<&str, Open> = BTreeMap::new();
    let mut last_exit: BTreeMap<&str, OffsetDateTime> = BTreeMap::new();
    let mut entries_by_day: BTreeMap<Date, u32> = BTreeMap::new();
    // Mints whose entry signal fired on their previous observation.
    let mut pending: BTreeSet<&str> = BTreeSet::new();
    let mut trades = Vec::new();

    for (now, mint, i) in events {
        let series = &series_by_token[mint];
        let obs = series.points[i];
        let mark = Mark {
            price_usd: obs.price_usd,
            liquidity_usd: obs.liquidity_usd,
        };
        if let Some(held) = open.get_mut(mint) {
            let reason = strategy::exit_reason(
                &held.position,
                held.peak_value_usd,
                Some(mark),
                &rules,
                costs,
                now,
            );
            held.peak_value_usd =
                strategy::track_peak(held.peak_value_usd, &held.position, mark, costs);
            let last = i + 1 == series.points.len();
            if reason.is_some() || last {
                if let Some(trade) = close(&mut ledger, &held.position, obs, reason, last, costs)? {
                    trades.push(trade);
                    open.remove(mint);
                    last_exit.insert(mint, now);
                }
            }
            continue;
        }

        let day = now.to_offset(UtcOffset::UTC).date();
        let day_full = entries_by_day.get(&day).copied().unwrap_or(0) >= config.max_entries_per_day;
        // No entry on the last observation: it could never be exited.
        let last = i + 1 == series.points.len();
        if !pending.remove(mint) {
            let cooling = last_exit
                .get(mint)
                .is_some_and(|at| now - *at < config.token_cooldown());
            let filtered = obs.liquidity_usd >= filter.min_liquidity_usd
                && now - series.pool_created_at >= filter.min_pool_age;
            if !(cooling || day_full || last) && filtered && config.entry_signal(series, now) {
                pending.insert(mint);
            }
            continue;
        }
        if day_full || last {
            continue;
        }
        let Some(position) = enter(config, mint, obs, costs, max_position_usd) else {
            continue;
        };
        ledger.open(position.clone())?;
        *entries_by_day.entry(day).or_default() += 1;
        open.insert(
            mint,
            Open {
                position,
                peak_value_usd: None,
            },
        );
    }

    report(trades)
}

fn enter(
    config: &StrategyConfig,
    mint: &str,
    obs: Observation,
    costs: &CostModel,
    max_position_usd: Decimal,
) -> Option<Position> {
    let by_mandate = max_position_usd.checked_mul(Decimal::from(config.position_share_bps))? / BPS;
    let by_liquidity = obs
        .liquidity_usd
        .checked_mul(Decimal::from(config.max_liquidity_share_bps))?
        / BPS;
    let size = by_mandate
        .min(by_liquidity)
        .round_dp_with_strategy(2, RoundingStrategy::ToZero);
    if size <= Decimal::ZERO {
        return None;
    }
    let (tokens, fees) = fill(paper::buy(
        size,
        obs.price_usd,
        obs.liquidity_usd,
        costs,
        costs.failure_rate,
    ))?;
    Some(Position {
        mint: mint.to_owned(),
        tokens,
        cost_usd: size.checked_add(fees.checked_add(failed_fees(costs)?)?)?,
        opened_at: obs.at,
        entry_liquidity_usd: obs.liquidity_usd,
    })
}

/// Sell the whole position. `Ok(None)` when the sell cannot be filled (it is
/// retried on the next observation); on the last observation of a series an
/// unfillable position is written off instead.
fn close(
    ledger: &mut Ledger,
    position: &Position,
    obs: Observation,
    reason: Option<ExitReason>,
    last: bool,
    costs: &CostModel,
) -> Result<Option<BacktestTrade>, BacktestError> {
    let sold = fill(paper::sell(
        position.tokens,
        obs.price_usd,
        obs.liquidity_usd,
        costs,
        costs.failure_rate,
    ))
    .and_then(|(usd_out, fees)| Some((usd_out, fees.checked_add(failed_fees(costs)?)?)));
    let (usd_out, fees) = match sold {
        Some(sold) => sold,
        None if last => (Decimal::ZERO, Decimal::ZERO),
        None => return Ok(None),
    };
    let net_pnl_usd = ledger.close(&position.mint, position.tokens, usd_out, fees, obs.at)?;
    Ok(Some(BacktestTrade {
        mint: position.mint.clone(),
        opened_at: position.opened_at,
        closed_at: obs.at,
        reason,
        cost_usd: position.cost_usd,
        net_pnl_usd,
    }))
}

/// Amount out and fees of a fill. The roll passed in is the failure rate,
/// which always fills (when the inputs can be priced at all).
fn fill(fill: Option<Fill>) -> Option<(Decimal, Decimal)> {
    match fill? {
        Fill::Filled {
            amount_out,
            fees_usd,
        } => Some((amount_out, fees_usd)),
        Fill::Failed { .. } => None,
    }
}

/// Expected fees of the failed attempts before a success.
fn failed_fees(costs: &CostModel) -> Option<Decimal> {
    let p = costs.failure_rate;
    let per_attempt = costs.network_fee_usd.checked_add(costs.priority_fee_usd)?;
    per_attempt.checked_mul(p)?.checked_div(Decimal::ONE - p)
}

fn report(trades: Vec<BacktestTrade>) -> Result<BacktestReport, BacktestError> {
    let pnls = || trades.iter().map(|t| t.net_pnl_usd);
    let net_usd = checked_sum(pnls())?;
    let count = Decimal::from(trades.len());
    let overflow = || BacktestError::Ledger(LedgerError::Overflow);
    let expectancy_usd = if trades.is_empty() {
        Decimal::ZERO
    } else {
        net_usd.checked_div(count).ok_or_else(overflow)?
    };
    let wins = trades
        .iter()
        .filter(|t| t.net_pnl_usd > Decimal::ZERO)
        .count();
    let win_rate_bps = if trades.is_empty() {
        0
    } else {
        u32::try_from(wins * 10_000 / trades.len()).unwrap_or(10_000)
    };
    let gross_win = checked_sum(pnls().filter(|v| *v > Decimal::ZERO))?;
    let gross_loss = -checked_sum(pnls().filter(|v| *v < Decimal::ZERO))?;
    let profit_factor = if gross_loss > Decimal::ZERO {
        Some(gross_win.checked_div(gross_loss).ok_or_else(overflow)?)
    } else {
        None
    };

    let (mut cumulative, mut high, mut max_drawdown_usd) =
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO);
    for pnl in pnls() {
        cumulative = checked_sum([cumulative, pnl].into_iter())?;
        high = high.max(cumulative);
        let drop = high.checked_sub(cumulative).ok_or_else(overflow)?;
        max_drawdown_usd = max_drawdown_usd.max(drop);
    }
    Ok(BacktestReport {
        trades,
        net_usd,
        expectancy_usd,
        win_rate_bps,
        max_drawdown_usd,
        profit_factor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{seeds, Condition, Op};
    use crate::filter::tests::rules;
    use crate::filter::Loosening;
    use crate::signals::SignalId;
    use crate::testutil::{costs, d, t0, Rng};
    use time::Duration;

    /// A simple config: enter when the 1-hour return is above +10%.
    fn config() -> StrategyConfig {
        let mut c = seeds()[0].clone();
        c.entry = vec![Condition {
            signal: SignalId::Return1h,
            op: Op::Gt,
            a: 1_000,
            b: None,
        }];
        c.take_profit_bps = 5_000;
        c.stop_loss_bps = 2_000;
        c.trailing_stop_bps = None;
        c.token_cooldown_secs = 3_600;
        c
    }

    /// 10-minute steps; liquidity fixed at 200k.
    fn series(prices: &[f64]) -> Series {
        Series {
            pool_created_at: t0() - Duration::hours(2),
            points: prices
                .iter()
                .enumerate()
                .map(|(i, p)| Observation {
                    at: t0() + Duration::minutes(10 * i as i64),
                    price_usd: Decimal::try_from(*p).unwrap(),
                    liquidity_usd: d("200000"),
                })
                .collect(),
        }
    }

    fn run(c: &StrategyConfig, tokens: &[(&str, Series)]) -> BacktestReport {
        let map = tokens
            .iter()
            .map(|(m, s)| ((*m).to_owned(), s.clone()))
            .collect();
        backtest(c, &rules(), &map, &costs(), d("15")).unwrap()
    }

    fn reasons(r: &BacktestReport) -> Vec<Option<ExitReason>> {
        r.trades.iter().map(|t| t.reason).collect()
    }

    #[test]
    fn take_profit_stop_loss_and_end_of_data() {
        // Flat hour, +20% (signal), fill, then +58% (take profit).
        let up = series(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.9, 1.9]);
        // Signal, fill, then -33% (stop loss).
        let down = series(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 0.8, 0.8]);
        // Signal, fill, then the data ends.
        let ends = series(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.25]);
        let r = run(&config(), &[("up", up), ("down", down), ("ends", ends)]);
        assert_eq!(
            reasons(&r),
            vec![
                Some(ExitReason::StopLoss),
                None,
                Some(ExitReason::TakeProfit)
            ]
        );
        assert!(r.trades[0].net_pnl_usd < d("-3"));
        assert!(r.trades[2].net_pnl_usd > d("5"));
        assert_eq!(r.win_rate_bps, 6_666);
        assert_eq!(
            r.net_usd,
            r.trades.iter().map(|t| t.net_pnl_usd).sum::<Decimal>()
        );
        assert_eq!(r.expectancy_usd, r.net_usd / d("3"));
        assert!(r.profit_factor.unwrap() > Decimal::ONE);
        assert_eq!(r.max_drawdown_usd, -r.trades[0].net_pnl_usd);
        // Filled one observation after the signal.
        assert!(r
            .trades
            .iter()
            .all(|t| t.opened_at == t0() + Duration::minutes(70)));
        // Entry size: the full 15 USD mandate share plus fees.
        assert!(r
            .trades
            .iter()
            .all(|t| t.cost_usd > d("15") && t.cost_usd < d("15.1")));
    }

    #[test]
    fn trailing_stop_locks_in_gains() {
        let prices = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.4, 1.6, 1.25, 1.25];
        let mut c = config();
        c.take_profit_bps = 100_000;
        let without = run(&c, &[("t", series(&prices))]);
        assert_eq!(reasons(&without), vec![None]);
        c.trailing_stop_bps = Some(2_000);
        let with = run(&c, &[("t", series(&prices))]);
        assert_eq!(reasons(&with), vec![Some(ExitReason::TrailingStop)]);
        assert_eq!(with.trades[0].closed_at, t0() + Duration::minutes(100));
        assert!(with.trades[0].net_pnl_usd > Decimal::ZERO);
    }

    #[test]
    fn entries_per_day_and_cooldown_hold_across_tokens() {
        let prices = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.9, 1.9];
        let tokens: Vec<(String, Series)> =
            (0..6).map(|i| (format!("m{i}"), series(&prices))).collect();
        let tokens: Vec<(&str, Series)> = tokens
            .iter()
            .map(|(m, s)| (m.as_str(), s.clone()))
            .collect();
        let mut c = config();
        c.max_entries_per_day = 4;
        assert_eq!(run(&c, &tokens).trades.len(), 4);

        // One token pumping twice within the cooldown is entered once.
        c.token_cooldown_secs = 7_200;
        let twice = series(&[
            1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.9, 1.9, 1.9, 1.9, 1.9, 2.5, 2.5,
        ]);
        assert_eq!(run(&c, &[("x", twice.clone())]).trades.len(), 1);
        c.token_cooldown_secs = 60;
        assert_eq!(run(&c, &[("x", twice)]).trades.len(), 2);
    }

    #[test]
    fn entries_fill_at_the_next_observation() {
        // The signal fires on the second-to-last point; its fill would be the
        // last point, which could never be exited, so nothing trades.
        let late = series(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.3]);
        assert!(run(&config(), &[("late", late)]).trades.is_empty());
    }

    #[test]
    fn filters_the_series_can_evaluate_apply() {
        let prices = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.2, 1.2, 1.9, 1.9];
        let tokens = [("m", series(&prices))];
        assert_eq!(run(&config(), &tokens).trades.len(), 1);

        let mut c = config();
        c.filters.min_liquidity_usd = Some(d("250000"));
        assert!(run(&c, &tokens).trades.is_empty());

        // The signal comes 3 hours after pool creation.
        let mut c = config();
        c.filters.min_pool_age_secs = Some(4 * 3_600);
        assert!(run(&c, &tokens).trades.is_empty());
        c.filters.min_pool_age_secs = Some(3 * 3_600);
        assert_eq!(run(&c, &tokens).trades.len(), 1);

        // A looser override is refused before anything runs.
        let mut c = config();
        c.filters.min_liquidity_usd = Some(d("1"));
        let map = BTreeMap::from([("m".to_owned(), series(&prices))]);
        assert_eq!(
            backtest(&c, &rules(), &map, &costs(), d("15")),
            Err(BacktestError::Config(ConfigError::Loosening(Loosening {
                field: "min_liquidity_usd"
            })))
        );
    }

    #[test]
    fn absurd_results_are_an_error_not_a_panic() {
        let trade = |pnl: Decimal| BacktestTrade {
            mint: "m".to_owned(),
            opened_at: t0(),
            closed_at: t0(),
            reason: None,
            cost_usd: Decimal::ONE,
            net_pnl_usd: pnl,
        };
        let overflow = Err(BacktestError::Ledger(LedgerError::Overflow));
        let tiny = Decimal::new(1, 28);
        assert_eq!(report(vec![trade(Decimal::MAX), trade(-tiny)]), overflow);
        let swing = vec![
            trade(Decimal::MAX),
            trade(-Decimal::MAX),
            trade(-Decimal::MAX),
        ];
        assert_eq!(report(swing), overflow);
    }

    #[test]
    fn deterministic_on_a_fixed_synthetic_series() {
        let mut rng = Rng::new(42);
        let tokens: Vec<(String, Series)> = (0..5)
            .map(|t| {
                let mut price = 1.0f64;
                let prices: Vec<f64> = (0..300)
                    .map(|_| {
                        price *= 0.9 + rng.below(25) as f64 / 100.0;
                        (price * 1e6).round() / 1e6 + 1e-6
                    })
                    .collect();
                (format!("mint{t}"), series(&prices))
            })
            .collect();
        let map: BTreeMap<String, Series> = tokens.into_iter().collect();
        let mut c = config();
        c.trailing_stop_bps = Some(1_500);
        let first = backtest(&c, &rules(), &map, &costs(), d("15")).unwrap();
        assert!(!first.trades.is_empty());
        for _ in 0..3 {
            assert_eq!(
                backtest(&c, &rules(), &map, &costs(), d("15")).unwrap(),
                first
            );
        }
        // Closes are reported in time order.
        assert!(first
            .trades
            .windows(2)
            .all(|w| w[0].closed_at <= w[1].closed_at));
    }

    #[test]
    fn refuses_invalid_input() {
        let ok = series(&[1.0, 1.0]);
        let mut bad = config();
        bad.stop_loss_bps = 0;
        let map = BTreeMap::from([("m".to_owned(), ok.clone())]);
        assert!(matches!(
            backtest(&bad, &rules(), &map, &costs(), d("15")),
            Err(BacktestError::Config(_))
        ));
        let broken = CostModel {
            failure_rate: d("1.5"),
            ..costs()
        };
        assert_eq!(
            backtest(&config(), &rules(), &map, &broken, d("15")),
            Err(BacktestError::InvalidCosts)
        );
        let mut unordered = ok.clone();
        unordered.points.reverse();
        let mut zero = ok;
        zero.points[1].price_usd = Decimal::ZERO;
        for s in [unordered, zero] {
            let map = BTreeMap::from([("m".to_owned(), s)]);
            assert_eq!(
                backtest(&config(), &rules(), &map, &costs(), d("15")),
                Err(BacktestError::InvalidSeries("m".to_owned()))
            );
        }
    }
}
