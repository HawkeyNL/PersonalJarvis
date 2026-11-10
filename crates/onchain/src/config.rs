//! Typed strategy configurations.
//!
//! A [`StrategyConfig`] is one version of one strategy: which code-defined
//! [`SignalId`] conditions must all hold to enter, how big to enter, and when
//! to exit. Configs may be proposed by a learning step, so
//! [`StrategyConfig::validate`] refuses (never clamps) every value outside
//! the code bounds in [`crate::strategy::bounds`] intersected with the
//! bounds of the config's [`Family`], and every filter override that would
//! loosen the mandate's [`FilterRules`].
//!
//! Configs are parsed with unknown fields denied, so a misspelt field is an
//! error rather than a silently ignored setting. Durations are whole seconds.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
use time::Duration;

use crate::filter::{FilterRules, Loosening};
use crate::signals::{self, Series, SignalId};
use crate::strategy::{bounds, within, ExitRules, OutOfBounds};

/// Strategy families from the strategy research.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    /// Graduated survivor momentum: hours-old tokens making new highs.
    A,
    /// Base reclaim: a dumped token breaking out of a tight range.
    B,
    /// Trend following on established tokens, days to weeks.
    C,
}

/// Per-family bounds, narrower than the code bounds.
struct FamilyBounds {
    signals: &'static [SignalId],
    max_hold: (Duration, Duration),
    entries_per_day: (u32, u32),
}

impl Family {
    fn bounds(self) -> FamilyBounds {
        use SignalId::*;
        match self {
            Family::A => FamilyBounds {
                signals: &[
                    Return15m,
                    Return1h,
                    PriceVsSma30m,
                    BreakoutHigh1h,
                    DrawdownFromHigh,
                    LiquidityChange1h,
                    LiquidityDrawdownFromPeak,
                    PoolAgeMinutes,
                ],
                max_hold: (Duration::minutes(15), Duration::hours(24)),
                entries_per_day: (1, 6),
            },
            Family::B => FamilyBounds {
                signals: &[
                    Return1h,
                    Return6h,
                    Return24h,
                    BreakoutHigh2h,
                    RangeWidth2h,
                    DrawdownFromHigh,
                    LiquidityChange2h,
                    LiquidityDrawdownFromPeak,
                    PoolAgeMinutes,
                ],
                max_hold: (Duration::hours(1), Duration::days(3)),
                entries_per_day: (1, 4),
            },
            Family::C => FamilyBounds {
                signals: &[
                    Return24h,
                    PriceVsSma20d,
                    BreakoutHigh10d,
                    DrawdownFromHigh,
                    LiquidityDrawdownFromPeak,
                    PoolAgeMinutes,
                ],
                max_hold: (Duration::days(1), bounds::CONFIG_MAX_HOLD.1),
                entries_per_day: (1, 2),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// Strictly above `a`.
    Gt,
    /// Strictly below `a`.
    Lt,
    /// Within `a..=b`.
    Between,
}

/// One entry condition. `b` is required for `between` and refused otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub signal: SignalId,
    pub op: Op,
    pub a: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<i64>,
}

impl Condition {
    /// Whether the condition holds at the series' latest observation up to
    /// `now`. A signal without a value never holds.
    pub fn holds(&self, series: &Series, now: time::OffsetDateTime) -> bool {
        let Some(v) = signals::compute(self.signal, series, now) else {
            return false;
        };
        match (self.op, self.b) {
            (Op::Gt, _) => v > self.a,
            (Op::Lt, _) => v < self.a,
            (Op::Between, Some(b)) => (self.a..=b).contains(&v),
            (Op::Between, None) => false,
        }
    }

    fn is_valid(&self) -> bool {
        let (min, max) = self.signal.range();
        let in_range = |v: i64| (min..=max).contains(&v);
        match (self.op, self.b) {
            (Op::Gt | Op::Lt, None) => in_range(self.a),
            (Op::Between, Some(b)) => in_range(self.a) && in_range(b) && self.a <= b,
            _ => false,
        }
    }
}

/// Stricter-only overrides of the mandate's filter rules. Unset fields keep
/// the mandate's value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_liquidity_usd: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_pool_age_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_top_holders_share: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sell_impact_bps: Option<u32>,
}

/// One version of one strategy. Units are basis points unless the name says
/// otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyConfig {
    /// `^[a-z0-9-]{1,32}$`, stable across versions.
    pub id: String,
    pub version: u32,
    /// The version this one was derived from; always lower than `version`.
    pub parent: Option<u32>,
    pub family: Family,
    /// All must hold (AND).
    pub entry: Vec<Condition>,
    /// Entry size as a share of the mandate's maximum position value.
    pub position_share_bps: u32,
    /// Entry size cap as a share of pool liquidity.
    pub max_liquidity_share_bps: u32,
    pub take_profit_bps: u32,
    pub stop_loss_bps: u32,
    pub trailing_stop_bps: Option<u32>,
    pub liquidity_drop_exit_bps: u32,
    pub max_hold_secs: u64,
    pub entry_slippage_bps: u32,
    pub exit_slippage_bps: u32,
    /// Entries across all tokens per UTC date.
    pub max_entries_per_day: u32,
    /// Minimum time between leaving a token and entering it again.
    pub token_cooldown_secs: u64,
    #[serde(default)]
    pub filters: FilterOverrides,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("strategy id must match ^[a-z0-9-]{{1,32}}$")]
    InvalidId,
    #[error(transparent)]
    OutOfBounds(#[from] OutOfBounds),
    #[error("signal {signal:?} is not allowed for family {family:?}")]
    SignalNotAllowed { signal: SignalId, family: Family },
    #[error("entry condition {index} is malformed or outside its signal's range")]
    InvalidCondition { index: usize },
    #[error(transparent)]
    Loosening(#[from] Loosening),
}

impl StrategyConfig {
    /// Refuse anything outside code bounds ∩ family bounds, or a filter
    /// override looser than the mandate's `base` rules. Nothing is clamped:
    /// an invalid config is an error, never a corrected config. On success
    /// returns `base` tightened by the overrides.
    pub fn validate(&self, base: &FilterRules) -> Result<FilterRules, ConfigError> {
        use bounds::*;
        let valid_id = (1..=32).contains(&self.id.len())
            && self
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
        if !valid_id {
            return Err(ConfigError::InvalidId);
        }
        let family = self.family.bounds();
        within("version", self.version, (1, u32::MAX))?;
        if let Some(parent) = self.parent {
            within("parent", parent, (1, self.version - 1))?;
        }
        within("entry", self.entry.len(), ENTRY_CONDITIONS)?;
        for (index, c) in self.entry.iter().enumerate() {
            if !family.signals.contains(&c.signal) {
                return Err(ConfigError::SignalNotAllowed {
                    signal: c.signal,
                    family: self.family,
                });
            }
            if !c.is_valid() {
                return Err(ConfigError::InvalidCondition { index });
            }
        }
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
        if let Some(trail) = self.trailing_stop_bps {
            within("trailing_stop_bps", trail, TRAILING_STOP_BPS)?;
        }
        within(
            "liquidity_drop_exit_bps",
            self.liquidity_drop_exit_bps,
            LIQUIDITY_DROP_BPS,
        )?;
        let max_hold = (
            CONFIG_MAX_HOLD.0.max(family.max_hold.0),
            CONFIG_MAX_HOLD.1.min(family.max_hold.1),
        );
        within("max_hold_secs", secs(self.max_hold_secs), max_hold)?;
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
        let entries = (
            ENTRIES_PER_DAY.0.max(family.entries_per_day.0),
            ENTRIES_PER_DAY.1.min(family.entries_per_day.1),
        );
        within("max_entries_per_day", self.max_entries_per_day, entries)?;
        within(
            "token_cooldown_secs",
            secs(self.token_cooldown_secs),
            TOKEN_COOLDOWN,
        )?;
        Ok(self.filter_rules(base)?)
    }

    /// The mandate's `base` rules with this config's overrides applied, or
    /// the first override that would loosen them.
    fn filter_rules(&self, base: &FilterRules) -> Result<FilterRules, Loosening> {
        let f = &self.filters;
        let mut rules = base.clone();
        if let Some(v) = f.min_liquidity_usd {
            rules.min_liquidity_usd = v;
        }
        if let Some(v) = f.min_pool_age_secs {
            rules.min_pool_age = secs(v);
        }
        if let Some(v) = f.max_top_holders_share {
            rules.max_top_holders_share = v;
        }
        if let Some(v) = f.max_sell_impact_bps {
            rules.max_sell_impact_bps = v;
        }
        base.tighten(rules)
    }

    /// All entry conditions hold at `now`.
    pub fn entry_signal(&self, series: &Series, now: time::OffsetDateTime) -> bool {
        self.entry.iter().all(|c| c.holds(series, now))
    }

    pub fn exit_rules(&self) -> ExitRules {
        ExitRules {
            take_profit_bps: self.take_profit_bps,
            stop_loss_bps: self.stop_loss_bps,
            trailing_stop_bps: self.trailing_stop_bps,
            liquidity_drop_exit_bps: self.liquidity_drop_exit_bps,
            max_hold: secs(self.max_hold_secs),
        }
    }

    pub fn token_cooldown(&self) -> Duration {
        secs(self.token_cooldown_secs)
    }

    /// USD to spend on an entry: the config's share of the mandate's
    /// `max_position_usd`, capped by its share of pool liquidity and rounded
    /// down to cents. `None` when that is nothing or overflows.
    pub fn entry_size(&self, liquidity_usd: Decimal, max_position_usd: Decimal) -> Option<Decimal> {
        let bps = Decimal::from(10_000);
        let by_mandate =
            max_position_usd.checked_mul(Decimal::from(self.position_share_bps))? / bps;
        let by_liquidity =
            liquidity_usd.checked_mul(Decimal::from(self.max_liquidity_share_bps))? / bps;
        // Round down: rounding up could exceed the mandate limit by a cent.
        let size = by_mandate
            .min(by_liquidity)
            .round_dp_with_strategy(2, RoundingStrategy::ToZero);
        (size > Decimal::ZERO).then_some(size)
    }
}

/// Seconds as a duration; values beyond `i64` saturate (and fail any bound).
fn secs(value: u64) -> Duration {
    i64::try_from(value).map_or(Duration::MAX, Duration::seconds)
}

/// Version-1 seed configs for families A, B and C. Sizes assume the
/// mandate's per-position limit is the intended trade size.
pub fn seeds() -> [StrategyConfig; 3] {
    use SignalId::*;
    let cond = |signal, op, a, b| Condition { signal, op, a, b };
    let hour = 3_600;
    let day = 24 * hour;
    [
        StrategyConfig {
            id: "a-survivor-momentum".to_owned(),
            version: 1,
            parent: None,
            family: Family::A,
            entry: vec![
                cond(PoolAgeMinutes, Op::Between, 30, Some(24 * 60)),
                cond(PriceVsSma30m, Op::Gt, 0, None),
                cond(BreakoutHigh1h, Op::Gt, 0, None),
                cond(Return1h, Op::Between, 1_000, Some(8_000)),
                cond(LiquidityChange1h, Op::Gt, 0, None),
                cond(LiquidityDrawdownFromPeak, Op::Lt, 2_000, None),
            ],
            position_share_bps: 10_000,
            max_liquidity_share_bps: 100,
            // No partial exits yet: a wide target, and the trail locks in gains.
            take_profit_bps: 15_000,
            stop_loss_bps: 2_000,
            trailing_stop_bps: Some(2_500),
            liquidity_drop_exit_bps: 3_000,
            max_hold_secs: 6 * hour,
            entry_slippage_bps: 150,
            exit_slippage_bps: 800,
            max_entries_per_day: 4,
            token_cooldown_secs: 6 * hour,
            filters: FilterOverrides {
                min_liquidity_usd: Some(Decimal::from(50_000)),
                ..FilterOverrides::default()
            },
        },
        StrategyConfig {
            id: "b-base-reclaim".to_owned(),
            version: 1,
            parent: None,
            family: Family::B,
            entry: vec![
                cond(PoolAgeMinutes, Op::Between, 6 * 60, Some(72 * 60)),
                cond(DrawdownFromHigh, Op::Gt, 6_000, None),
                cond(RangeWidth2h, Op::Lt, 3_000, None),
                cond(BreakoutHigh2h, Op::Gt, 0, None),
                cond(LiquidityChange2h, Op::Between, -1_500, Some(1_500)),
            ],
            position_share_bps: 10_000,
            max_liquidity_share_bps: 100,
            take_profit_bps: 3_500,
            stop_loss_bps: 1_800,
            trailing_stop_bps: None,
            liquidity_drop_exit_bps: 2_500,
            max_hold_secs: day,
            entry_slippage_bps: 150,
            exit_slippage_bps: 800,
            max_entries_per_day: 2,
            token_cooldown_secs: day,
            filters: FilterOverrides {
                min_liquidity_usd: Some(Decimal::from(50_000)),
                min_pool_age_secs: Some(6 * hour),
                ..FilterOverrides::default()
            },
        },
        StrategyConfig {
            id: "c-established-trend".to_owned(),
            version: 1,
            parent: None,
            family: Family::C,
            entry: vec![
                cond(PoolAgeMinutes, Op::Gt, 14 * 24 * 60, None),
                cond(PriceVsSma20d, Op::Gt, 0, None),
                cond(BreakoutHigh10d, Op::Gt, 0, None),
            ],
            position_share_bps: 10_000,
            max_liquidity_share_bps: 50,
            // Let winners run; the trail stands in for "close below the
            // 10-day low" until exits can use signals.
            take_profit_bps: 100_000,
            stop_loss_bps: 2_500,
            trailing_stop_bps: Some(3_000),
            liquidity_drop_exit_bps: 3_000,
            max_hold_secs: 30 * day,
            entry_slippage_bps: 100,
            exit_slippage_bps: 500,
            max_entries_per_day: 2,
            token_cooldown_secs: 7 * day,
            filters: FilterOverrides {
                min_liquidity_usd: Some(Decimal::from(250_000)),
                min_pool_age_secs: Some(14 * day),
                ..FilterOverrides::default()
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::tests::rules;
    use crate::testutil::{d, Rng};

    fn config() -> StrategyConfig {
        seeds()[0].clone()
    }

    #[test]
    fn seeds_validate_and_only_tighten() {
        let base = rules();
        for seed in seeds() {
            let tight = seed.validate(&base).expect("seed validates");
            assert_eq!(base.tighten(tight.clone()), Ok(tight));
        }
    }

    #[test]
    fn json_round_trips_and_denies_unknown_fields() {
        for seed in seeds() {
            let json = serde_json::to_string(&seed).unwrap();
            assert_eq!(serde_json::from_str::<StrategyConfig>(&json).unwrap(), seed);
            let extra = json.replacen('{', r#"{"max_position_usd":"1000","#, 1);
            assert!(serde_json::from_str::<StrategyConfig>(&extra).is_err());
        }
        let bad_cond = r#"{"signal":"return1h","op":"gt","a":1,"bonus":2}"#;
        assert!(serde_json::from_str::<Condition>(bad_cond).is_err());
        let bad_signal = r#"{"signal":"buys_5m","op":"gt","a":1}"#;
        assert!(serde_json::from_str::<Condition>(bad_signal).is_err());
        let bad_filter = r#"{"allowed_extensions":[]}"#;
        assert!(serde_json::from_str::<FilterOverrides>(bad_filter).is_err());
    }

    #[test]
    fn validate_rejects_every_out_of_bounds_field() {
        type Mutation = fn(&mut StrategyConfig);
        let oob = |field| Err(ConfigError::OutOfBounds(OutOfBounds { field }));
        let cases: &[(Mutation, Result<(), ConfigError>)] = &[
            (|c| c.id = String::new(), Err(ConfigError::InvalidId)),
            (|c| c.id = "A-upper".to_owned(), Err(ConfigError::InvalidId)),
            (|c| c.id = "a_b".to_owned(), Err(ConfigError::InvalidId)),
            (|c| c.id = "a".repeat(33), Err(ConfigError::InvalidId)),
            (|c| c.version = 0, oob("version")),
            (|c| c.parent = Some(1), oob("parent")),
            (|c| c.entry.clear(), oob("entry")),
            (|c| c.entry = vec![c.entry[0]; 9], oob("entry")),
            (
                |c| c.entry[1].signal = SignalId::PriceVsSma20d,
                Err(ConfigError::SignalNotAllowed {
                    signal: SignalId::PriceVsSma20d,
                    family: Family::A,
                }),
            ),
            (
                |c| c.entry[1].b = Some(5),
                Err(ConfigError::InvalidCondition { index: 1 }),
            ),
            (
                |c| c.entry[0].b = None,
                Err(ConfigError::InvalidCondition { index: 0 }),
            ),
            (
                |c| c.entry[0].a = 10_000,
                Err(ConfigError::InvalidCondition { index: 0 }),
            ),
            (
                |c| c.entry[5].a = 10_001,
                Err(ConfigError::InvalidCondition { index: 5 }),
            ),
            (|c| c.position_share_bps = 0, oob("position_share_bps")),
            (
                |c| c.max_liquidity_share_bps = 201,
                oob("max_liquidity_share_bps"),
            ),
            (|c| c.take_profit_bps = 99, oob("take_profit_bps")),
            (|c| c.stop_loss_bps = 9_001, oob("stop_loss_bps")),
            (|c| c.trailing_stop_bps = Some(99), oob("trailing_stop_bps")),
            (
                |c| c.liquidity_drop_exit_bps = 0,
                oob("liquidity_drop_exit_bps"),
            ),
            // Inside the code bound, outside family A's 15 min .. 24 h.
            (|c| c.max_hold_secs = 60, oob("max_hold_secs")),
            (|c| c.max_hold_secs = 2 * 86_400, oob("max_hold_secs")),
            (|c| c.max_hold_secs = u64::MAX, oob("max_hold_secs")),
            (|c| c.entry_slippage_bps = 1_001, oob("entry_slippage_bps")),
            (|c| c.exit_slippage_bps = 0, oob("exit_slippage_bps")),
            (|c| c.max_entries_per_day = 0, oob("max_entries_per_day")),
            (|c| c.max_entries_per_day = 7, oob("max_entries_per_day")),
            (|c| c.token_cooldown_secs = 0, oob("token_cooldown_secs")),
            (
                |c| c.token_cooldown_secs = 31 * 86_400,
                oob("token_cooldown_secs"),
            ),
        ];
        for (mutate, expected) in cases {
            let mut c = config();
            mutate(&mut c);
            let before = c.clone();
            assert_eq!(c.validate(&rules()).map(|_| ()), *expected, "{c:?}");
            assert_eq!(c, before, "validate never changes the config");
        }
    }

    #[test]
    fn random_configs_are_rejected_unless_inside_bounds() {
        // Fuzz single fields far beyond their bounds; anything accepted must
        // be inside every bound, and nothing is ever rewritten.
        let mut rng = Rng::new(17);
        for _ in 0..2_000 {
            let mut c = config();
            let v = rng.next();
            match rng.below(6) {
                0 => c.position_share_bps = v as u32 % 20_000,
                1 => c.stop_loss_bps = v as u32 % 20_000,
                2 => c.trailing_stop_bps = Some(v as u32 % 20_000),
                3 => c.max_hold_secs = v % (40 * 86_400),
                4 => c.max_entries_per_day = v as u32 % 40,
                _ => c.entry[3].a = (v % 2_000_000) as i64 - 1_000_000,
            }
            let before = c.clone();
            if c.validate(&rules()).is_ok() {
                assert!((1..=10_000).contains(&c.position_share_bps));
                assert!((100..=9_000).contains(&c.stop_loss_bps));
                assert!(c
                    .trailing_stop_bps
                    .is_some_and(|t| (100..=9_000).contains(&t)));
                assert!((15 * 60..=86_400).contains(&c.max_hold_secs));
                assert!((1..=6).contains(&c.max_entries_per_day));
                assert!(c.entry[3].a >= -10_000 && c.entry[3].a <= c.entry[3].b.unwrap());
            }
            assert_eq!(c, before);
        }
    }

    #[test]
    fn filter_overrides_only_tighten() {
        let base = rules();
        type Mutation = fn(&mut FilterOverrides);
        let loosen: &[(Mutation, &str)] = &[
            (
                |f| f.min_liquidity_usd = Some(d("49999")),
                "min_liquidity_usd",
            ),
            (|f| f.min_pool_age_secs = Some(0), "min_pool_age"),
            (
                |f| f.max_top_holders_share = Some(d("0.31")),
                "max_top_holders_share",
            ),
            (
                |f| f.max_sell_impact_bps = Some(u32::MAX),
                "max_sell_impact_bps",
            ),
        ];
        for (mutate, field) in loosen {
            let mut c = config();
            c.filters = FilterOverrides::default();
            mutate(&mut c.filters);
            assert_eq!(
                c.validate(&base),
                Err(ConfigError::Loosening(Loosening { field }))
            );
        }
        let mut c = config();
        c.filters = FilterOverrides::default();
        assert_eq!(c.validate(&base), Ok(base.clone()));
        c.filters.max_sell_impact_bps = Some(1);
        c.filters.min_pool_age_secs = Some(u64::MAX);
        let tight = c.validate(&base).unwrap();
        assert_eq!(tight.max_sell_impact_bps, 1);
        assert_eq!(tight.min_pool_age, Duration::MAX);
    }
}
