//! The root-owned paper profile: the watchlist and the simulation inputs.
//!
//! `stake_usd`, `max_position_usd` and `max_open_positions` size the
//! simulation only. They are never limits for live trading, which come from
//! a signed mandate alone. No API writes this file. Values stay private: the
//! repository ships no defaults on purpose.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use jarvis_onchain::filter::FilterRules;
use jarvis_onchain::paper::CostModel;
use jarvis_onchain::strategy::StrategyParams;
use jarvis_onchain::types::ExtensionKind;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use time::Duration;

const MAX_FILE_BYTES: u64 = 64 * 1024;
/// One DexScreener call carries every mint of a tick (at most 30).
pub const MAX_WATCHLIST: usize = 20;
pub const MAX_OPEN_POSITIONS: usize = 10;
/// Upper bound for every duration in the file, so none can overflow.
const MAX_SECS: u64 = 30 * 24 * 60 * 60;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    watchlist: Vec<String>,
    goal_multiple: Decimal,
    stake_usd: Decimal,
    max_position_usd: Decimal,
    max_open_positions: usize,
    filter: FilterFile,
    strategy: StrategyFile,
    costs: CostFile,
}

/// Everything that decides results. It is recorded per strategy version, so
/// changing any of it without a new version stops the trader.
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
    pub goal_multiple: Decimal,
    pub stake_usd: Decimal,
    pub max_position_usd: Decimal,
    pub max_open_positions: usize,
    pub rules: FilterRules,
    pub strategy: StrategyParams,
    pub costs: CostModel,
    /// Canonical JSON of everything that decides results, recorded with
    /// [`StrategyParams::version`].
    pub versioned_params: String,
}

fn seconds(value: u64, field: &str) -> Result<Duration, String> {
    if value > MAX_SECS {
        return Err(format!("{field} is above 30 days"));
    }
    i64::try_from(value)
        .map(Duration::seconds)
        .map_err(|_| format!("{field} is out of range"))
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
        let s = &v.strategy;
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
        let costs = CostModel {
            dex_fee_bps: v.costs.dex_fee_bps,
            network_fee_usd: v.costs.network_fee_usd,
            priority_fee_usd: v.costs.priority_fee_usd,
            failure_rate: v.costs.failure_rate,
        };
        if !costs.is_valid() {
            return Err("cost model is out of range".into());
        }
        let versioned_params = serde_json::to_string(&Versioned {
            stake_usd: &v.stake_usd,
            max_position_usd: &v.max_position_usd,
            max_open_positions: v.max_open_positions,
            filter: &v.filter,
            strategy: &v.strategy,
            costs: &v.costs,
        })
        .map_err(|error| format!("paper profile cannot be recorded: {error}"))?;
        Ok(Self {
            watchlist: file.watchlist,
            goal_multiple: file.goal_multiple,
            stake_usd: file.stake_usd,
            max_position_usd: file.max_position_usd,
            max_open_positions: file.max_open_positions,
            rules,
            strategy,
            costs,
            versioned_params,
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const MINT_A: &str = "So11111111111111111111111111111111111111112";
    pub(crate) const MINT_B: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

    /// Example values for tests only, not advice.
    pub(crate) fn fixture_json() -> serde_json::Value {
        serde_json::json!({
            "watchlist": [MINT_A, MINT_B],
            "goal_multiple": "2",
            "stake_usd": "250",
            "max_position_usd": "25",
            "max_open_positions": 3,
            "filter": {
                "min_liquidity_usd": "20000",
                "min_pool_age_secs": 3600,
                "max_top_holders_share": "0.3",
                "max_sell_impact_bps": 300,
                "allowed_extensions": ["metadata_pointer", "token_metadata"],
                "max_snapshot_age_secs": 60,
                "min_lp_lock_remaining_secs": 2592000
            },
            "strategy": {
                "version": 3, "min_buy_ratio_bps": 6000, "min_momentum_bps": 500,
                "max_momentum_bps": 5000, "position_share_bps": 10000,
                "max_liquidity_share_bps": 100, "take_profit_bps": 5000,
                "stop_loss_bps": 2500, "liquidity_drop_exit_bps": 4000,
                "max_hold_secs": 14400, "entry_slippage_bps": 150,
                "exit_slippage_bps": 800, "intent_ttl_secs": 20
            },
            "costs": {
                "dex_fee_bps": 25, "network_fee_usd": "0.001",
                "priority_fee_usd": "0.01", "failure_rate": "0.1"
            }
        })
    }

    pub(crate) fn fixture() -> PaperProfile {
        PaperProfile::parse(&fixture_json().to_string()).unwrap()
    }

    #[test]
    fn parses_and_records_canonical_params() {
        let profile = fixture();
        assert_eq!(profile.strategy.version, 3);
        assert_eq!(profile.rules.max_snapshot_age, Duration::minutes(1));
        // Formatting and the watchlist do not change the recorded version.
        let mut other = fixture_json();
        other["watchlist"] = serde_json::json!([MINT_B]);
        let reparsed = PaperProfile::parse(&serde_json::to_string_pretty(&other).unwrap()).unwrap();
        assert_eq!(reparsed.versioned_params, profile.versioned_params);
        let mut changed = fixture_json();
        changed["costs"]["dex_fee_bps"] = 30.into();
        assert_ne!(
            PaperProfile::parse(&changed.to_string())
                .unwrap()
                .versioned_params,
            profile.versioned_params
        );
    }

    #[test]
    fn load_refuses_symlinks_and_unprotected_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("profile.json");
        fs::write(&file, fixture_json().to_string()).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(PaperProfile::load(&file, false).is_ok());
        // Not root-owned, so production refuses it.
        assert!(PaperProfile::load(&file, true).is_err());
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(PaperProfile::load(&link, false).is_err());
        fs::write(&file, " ".repeat(65 * 1024)).unwrap();
        assert!(PaperProfile::load(&file, false).is_err());
    }

    #[test]
    fn refuses_unsafe_or_unknown_input() {
        let cases: [(&str, serde_json::Value); 9] = [
            ("/unknown", serde_json::json!(1)),
            ("/watchlist", serde_json::json!([])),
            (
                "/watchlist",
                serde_json::json!(["not-base58-0OIl-not-base58-0OIl-x"]),
            ),
            ("/watchlist", serde_json::json!([MINT_A, MINT_A])),
            ("/max_open_positions", serde_json::json!(11)),
            // 3 x 100 exceeds the 250 stake.
            ("/max_position_usd", serde_json::json!("100")),
            ("/goal_multiple", serde_json::json!("1")),
            ("/strategy/stop_loss_bps", serde_json::json!(1)),
            ("/costs/failure_rate", serde_json::json!("1.5")),
        ];
        for (pointer, value) in cases {
            let mut json = fixture_json();
            match json.pointer_mut(pointer) {
                Some(slot) => *slot = value,
                None => {
                    json["unknown"] = value;
                }
            }
            assert!(PaperProfile::parse(&json.to_string()).is_err(), "{pointer}");
        }
        let mut json = fixture_json();
        json["filter"]["allowed_extensions"] = serde_json::json!(["unknown"]);
        assert!(PaperProfile::parse(&json.to_string()).is_err());
    }
}
