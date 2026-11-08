//! Entry signals computed from our own observation series.
//!
//! A [`SignalId`] names one feature that code computes from price and
//! liquidity we observed ourselves. Strategy configs can only pick from this
//! enum and compare a value against bounds; there are no expressions or
//! formulas. Buy and sell counts are deliberately absent: they are the
//! cheapest numbers to fake.
//!
//! Every signal at time `now` only reads observations at or before `now`, so
//! a backtest cannot look ahead. A windowed signal needs an observation at or
//! before the start of its window; without full coverage, or with data that
//! cannot be computed with, the signal is `None` and no condition on it holds.
//!
//! Values are whole numbers: basis points for ratios (10 000 = 100%) and
//! minutes for ages.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// One observation of a token's main pool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    pub at: OffsetDateTime,
    pub price_usd: Decimal,
    pub liquidity_usd: Decimal,
}

/// Our observations of one token, oldest first.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub pool_created_at: OffsetDateTime,
    pub points: Vec<Observation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalId {
    /// Price change over the window, bps.
    Return15m,
    Return1h,
    Return6h,
    Return24h,
    /// Price above (positive) or below its simple moving average, bps.
    PriceVsSma30m,
    /// Against the average of the last 20 daily closes; each 24-hour
    /// bucket back from now must hold an observation.
    PriceVsSma20d,
    /// Price above (positive) or below the highest price of the window
    /// before the current observation, bps. Positive means a new high.
    BreakoutHigh1h,
    BreakoutHigh2h,
    BreakoutHigh10d,
    /// Width of the price range of the window before the current
    /// observation, `(high - low) / low`, bps.
    RangeWidth2h,
    /// How far price is below the highest price observed so far, bps (>= 0).
    DrawdownFromHigh,
    /// Liquidity change over the window, bps.
    LiquidityChange1h,
    LiquidityChange2h,
    /// How far liquidity is below its highest observed level, bps (>= 0).
    LiquidityDrawdownFromPeak,
    /// Minutes since the pool was created.
    PoolAgeMinutes,
}

impl SignalId {
    /// The range a condition bound on this signal may use.
    pub fn range(self) -> (i64, i64) {
        use SignalId::*;
        match self {
            RangeWidth2h => (0, 1_000_000),
            DrawdownFromHigh | LiquidityDrawdownFromPeak => (0, 10_000),
            PoolAgeMinutes => (0, 525_600),
            _ => (-10_000, 1_000_000),
        }
    }
}

/// The value of `id` at `now`, from observations at or before `now`.
pub fn compute(id: SignalId, series: &Series, now: OffsetDateTime) -> Option<i64> {
    use SignalId::*;
    let points = &series.points[..series.points.partition_point(|p| p.at <= now)];
    let current = points.last()?;
    let price = |p: &Observation| p.price_usd;
    let liquidity = |p: &Observation| p.liquidity_usd;
    match id {
        Return15m => change(points, Duration::minutes(15), price),
        Return1h => change(points, Duration::hours(1), price),
        Return6h => change(points, Duration::hours(6), price),
        Return24h => change(points, Duration::hours(24), price),
        PriceVsSma30m => {
            let window = covered(points, Duration::minutes(30))?;
            let sum = window
                .iter()
                .try_fold(Decimal::ZERO, |acc, p| acc.checked_add(p.price_usd))?;
            bps(current.price_usd, sum / Decimal::from(window.len()))
        }
        PriceVsSma20d => bps(current.price_usd, daily_close_average(points, 20)?),
        BreakoutHigh1h => breakout(points, Duration::hours(1)),
        BreakoutHigh2h => breakout(points, Duration::hours(2)),
        BreakoutHigh10d => breakout(points, Duration::days(10)),
        RangeWidth2h => {
            let prior = prior_window(points, Duration::hours(2))?;
            let high = prior.iter().map(price).max()?;
            let low = prior.iter().map(price).min()?;
            bps(high, low)
        }
        DrawdownFromHigh => drawdown(points, price),
        LiquidityChange1h => change(points, Duration::hours(1), liquidity),
        LiquidityChange2h => change(points, Duration::hours(2), liquidity),
        LiquidityDrawdownFromPeak => drawdown(points, liquidity),
        PoolAgeMinutes => {
            let age = current.at - series.pool_created_at;
            (age >= Duration::ZERO).then(|| age.whole_minutes())
        }
    }
}

/// `(value - base) / base` in bps, rounded toward zero. `None` for a base of
/// zero or less, or a result too large for `i64`.
fn bps(value: Decimal, base: Decimal) -> Option<i64> {
    if base <= Decimal::ZERO {
        return None;
    }
    value
        .checked_sub(base)?
        .checked_mul(BPS)?
        .checked_div(base)?
        .trunc()
        .to_i64()
}

/// The points from the last one at or before `now - window` up to now, or
/// `None` when the series does not reach back that far.
fn covered(points: &[Observation], window: Duration) -> Option<&[Observation]> {
    let start = points.last()?.at.checked_sub(window)?;
    let first = points.partition_point(|p| p.at <= start).checked_sub(1)?;
    Some(&points[first..])
}

/// The covered window without the current observation.
fn prior_window(points: &[Observation], window: Duration) -> Option<&[Observation]> {
    let window = covered(points, window)?;
    Some(&window[..window.len() - 1])
}

fn change(
    points: &[Observation],
    window: Duration,
    field: impl Fn(&Observation) -> Decimal,
) -> Option<i64> {
    let window = covered(points, window)?;
    bps(field(window.last()?), field(window.first()?))
}

fn breakout(points: &[Observation], window: Duration) -> Option<i64> {
    let high = prior_window(points, window)?
        .iter()
        .map(|p| p.price_usd)
        .max()?;
    bps(points.last()?.price_usd, high)
}

// Scans the whole history on every call; keep running maxima in the backtest
// if long series make it slow.
fn drawdown(points: &[Observation], field: impl Fn(&Observation) -> Decimal) -> Option<i64> {
    let high = points.iter().map(&field).max()?;
    bps(field(points.last()?), high).map(|v| -v)
}

/// Average of the last observation in each of `days` consecutive 24-hour
/// buckets ending now. `None` if any bucket is empty.
fn daily_close_average(points: &[Observation], days: i64) -> Option<Decimal> {
    let now = points.last()?.at;
    let mut sum = Decimal::ZERO;
    for day in 0..days {
        let end = now.checked_sub(Duration::days(day))?;
        let start = end.checked_sub(Duration::days(1))?;
        let close = points[..points.partition_point(|p| p.at <= end)].last()?;
        if close.at <= start {
            return None;
        }
        sum = sum.checked_add(close.price_usd)?;
    }
    Some(sum / Decimal::from(days))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::{d, t0};
    use SignalId::*;

    /// One observation per `step` minutes, starting at `t0()`.
    pub fn series(step: i64, prices: &[&str], liquidity: &[&str]) -> Series {
        Series {
            pool_created_at: t0() - Duration::hours(1),
            points: prices
                .iter()
                .zip(liquidity)
                .enumerate()
                .map(|(i, (p, l))| Observation {
                    at: t0() + Duration::minutes(step * i as i64),
                    price_usd: d(p),
                    liquidity_usd: d(l),
                })
                .collect(),
        }
    }

    fn at(minutes: i64) -> OffsetDateTime {
        t0() + Duration::minutes(minutes)
    }

    #[test]
    fn signals_on_a_fixed_series() {
        // 15-minute steps over 2 hours: a range, then a breakout.
        let s = series(
            15,
            &[
                "1.0", "1.2", "0.9", "1.1", "1.0", "1.1", "1.0", "1.05", "1.5",
            ],
            &[
                "100", "100", "110", "120", "120", "130", "140", "150", "150",
            ],
        );
        let now = at(120);
        let v = |id| compute(id, &s, now);
        assert_eq!(v(Return15m), Some(4_285)); // 1.05 -> 1.5
        assert_eq!(v(Return1h), Some(5_000)); // 1.0 -> 1.5
        assert_eq!(v(BreakoutHigh1h), Some(3_636)); // over 1.1
        assert_eq!(v(BreakoutHigh2h), Some(2_500)); // over 1.2
        assert_eq!(v(RangeWidth2h), Some(3_333)); // 0.9 .. 1.2
        assert_eq!(v(DrawdownFromHigh), Some(0));
        assert_eq!(v(LiquidityChange1h), Some(2_500)); // 120 -> 150
        assert_eq!(v(LiquidityChange2h), Some(5_000));
        assert_eq!(v(LiquidityDrawdownFromPeak), Some(0));
        assert_eq!(v(PoolAgeMinutes), Some(180));
        // SMA from the point at the window start: 1.0, 1.05, 1.5.
        assert_eq!(v(PriceVsSma30m), Some(2_676));

        // Not enough history for longer windows: no value, not zero.
        assert_eq!(v(Return6h), None);
        assert_eq!(v(BreakoutHigh10d), None);
        assert_eq!(v(PriceVsSma20d), None);

        // Earlier `now` ignores later points: no look-ahead.
        assert_eq!(compute(Return1h, &s, at(105)), Some(-454)); // 1.1 -> 1.05
        assert_eq!(compute(DrawdownFromHigh, &s, at(105)), Some(1_250));
        assert_eq!(compute(Return15m, &s, t0() - Duration::minutes(1)), None);
    }

    #[test]
    fn daily_average_needs_every_day() {
        let prices: Vec<String> = (1..=21).map(|i| i.to_string()).collect();
        let prices: Vec<&str> = prices.iter().map(String::as_str).collect();
        let s = series(24 * 60, &prices, &["1000"; 21]);
        // Closes 2..=21 average 11.5; price 21 is 8 260 bps above it.
        assert_eq!(compute(PriceVsSma20d, &s, at(20 * 24 * 60)), Some(8_260));
        assert_eq!(compute(BreakoutHigh10d, &s, at(20 * 24 * 60)), Some(500));

        let mut gap = s.clone();
        gap.points.remove(10);
        assert_eq!(compute(PriceVsSma20d, &gap, at(20 * 24 * 60)), None);
    }

    #[test]
    fn bad_data_gives_no_value() {
        let s = series(30, &["0", "1"], &["0", "10"]);
        assert_eq!(compute(Return15m, &s, at(30)), None);
        let dead = series(30, &["0", "0"], &["1", "1"]);
        assert_eq!(compute(DrawdownFromHigh, &dead, at(30)), None);
        let mut young = s.clone();
        young.pool_created_at = at(31);
        assert_eq!(compute(PoolAgeMinutes, &young, at(30)), None);
        assert_eq!(
            compute(
                Return15m,
                &Series {
                    points: vec![],
                    ..s
                },
                at(30)
            ),
            None
        );
    }
}
