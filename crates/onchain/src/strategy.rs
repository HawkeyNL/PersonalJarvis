//! Strategy engine: versioned parameters in, trade intents out.
//!
//! The strategy is plain deterministic code: the same snapshot, parameters and
//! time always give the same intent. Learning may propose new
//! [`StrategyParams`], but every field has a hard minimum and maximum fixed in
//! this file, and [`StrategyParams::validate`] refuses anything outside them.
//!
//! An intent is a *request*, not permission. The entry size is a share of the
//! mandate's maximum position value (passed in, never stored here) and is
//! further capped by pool liquidity. Downstream the risk check and signer
//! still apply the mandate's own limits; nothing here can raise them.
//!
//! Entries are only considered for snapshots that pass [`crate::filter`];
//! this module runs the filter itself so it cannot be skipped. Exits skip the
//! filter: getting out of a held position must stay possible.

use std::collections::BTreeSet;

use rust_decimal::{Decimal, RoundingStrategy};
use time::{Duration, OffsetDateTime};

use crate::filter::{self, FilterRules, Verdict};
use crate::paper::{self, CostModel};
use crate::types::{Position, TokenSnapshot};

const BPS: Decimal = Decimal::from_parts(10_000, 0, 0, false, 0);

/// One version of the strategy's tunable parameters. Units are basis points
/// unless the name says otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyParams {
    /// Strictly increasing per promoted parameter set; recorded on intents.
    pub version: u32,
    /// Minimum share of buys among 5-minute trades.
    pub min_buy_ratio_bps: u32,
    /// The 5-minute price change must fall inside this band: some momentum,
    /// but not chasing a vertical pump.
    pub min_momentum_bps: i32,
    pub max_momentum_bps: i32,
    /// Entry size as a share of the mandate's maximum position value.
    pub position_share_bps: u32,
    /// Entry size cap as a share of pool liquidity, to bound price impact.
    pub max_liquidity_share_bps: u32,
    pub take_profit_bps: u32,
    pub stop_loss_bps: u32,
    /// Exit when pool liquidity falls this far below its level at entry.
    pub liquidity_drop_exit_bps: u32,
    pub max_hold: Duration,
    pub entry_slippage_bps: u32,
    pub exit_slippage_bps: u32,
    /// How long an intent stays valid after it is emitted.
    pub intent_ttl: Duration,
}

/// Code-fixed bounds. Changing these is a code change with review, never a
/// learning step.
pub mod bounds {
    use time::Duration;

    pub const BUY_RATIO_BPS: (u32, u32) = (5_000, 10_000);
    pub const MOMENTUM_BPS: (i32, i32) = (-5_000, 50_000);
    pub const POSITION_SHARE_BPS: (u32, u32) = (1, 10_000);
    pub const LIQUIDITY_SHARE_BPS: (u32, u32) = (1, 200);
    pub const TAKE_PROFIT_BPS: (u32, u32) = (100, 100_000);
    pub const STOP_LOSS_BPS: (u32, u32) = (100, 9_000);
    pub const LIQUIDITY_DROP_BPS: (u32, u32) = (100, 9_000);
    pub const MAX_HOLD: (Duration, Duration) = (Duration::minutes(1), Duration::days(7));
    pub const ENTRY_SLIPPAGE_BPS: (u32, u32) = (1, 1_000);
    pub const EXIT_SLIPPAGE_BPS: (u32, u32) = (1, 5_000);
    pub const INTENT_TTL: (Duration, Duration) = (Duration::seconds(1), Duration::seconds(120));
}

/// A parameter outside its code-fixed bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("strategy parameter `{field}` is outside its bounds")]
pub struct OutOfBounds {
    pub field: &'static str,
}

impl StrategyParams {
    pub fn validate(&self) -> Result<(), OutOfBounds> {
        use bounds::*;
        within("version", self.version, (1, u32::MAX))?;
        within("min_buy_ratio_bps", self.min_buy_ratio_bps, BUY_RATIO_BPS)?;
        within("min_momentum_bps", self.min_momentum_bps, MOMENTUM_BPS)?;
        within(
            "max_momentum_bps",
            self.max_momentum_bps,
            (self.min_momentum_bps, MOMENTUM_BPS.1),
        )?;
        within(
            "position_share_bps",
            self.position_share_bps,
            POSITION_SHARE_BPS,
        )?;
        within(
            "max_liquidity_share_bps",
            self.max_liquidity_share_bps,
            LIQUIDITY_SHARE_BPS,
        )?;
        within("take_profit_bps", self.take_profit_bps, TAKE_PROFIT_BPS)?;
        within("stop_loss_bps", self.stop_loss_bps, STOP_LOSS_BPS)?;
        within(
            "liquidity_drop_exit_bps",
            self.liquidity_drop_exit_bps,
            LIQUIDITY_DROP_BPS,
        )?;
        within("max_hold", self.max_hold, MAX_HOLD)?;
        within(
            "entry_slippage_bps",
            self.entry_slippage_bps,
            ENTRY_SLIPPAGE_BPS,
        )?;
        within(
            "exit_slippage_bps",
            self.exit_slippage_bps,
            EXIT_SLIPPAGE_BPS,
        )?;
        within("intent_ttl", self.intent_ttl, INTENT_TTL)
    }
}

fn within<T: PartialOrd>(
    field: &'static str,
    value: T,
    (min, max): (T, T),
) -> Result<(), OutOfBounds> {
    if value >= min && value <= max {
        Ok(())
    } else {
        Err(OutOfBounds { field })
    }
}

/// A request to trade. The intent id, the quote mint and `min_amount_out` are
/// assigned downstream; the signer computes the minimum output from its own
/// quote, never from this struct.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeIntent {
    pub strategy_version: u32,
    pub mint: String,
    pub kind: IntentKind,
    /// Entry: USD to spend. Exit: tokens to sell.
    pub amount_in: Decimal,
    /// Requested slippage; the mandate's cap still applies downstream.
    pub max_slippage_bps: u32,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    Entry,
    Exit(ExitReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    LiquidityDrop,
    StopLoss,
    TakeProfit,
    TimeStop,
}

/// Decide whether to enter `s`. Returns `None` for invalid parameters, a
/// filter reject, no signal, or a size that rounds to nothing.
/// `max_position_usd` is the mandate's per-position limit.
pub fn evaluate_entry(
    s: &TokenSnapshot,
    params: &StrategyParams,
    rules: &FilterRules,
    deny_list: &BTreeSet<String>,
    max_position_usd: Decimal,
    now: OffsetDateTime,
) -> Option<TradeIntent> {
    params.validate().ok()?;
    if filter::check(s, rules, deny_list, now) != Verdict::Accept {
        return None;
    }

    let trades = u64::from(s.buys_5m) + u64::from(s.sells_5m);
    let buy_ratio_ok =
        trades > 0 && u64::from(s.buys_5m) * 10_000 >= u64::from(params.min_buy_ratio_bps) * trades;
    let momentum_ok =
        (params.min_momentum_bps..=params.max_momentum_bps).contains(&s.price_change_5m_bps);
    if !(buy_ratio_ok && momentum_ok) {
        return None;
    }

    let by_mandate = max_position_usd.checked_mul(Decimal::from(params.position_share_bps))? / BPS;
    let by_liquidity = s
        .liquidity_usd
        .checked_mul(Decimal::from(params.max_liquidity_share_bps))?
        / BPS;
    // Round down: rounding up could exceed the mandate limit by a cent.
    let size = by_mandate
        .min(by_liquidity)
        .round_dp_with_strategy(2, RoundingStrategy::ToZero);
    if size <= Decimal::ZERO {
        return None;
    }

    Some(TradeIntent {
        strategy_version: params.version,
        mint: s.mint.clone(),
        kind: IntentKind::Entry,
        amount_in: size,
        max_slippage_bps: params.entry_slippage_bps,
        created_at: now,
        expires_at: now + params.intent_ttl,
    })
}

/// Decide whether to exit `position` given the latest snapshot of its pool.
/// Checks run safety-first: liquidity drop, stop loss, take profit, time
/// stop. Price-based exits value the whole position at what selling it would
/// return along the pool curve, not at the mid price.
///
/// Only a fresh snapshot (within `max_snapshot_age`) of the position's own
/// mint may drive a price-based exit. Otherwise, and for data too absurd to
/// compute with, only the time stop applies.
///
/// Exits must never be blocked by a bad parameter set, so instead of
/// refusing invalid params this clamps the exit slippage and intent lifetime
/// into their code-fixed bounds.
pub fn evaluate_exit(
    position: &Position,
    s: &TokenSnapshot,
    params: &StrategyParams,
    costs: &CostModel,
    max_snapshot_age: Duration,
    now: OffsetDateTime,
) -> Option<TradeIntent> {
    let usable =
        s.mint == position.mint && s.observed_at <= now && now - s.observed_at <= max_snapshot_age;
    let reason = usable
        .then(|| market_exit(position, s, params, costs))
        .flatten()
        .or_else(|| {
            (now - position.opened_at >= params.max_hold).then_some(ExitReason::TimeStop)
        })?;
    Some(TradeIntent {
        strategy_version: params.version,
        mint: position.mint.clone(),
        kind: IntentKind::Exit(reason),
        amount_in: position.tokens,
        max_slippage_bps: params
            .exit_slippage_bps
            .clamp(bounds::EXIT_SLIPPAGE_BPS.0, bounds::EXIT_SLIPPAGE_BPS.1),
        created_at: now,
        expires_at: now
            + params
                .intent_ttl
                .clamp(bounds::INTENT_TTL.0, bounds::INTENT_TTL.1),
    })
}

/// Liquidity drop, stop loss or take profit; `None` when none fires or the
/// numbers overflow. A fresh price of zero or less, or an invalid cost
/// model, is a stop loss: the position can no longer be valued.
fn market_exit(
    position: &Position,
    s: &TokenSnapshot,
    params: &StrategyParams,
    costs: &CostModel,
) -> Option<ExitReason> {
    let keep = |bps: u32| BPS - Decimal::from(bps.min(10_000));
    let floor = position
        .entry_liquidity_usd
        .checked_mul(keep(params.liquidity_drop_exit_bps))?;
    if s.liquidity_usd.checked_mul(BPS)? <= floor {
        return Some(ExitReason::LiquidityDrop);
    }
    // A position that cannot be valued (dead price, or a broken cost model
    // that would otherwise silently block every price exit) is cut.
    if s.price_usd <= Decimal::ZERO || !costs.is_valid() {
        return Some(ExitReason::StopLoss);
    }
    let value = paper::sell_value(position.tokens, s.price_usd, s.liquidity_usd, costs)?
        .checked_mul(BPS)?;
    if value <= position.cost_usd.checked_mul(keep(params.stop_loss_bps))? {
        return Some(ExitReason::StopLoss);
    }
    let target = position
        .cost_usd
        .checked_mul(BPS + Decimal::from(params.take_profit_bps))?;
    (value >= target).then_some(ExitReason::TakeProfit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::rules;
    use crate::testutil::{costs, d, good_snapshot, t0, Rng};

    fn params() -> StrategyParams {
        StrategyParams {
            version: 3,
            min_buy_ratio_bps: 6_000,
            min_momentum_bps: 500,
            max_momentum_bps: 5_000,
            position_share_bps: 10_000,
            max_liquidity_share_bps: 100,
            take_profit_bps: 5_000,
            stop_loss_bps: 2_500,
            liquidity_drop_exit_bps: 4_000,
            max_hold: Duration::hours(4),
            entry_slippage_bps: 150,
            exit_slippage_bps: 800,
            intent_ttl: Duration::seconds(20),
        }
    }

    fn entry(s: &TokenSnapshot, max_position: &str) -> Option<TradeIntent> {
        evaluate_entry(
            s,
            &params(),
            &rules(),
            &BTreeSet::new(),
            d(max_position),
            t0(),
        )
    }

    #[test]
    fn entry_is_deterministic_and_sized_by_mandate() {
        let intent = entry(&good_snapshot(), "40").expect("entry");
        assert_eq!(intent, entry(&good_snapshot(), "40").unwrap());
        assert_eq!(intent.kind, IntentKind::Entry);
        assert_eq!(intent.amount_in, d("40"));
        assert_eq!(intent.strategy_version, 3);
        assert_eq!(intent.expires_at, t0() + Duration::seconds(20));
    }

    #[test]
    fn entry_size_never_exceeds_mandate_or_liquidity_share() {
        let mut rng = Rng::new(5);
        for _ in 0..500 {
            let mut s = good_snapshot();
            s.liquidity_usd = rng.decimal(1_000_000, 2) + d("50000");
            let max_position = rng.decimal(5_000, 2);
            let mut p = params();
            p.position_share_bps = rng.below(10_000) as u32 + 1;
            if let Some(intent) =
                evaluate_entry(&s, &p, &rules(), &BTreeSet::new(), max_position, t0())
            {
                assert!(intent.amount_in <= max_position);
                assert!(intent.amount_in <= s.liquidity_usd / d("100"));
                assert!(intent.amount_in > Decimal::ZERO);
            }
        }
    }

    #[test]
    fn no_entry_without_filter_pass_signal_or_valid_params() {
        let mut rejected = good_snapshot();
        rejected.mint_authority_revoked = false;
        assert_eq!(entry(&rejected, "40"), None);

        let mut weak = good_snapshot();
        weak.buys_5m = 50;
        weak.sells_5m = 50;
        assert_eq!(entry(&weak, "40"), None);

        let mut pump = good_snapshot();
        pump.price_change_5m_bps = 5_001;
        assert_eq!(entry(&pump, "40"), None);

        let mut silent = good_snapshot();
        silent.buys_5m = 0;
        silent.sells_5m = 0;
        assert_eq!(entry(&silent, "40"), None);

        assert_eq!(entry(&good_snapshot(), "0"), None);

        let mut bad = params();
        bad.stop_loss_bps = 0;
        assert_eq!(
            evaluate_entry(
                &good_snapshot(),
                &bad,
                &rules(),
                &BTreeSet::new(),
                d("40"),
                t0()
            ),
            None
        );
    }

    #[test]
    fn promotion_fields_never_change_the_intent() {
        let mut rng = Rng::new(9);
        for _ in 0..300 {
            let mut base = good_snapshot();
            base.buys_5m = rng.below(100) as u32;
            base.sells_5m = rng.below(100) as u32;
            base.price_change_5m_bps = rng.below(8_000) as i32 - 1_000;
            let mut promoted = base.clone();
            promoted.boosts = rng.next() as u32;
            promoted.trending_rank = Some(1);
            assert_eq!(entry(&base, "40"), entry(&promoted, "40"));
        }
    }

    #[test]
    fn validate_enforces_every_bound() {
        assert_eq!(params().validate(), Ok(()));
        type Mutation = fn(&mut StrategyParams);
        let cases: &[(Mutation, &str)] = &[
            (|p| p.version = 0, "version"),
            (|p| p.min_buy_ratio_bps = 4_999, "min_buy_ratio_bps"),
            (|p| p.min_buy_ratio_bps = 10_001, "min_buy_ratio_bps"),
            (|p| p.min_momentum_bps = -5_001, "min_momentum_bps"),
            (|p| p.max_momentum_bps = 50_001, "max_momentum_bps"),
            (
                |p| p.max_momentum_bps = p.min_momentum_bps - 1,
                "max_momentum_bps",
            ),
            (|p| p.position_share_bps = 0, "position_share_bps"),
            (|p| p.position_share_bps = 10_001, "position_share_bps"),
            (
                |p| p.max_liquidity_share_bps = 201,
                "max_liquidity_share_bps",
            ),
            (|p| p.take_profit_bps = 99, "take_profit_bps"),
            (|p| p.stop_loss_bps = 9_001, "stop_loss_bps"),
            (
                |p| p.liquidity_drop_exit_bps = 99,
                "liquidity_drop_exit_bps",
            ),
            (|p| p.max_hold = Duration::days(8), "max_hold"),
            (|p| p.entry_slippage_bps = 1_001, "entry_slippage_bps"),
            (|p| p.exit_slippage_bps = 0, "exit_slippage_bps"),
            (|p| p.intent_ttl = Duration::seconds(121), "intent_ttl"),
        ];
        for (mutate, field) in cases {
            let mut p = params();
            mutate(&mut p);
            assert_eq!(p.validate(), Err(OutOfBounds { field }));
        }
        let mut edges = params();
        edges.min_buy_ratio_bps = bounds::BUY_RATIO_BPS.0;
        edges.max_hold = bounds::MAX_HOLD.1;
        edges.intent_ttl = bounds::INTENT_TTL.0;
        assert_eq!(edges.validate(), Ok(()));
    }

    fn position(cost: &str) -> Position {
        Position {
            mint: "MintGood111".to_owned(),
            tokens: d("40000"),
            cost_usd: d(cost),
            opened_at: t0(),
            entry_liquidity_usd: d("200000"),
        }
    }

    fn exit(p: &Position, s: &TokenSnapshot, now: OffsetDateTime) -> Option<IntentKind> {
        evaluate_exit(p, s, &params(), &costs(), Duration::seconds(30), now).map(|i| i.kind)
    }

    #[test]
    fn exits_fire_safety_first() {
        let p = position("40");
        let s = good_snapshot(); // 40000 tokens at 0.001 ~ 39.88 USD executable
        assert_eq!(exit(&p, &s, t0()), None);

        let mut drained = s.clone();
        drained.liquidity_usd = d("120000");
        drained.price_usd = d("0.01"); // even a "profit" exits on a liquidity drop
        assert_eq!(
            exit(&p, &drained, t0()),
            Some(IntentKind::Exit(ExitReason::LiquidityDrop))
        );

        let mut down = s.clone();
        down.price_usd = d("0.0007");
        assert_eq!(
            exit(&p, &down, t0()),
            Some(IntentKind::Exit(ExitReason::StopLoss))
        );

        let mut up = s.clone();
        up.price_usd = d("0.0017");
        assert_eq!(
            exit(&p, &up, t0()),
            Some(IntentKind::Exit(ExitReason::TakeProfit))
        );

        assert_eq!(
            exit(&p, &s, t0() + Duration::hours(4)),
            Some(IntentKind::Exit(ExitReason::TimeStop))
        );

        let intent =
            evaluate_exit(&p, &up, &params(), &costs(), Duration::seconds(30), t0()).unwrap();
        assert_eq!(intent.amount_in, d("40000"));
        assert_eq!(intent.max_slippage_bps, 800);
    }

    #[test]
    fn take_profit_uses_executable_value_not_mid_price() {
        // Mid value is exactly +50%, but selling moves the price: no exit yet.
        let p = position("40");
        let mut s = good_snapshot();
        s.price_usd = d("0.0015");
        assert_eq!(exit(&p, &s, t0()), None);
    }

    #[test]
    fn wrong_stale_or_absurd_snapshots_only_allow_the_time_stop() {
        let p = position("40");
        let mut crash = good_snapshot();
        crash.price_usd = d("0.0001");

        let mut other = crash.clone();
        other.mint = "OtherMint".to_owned();
        let mut stale = crash.clone();
        stale.observed_at = t0() - Duration::seconds(31);
        let mut future = crash.clone();
        future.observed_at = t0() + Duration::seconds(1);
        let mut absurd = good_snapshot();
        absurd.liquidity_usd = Decimal::MAX;
        absurd.price_usd = d("0.0000001");

        assert_eq!(
            exit(&p, &crash, t0()),
            Some(IntentKind::Exit(ExitReason::StopLoss))
        );
        for s in [&other, &stale, &future, &absurd] {
            assert_eq!(exit(&p, s, t0()), None);
            assert_eq!(
                exit(&p, s, t0() + Duration::hours(4)),
                Some(IntentKind::Exit(ExitReason::TimeStop))
            );
        }
    }

    #[test]
    fn fresh_zero_price_is_a_stop_loss() {
        let mut s = good_snapshot();
        s.price_usd = Decimal::ZERO;
        assert_eq!(
            exit(&position("40"), &s, t0()),
            Some(IntentKind::Exit(ExitReason::StopLoss))
        );
    }

    #[test]
    fn exits_clamp_bad_params_instead_of_blocking() {
        let p = position("40");
        let late = t0() + Duration::hours(4);
        let mut bad = params();
        bad.exit_slippage_bps = 0;
        bad.intent_ttl = Duration::days(1);
        let intent = evaluate_exit(
            &p,
            &good_snapshot(),
            &bad,
            &costs(),
            Duration::seconds(30),
            late,
        )
        .unwrap();
        assert_eq!(intent.max_slippage_bps, bounds::EXIT_SLIPPAGE_BPS.0);
        assert_eq!(intent.expires_at, late + bounds::INTENT_TTL.1);
        bad.exit_slippage_bps = u32::MAX;
        let intent = evaluate_exit(
            &p,
            &good_snapshot(),
            &bad,
            &costs(),
            Duration::seconds(30),
            late,
        )
        .unwrap();
        assert_eq!(intent.max_slippage_bps, bounds::EXIT_SLIPPAGE_BPS.1);
    }

    #[test]
    fn collapsed_price_or_broken_costs_stop_out() {
        let p = position("40");
        let mut s = good_snapshot();
        s.price_usd = d("0.00000000000000000001");
        assert_eq!(
            exit(&p, &s, t0()),
            Some(IntentKind::Exit(ExitReason::StopLoss))
        );

        let broken = CostModel {
            dex_fee_bps: 10_000,
            ..costs()
        };
        let intent = evaluate_exit(
            &p,
            &good_snapshot(),
            &params(),
            &broken,
            Duration::seconds(30),
            t0(),
        );
        assert_eq!(
            intent.map(|i| i.kind),
            Some(IntentKind::Exit(ExitReason::StopLoss))
        );
    }
}
