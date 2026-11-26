//! The root-owned paper profile: the watchlist and the simulation inputs.
//! Core reads it and `jarvis memecoin init-profile` writes it; both parse
//! and validate it here, so the rules exist once. No API writes this file.
//! This is the crate's only I/O: reading that file.
//!
//! `stake_usd`, `max_position_usd` and `max_open_positions` size the
//! simulation only. They are never limits for live trading, which come from
//! a signed mandate alone.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::filter::FilterRules;
use crate::paper::CostModel;
use crate::strategy::{bounds, StrategyParams};
use crate::types::ExtensionKind;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Duration;

/// Largest profile file read at all.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Typed strategy versions start above this; legacy profile versions stay
/// below it. Versions are unique across strategies, so the two never collide.
pub const STRATEGY_VERSION_FLOOR: u32 = 1_000_000;
/// Claude's decision and review calls per rolling hour and day. The profile
/// can only lower these.
pub const MAX_CALLS_PER_HOUR: u32 = 4;
pub const MAX_CALLS_PER_DAY: u32 = 48;
/// One DexScreener call carries every mint of a tick (at most 30).
pub const MAX_WATCHLIST: usize = 20;
pub const MAX_OPEN_POSITIONS: usize = 10;
/// Upper bound for every duration in the file, so none can overflow.
const MAX_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    watchlist: Vec<String>,
    /// Also observe mints from pump.fun migrations (on unless set false).
    #[serde(default = "on")]
    discovery: bool,
    goal_multiple: Decimal,
    stake_usd: Decimal,
    max_position_usd: Decimal,
    max_open_positions: usize,
    /// Most one trade may lose at its stop, fees and slippage included, as a
    /// share of the paper equity. At most the code cap (10%), which is also
    /// the default.
    #[serde(default = "max_risk_per_trade_bps")]
    max_risk_per_trade_bps: u32,
    /// Most all open positions together may lose at their stops, as a share
    /// of the paper equity. At most the code cap (30%), which is also the
    /// default, and at least `max_risk_per_trade_bps`.
    #[serde(default = "max_total_risk_bps")]
    max_total_risk_bps: u32,
    filter: FilterFile,
    /// Deprecated: the single strategy of profiles written before typed
    /// strategies. It no longer enters; it only names the exit rules of the
    /// positions it opened.
    #[serde(default)]
    strategy: Option<StrategyFile>,
    costs: CostFile,
    /// Claude decides the paper entries and exits and reviews the strategies
    /// daily. Absent: off, and each strategy enters on its own signal.
    #[serde(default)]
    agent: Option<AgentFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentFile {
    /// Lower than the code caps only.
    #[serde(default)]
    max_calls_per_hour: Option<u32>,
    #[serde(default)]
    max_calls_per_day: Option<u32>,
    /// Strategies Claude creates start enabled (on unless set false).
    #[serde(default = "on")]
    new_strategies_enabled: bool,
}

fn on() -> bool {
    true
}

/// What the profile allows Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentSettings {
    pub max_calls_per_hour: u32,
    pub max_calls_per_day: u32,
    /// Strategies Claude creates start enabled.
    pub new_strategies_enabled: bool,
}

impl AgentSettings {
    /// Unset caps take the code caps; a higher one is refused.
    pub fn new(
        per_hour: Option<u32>,
        per_day: Option<u32>,
        new_strategies_enabled: bool,
    ) -> Result<Self, String> {
        let per_hour = per_hour.unwrap_or(MAX_CALLS_PER_HOUR);
        let per_day = per_day.unwrap_or(MAX_CALLS_PER_DAY);
        if !(1..=MAX_CALLS_PER_HOUR).contains(&per_hour)
            || !(1..=MAX_CALLS_PER_DAY).contains(&per_day)
        {
            return Err(format!(
                "agent calls must be 1 to {MAX_CALLS_PER_HOUR} per hour and 1 to {MAX_CALLS_PER_DAY} per day"
            ));
        }
        Ok(Self {
            max_calls_per_hour: per_hour,
            max_calls_per_day: per_day,
            new_strategies_enabled,
        })
    }
}

fn max_risk_per_trade_bps() -> u32 {
    bounds::RISK_PER_TRADE_BPS.1
}

fn max_total_risk_bps() -> u32 {
    bounds::TOTAL_RISK_BPS.1
}

/// Everything that decided the results of a legacy strategy. It is recorded
/// per legacy version, so changing any of it without a new version stops
/// the trader while the profile still has a `strategy`.
#[derive(Serialize)]
struct Versioned<'a> {
    stake_usd: &'a Decimal,
    max_position_usd: &'a Decimal,
    max_open_positions: usize,
    filter: &'a FilterFile,
    strategy: &'a StrategyFile,
    costs: &'a CostFile,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FilterFile {
    min_liquidity_usd: Decimal,
    min_pool_age_secs: u64,
    max_top_holders_share: Decimal,
    max_sell_impact_bps: u32,
    allowed_extensions: Vec<Extension>,
    max_snapshot_age_secs: u64,
    min_lp_lock_remaining_secs: u64,
}

#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Extension {
    MetadataPointer,
    TokenMetadata,
    TransferFeeConfig,
    TransferHook,
    PermanentDelegate,
    DefaultAccountState,
    Pausable,
    MintCloseAuthority,
    NonTransferable,
    ConfidentialTransfer,
    InterestBearing,
}

impl From<Extension> for ExtensionKind {
    fn from(value: Extension) -> Self {
        match value {
            Extension::MetadataPointer => Self::MetadataPointer,
            Extension::TokenMetadata => Self::TokenMetadata,
            Extension::TransferFeeConfig => Self::TransferFeeConfig,
            Extension::TransferHook => Self::TransferHook,
            Extension::PermanentDelegate => Self::PermanentDelegate,
            Extension::DefaultAccountState => Self::DefaultAccountState,
            Extension::Pausable => Self::Pausable,
            Extension::MintCloseAuthority => Self::MintCloseAuthority,
            Extension::NonTransferable => Self::NonTransferable,
            Extension::ConfidentialTransfer => Self::ConfidentialTransfer,
            Extension::InterestBearing => Self::InterestBearing,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrategyFile {
    version: u32,
    min_buy_ratio_bps: u32,
    min_momentum_bps: i32,
    max_momentum_bps: i32,
    position_share_bps: u32,
    max_liquidity_share_bps: u32,
    take_profit_bps: u32,
    stop_loss_bps: u32,
    liquidity_drop_exit_bps: u32,
    max_hold_secs: u64,
    entry_slippage_bps: u32,
    exit_slippage_bps: u32,
    intent_ttl_secs: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CostFile {
    dex_fee_bps: u32,
    network_fee_usd: Decimal,
    priority_fee_usd: Decimal,
    failure_rate: Decimal,
}

/// A validated paper profile.
#[derive(Debug, Clone)]
pub struct PaperProfile {
    pub watchlist: Vec<String>,
    /// Candidates from pump.fun migrations next to the watchlist.
    pub discovery: bool,
    pub goal_multiple: Decimal,
    pub stake_usd: Decimal,
    pub max_position_usd: Decimal,
    pub max_open_positions: usize,
    /// Every entry is cut so it risks at most this share of the equity.
    pub max_risk_per_trade_bps: u32,
    /// Every entry is cut so all open positions together risk at most this
    /// share of the equity.
    pub max_total_risk_bps: u32,
    pub rules: FilterRules,
    /// Deprecated legacy strategy, if the profile still has one. Its version
    /// is below [`STRATEGY_VERSION_FLOOR`].
    pub strategy: Option<StrategyParams>,
    pub costs: CostModel,
    /// Canonical JSON of everything that decided the legacy strategy's
    /// results, recorded with its version; `None` without one.
    pub versioned_params: Option<String>,
    /// `None`: Claude takes no part.
    pub agent: Option<AgentSettings>,
}

fn seconds(value: u64, field: &str) -> Result<Duration, String> {
    if value > MAX_SECS {
        return Err(format!("{field} is above 30 days"));
    }
    i64::try_from(value)
        .map(Duration::seconds)
        .map_err(|_| format!("{field} is out of range"))
}

/// The deprecated legacy strategy, validated against the code bounds. Its
/// version must stay below [`STRATEGY_VERSION_FLOOR`], where typed strategy
/// versions start, so the two can never collide.
fn legacy_strategy(s: &StrategyFile) -> Result<StrategyParams, String> {
    if s.version >= STRATEGY_VERSION_FLOOR {
        return Err(format!(
            "strategy.version must be below {STRATEGY_VERSION_FLOOR}"
        ));
    }
    let strategy = StrategyParams {
        version: s.version,
        min_buy_ratio_bps: s.min_buy_ratio_bps,
        min_momentum_bps: s.min_momentum_bps,
        max_momentum_bps: s.max_momentum_bps,
        position_share_bps: s.position_share_bps,
        max_liquidity_share_bps: s.max_liquidity_share_bps,
        take_profit_bps: s.take_profit_bps,
        stop_loss_bps: s.stop_loss_bps,
        liquidity_drop_exit_bps: s.liquidity_drop_exit_bps,
        max_hold: seconds(s.max_hold_secs, "max_hold_secs")?,
        entry_slippage_bps: s.entry_slippage_bps,
        exit_slippage_bps: s.exit_slippage_bps,
        intent_ttl: seconds(s.intent_ttl_secs, "intent_ttl_secs")?,
    };
    strategy
        .validate()
        .map_err(|error| format!("strategy parameters: {error}"))?;
    Ok(strategy)
}

/// A base58 Solana address, as the schema also requires.
pub fn is_mint(value: &str) -> bool {
    (32..=44).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l'))
}

impl PaperProfile {
    pub fn parse(json: &str) -> Result<Self, String> {
        let file: ProfileFile = serde_json::from_str(json)
            .map_err(|error| format!("invalid paper profile: {error}"))?;
        let v = &file;
        if file.watchlist.is_empty() || file.watchlist.len() > MAX_WATCHLIST {
            return Err(format!("watchlist must hold 1 to {MAX_WATCHLIST} mints"));
        }
        if !file.watchlist.iter().all(|mint| is_mint(mint))
            || file.watchlist.iter().collect::<BTreeSet<_>>().len() != file.watchlist.len()
        {
            return Err("watchlist must hold distinct base58 mints".into());
        }
        if !(1..=MAX_OPEN_POSITIONS).contains(&v.max_open_positions) {
            return Err(format!(
                "max_open_positions must be 1 to {MAX_OPEN_POSITIONS}"
            ));
        }
        // ponytail: no cash ledger; open positions stay within the stake, entry
        // and failed-attempt fees aside. Track cash if fees ever matter here.
        let committed = v
            .max_position_usd
            .checked_mul(Decimal::from(v.max_open_positions));
        if v.stake_usd <= Decimal::ZERO
            || v.max_position_usd <= Decimal::ZERO
            || committed.is_none_or(|committed| committed > v.stake_usd)
        {
            return Err("stake_usd must cover max_position_usd x max_open_positions".into());
        }
        let (min_risk, max_risk) = bounds::RISK_PER_TRADE_BPS;
        if !(min_risk..=max_risk).contains(&v.max_risk_per_trade_bps) {
            return Err(format!(
                "max_risk_per_trade_bps must be {min_risk} to {max_risk}"
            ));
        }
        let (min_total, max_total) = bounds::TOTAL_RISK_BPS;
        if !(min_total..=max_total).contains(&v.max_total_risk_bps)
            || v.max_total_risk_bps < v.max_risk_per_trade_bps
        {
            return Err(format!(
                "max_total_risk_bps must be {min_total} to {max_total} and at least max_risk_per_trade_bps"
            ));
        }
        if file.goal_multiple <= Decimal::ONE {
            return Err("goal_multiple must be above 1".into());
        }
        let f = &v.filter;
        if f.min_liquidity_usd < Decimal::ZERO
            || f.max_top_holders_share < Decimal::ZERO
            || f.max_top_holders_share > Decimal::ONE
            || f.max_sell_impact_bps > 10_000
            || f.max_snapshot_age_secs == 0
        {
            return Err("filter rules are out of range".into());
        }
        let rules = FilterRules {
            min_liquidity_usd: f.min_liquidity_usd,
            min_pool_age: seconds(f.min_pool_age_secs, "min_pool_age_secs")?,
            max_top_holders_share: f.max_top_holders_share,
            max_sell_impact_bps: f.max_sell_impact_bps,
            allowed_extensions: f.allowed_extensions.iter().map(|&e| e.into()).collect(),
            max_snapshot_age: seconds(f.max_snapshot_age_secs, "max_snapshot_age_secs")?,
            min_lp_lock_remaining: seconds(
                f.min_lp_lock_remaining_secs,
                "min_lp_lock_remaining_secs",
            )?,
        };
        let strategy = v.strategy.as_ref().map(legacy_strategy).transpose()?;
        let costs = CostModel {
            dex_fee_bps: v.costs.dex_fee_bps,
            network_fee_usd: v.costs.network_fee_usd,
            priority_fee_usd: v.costs.priority_fee_usd,
            failure_rate: v.costs.failure_rate,
        };
        // A certain failure would make every exit fail until it is stuck.
        if !costs.is_valid() || costs.failure_rate >= Decimal::ONE {
            return Err("cost model is out of range".into());
        }
        let versioned_params = v
            .strategy
            .as_ref()
            .map(|strategy| {
                serde_json::to_string(&Versioned {
                    stake_usd: &v.stake_usd,
                    max_position_usd: &v.max_position_usd,
                    max_open_positions: v.max_open_positions,
                    filter: &v.filter,
                    strategy,
                    costs: &v.costs,
                })
                .map_err(|error| format!("paper profile cannot be recorded: {error}"))
            })
            .transpose()?;
        Ok(Self {
            watchlist: file.watchlist,
            discovery: file.discovery,
            goal_multiple: file.goal_multiple,
            stake_usd: file.stake_usd,
            max_position_usd: file.max_position_usd,
            max_open_positions: file.max_open_positions,
            max_risk_per_trade_bps: file.max_risk_per_trade_bps,
            max_total_risk_bps: file.max_total_risk_bps,
            rules,
            strategy,
            costs,
            versioned_params,
            agent: file
                .agent
                .map(|a| {
                    AgentSettings::new(
                        a.max_calls_per_hour,
                        a.max_calls_per_day,
                        a.new_strategies_enabled,
                    )
                })
                .transpose()?,
        })
    }

    /// Read the profile. In production it must be a root-owned regular file
    /// with mode 0640 and its directory's group, like the Hugging Face
    /// catalog, so Core can read but never change it.
    pub fn load(path: &Path, enforce_production_boundary: bool) -> Result<Self, String> {
        // O_NOFOLLOW and fstat: the file checked is the file read.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("paper profile unavailable: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("paper profile unavailable: {error}"))?;
        if enforce_production_boundary {
            let parent = path
                .parent()
                .and_then(|parent| fs::metadata(parent).ok())
                .ok_or("paper profile has no trusted parent")?;
            if metadata.uid() != 0
                || metadata.gid() != parent.gid()
                || metadata.permissions().mode() & 0o777 != 0o640
            {
                return Err("paper profile permissions are unsafe".into());
            }
        }
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err("paper profile must be a regular file of at most 64 KiB".into());
        }
        let mut json = String::new();
        file.take(MAX_FILE_BYTES)
            .read_to_string(&mut json)
            .map_err(|error| format!("paper profile unreadable: {error}"))?;
        Self::parse(&json)
    }
}
