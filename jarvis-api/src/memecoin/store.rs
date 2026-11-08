//! Typed repository for the memecoin paper trader (schemas 0014 and 0017).
//!
//! Paper only: these rows record what the simulator decided and what it
//! would have earned. None of them is an order or a limit. Money and token
//! amounts cross this boundary as `Decimal` and are stored as strings.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_onchain::backtest::{performance, Performance};
use jarvis_store::Database;

/// Candidate decisions and gate snapshots are kept this long.
pub const RETENTION_DAYS: u32 = 30;
/// Strategy-agent decisions are kept this long.
pub const DECISION_RETENTION_DAYS: u32 = 90;
/// Every observation is kept this long, then only the daily one.
pub const OBSERVATION_FULL_HOURS: u32 = 72;
/// Daily observations are kept this long.
pub const OBSERVATION_RETENTION_DAYS: u32 = 60;
/// Universe entries are kept this long after their migration.
pub const UNIVERSE_RETENTION_DAYS: u32 = 7;
/// Rows deleted per statement and statements per table per sweep.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;
/// Replay reads the whole paper ledger; refuse an implausibly large one
/// instead of loading it without bound.
pub const MAX_LEDGER_ROWS: usize = 100_000;
/// The most rows a listing returns.
pub const MAX_LIST: usize = 100;
/// A series for signals holds every observation of this many recent hours
/// (enough for the 24-hour signals) plus the daily ones before them ...
pub const SERIES_FULL_HOURS: i64 = 25;
/// ... and at most this many points, the newest kept: one a minute for
/// [`SERIES_FULL_HOURS`] plus [`OBSERVATION_RETENTION_DAYS`] daily ones fit.
pub const MAX_SERIES_POINTS: usize = 2_000;

/// Database detail is logged by the caller, never returned to clients.
#[derive(Debug)]
pub enum MemecoinStoreError {
    Database(Box<surrealdb::Error>),
    InvalidData,
    /// The strategy version is already recorded with other parameters.
    VersionConflict,
    /// The paper ledger exceeds [`MAX_LEDGER_ROWS`].
    TooLarge,
}

impl std::fmt::Display for MemecoinStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "memecoin paper database error: {error}"),
            Self::InvalidData => f.write_str("invalid memecoin paper row"),
            Self::VersionConflict => {
                f.write_str("strategy version already recorded with other parameters")
            }
            Self::TooLarge => write!(f, "memecoin paper ledger exceeds {MAX_LEDGER_ROWS} rows"),
        }
    }
}

impl std::error::Error for MemecoinStoreError {}

impl From<surrealdb::Error> for MemecoinStoreError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Database(Box::new(error))
    }
}

type Result<T> = std::result::Result<T, MemecoinStoreError>;

/// One filter decision. `reason` is `None` for an accepted candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub mint: String,
    pub observed_at: OffsetDateTime,
    pub strategy_version: u32,
    pub reason: Option<&'static str>,
    pub price_usd: Option<Decimal>,
    pub liquidity_usd: Option<Decimal>,
}

/// A stored candidate as listed back.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCandidate {
    pub mint: String,
    pub observed_at: OffsetDateTime,
    pub strategy_version: u32,
    pub reason: Option<String>,
    pub price_usd: Option<Decimal>,
    pub liquidity_usd: Option<Decimal>,
}

/// One lot, the position it forms and its round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct Trip {
    pub id: String,
    pub mint: String,
    pub strategy_version: u32,
    pub tokens: Decimal,
    /// Cost basis including entry fees.
    pub cost_usd: Decimal,
    pub entry_liquidity_usd: Decimal,
    pub opened_at: OffsetDateTime,
    /// Failed or unpriceable exit attempts so far.
    pub exit_failures: u32,
    /// Held, but exits stopped after [`Trip::exit_failures`] reached the cap.
    pub stuck: bool,
    /// Highest executable value seen while held, for the trailing stop.
    pub peak_value_usd: Option<Decimal>,
    pub close: Option<TripClose>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TripClose {
    pub closed_at: OffsetDateTime,
    pub usd_out: Decimal,
    pub exit_fees_usd: Decimal,
    pub net_pnl_usd: Decimal,
    pub exit_reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostKind {
    FailedTx,
    AiCost,
}

impl CostKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::FailedTx => "failed_tx",
            Self::AiCost => "ai_cost",
        }
    }
}

/// Costs without a fill (fees of failed attempts or attributed AI spend),
/// summed per strategy version, kind and UTC day. `at` is the latest one.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub kind: CostKind,
    pub usd: Decimal,
    pub at: OffsetDateTime,
    pub strategy_version: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateSnapshot {
    pub id: String,
    pub at: OffsetDateTime,
    pub strategy_version: u32,
    pub closed_trades: u64,
    pub duration_secs: u64,
    pub trading_days: u64,
    pub net_result_usd: Decimal,
    pub net_expectancy_usd: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Control {
    pub paused: bool,
    pub paused_at: Option<OffsetDateTime>,
    /// Device that paused.
    pub paused_by: Option<String>,
}

const CANDIDATE_REASONS: &[&str] = &[
    "data_unavailable",
    "stale_snapshot",
    "invalid_snapshot",
    "denied",
    "mint_authority_active",
    "freeze_authority_active",
    "unknown_extension",
    "extension_not_allowed",
    "extension_authority_active",
    "lp_not_secured",
    "lp_unlocks_too_soon",
    "liquidity_too_low",
    "pool_too_young",
    "holder_concentration_too_high",
    "no_sell_route",
    "sell_impact_too_high",
];
const EXIT_REASONS: &[&str] = &[
    "liquidity_drop",
    "stop_loss",
    "take_profit",
    "time_stop",
    "trailing_stop",
    "agent_exit",
];

fn rfc3339(at: OffsetDateTime) -> Result<String> {
    at.format(&Rfc3339)
        .map_err(|_| MemecoinStoreError::InvalidData)
}

fn decimal(value: &str) -> Result<Decimal> {
    Decimal::from_str(value).map_err(|_| MemecoinStoreError::InvalidData)
}

fn optional_decimal(value: Option<&str>) -> Result<Option<Decimal>> {
    value.map(decimal).transpose()
}

fn known(value: &str, set: &[&'static str]) -> Result<&'static str> {
    set.iter()
        .copied()
        .find(|known| *known == value)
        .ok_or(MemecoinStoreError::InvalidData)
}

async fn run(db: &Database, query: &str, bindings: Value) -> Result<surrealdb::Response> {
    Ok(db.query(query).bind(bindings).await?.check()?)
}

async fn rows<T: DeserializeOwned>(db: &Database, query: &str, bindings: Value) -> Result<Vec<T>> {
    Ok(run(db, query, bindings).await?.take(0)?)
}

/// A query on one record. The record id is bound on its own: inside a JSON
/// binding it would arrive as a plain object (and `type::thing` is gone in
/// SurrealDB 3).
async fn record_rows<T: DeserializeOwned>(
    db: &Database,
    query: &str,
    table: &str,
    key: &str,
    bindings: Value,
) -> Result<Vec<T>> {
    Ok(db
        .query(query)
        .bind(bindings)
        .bind(("rid", surrealdb::RecordId::from_table_key(table, key)))
        .await?
        .check()?
        .take(0)?)
}

pub async fn insert_candidate(db: &Database, candidate: &Candidate) -> Result<()> {
    if candidate
        .reason
        .is_some_and(|reason| !CANDIDATE_REASONS.contains(&reason))
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    run(
        db,
        "CREATE memecoin_candidates SET id = $id, mint = $mint, observed_at = <datetime>$observed_at, \
         strategy_version = $strategy_version, reason = $reason, price_usd = $price_usd, \
         liquidity_usd = $liquidity_usd RETURN NONE",
        json!({
            "id": candidate.id,
            "mint": candidate.mint,
            "observed_at": rfc3339(candidate.observed_at)?,
            "strategy_version": candidate.strategy_version,
            "reason": candidate.reason,
            "price_usd": candidate.price_usd.map(|v| v.to_string()),
            "liquidity_usd": candidate.liquidity_usd.map(|v| v.to_string()),
        }),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct CandidateRow {
    mint: String,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
    strategy_version: u32,
    reason: Option<String>,
    price_usd: Option<String>,
    liquidity_usd: Option<String>,
}

/// Newest decisions first, at most [`MAX_LIST`].
pub async fn recent_candidates(db: &Database, limit: usize) -> Result<Vec<StoredCandidate>> {
    let rows: Vec<CandidateRow> = rows(
        db,
        "SELECT mint, observed_at, strategy_version, reason, price_usd, liquidity_usd \
         FROM memecoin_candidates ORDER BY observed_at DESC LIMIT $limit",
        json!({"limit": limit.min(MAX_LIST)}),
    )
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(StoredCandidate {
                mint: row.mint,
                observed_at: row.observed_at,
                strategy_version: row.strategy_version,
                reason: row.reason,
                price_usd: optional_decimal(row.price_usd.as_deref())?,
                liquidity_usd: optional_decimal(row.liquidity_usd.as_deref())?,
            })
        })
        .collect()
}

/// Record `version` with its canonical parameter JSON, or confirm that it is
/// already recorded with exactly these parameters. A version never changes.
pub async fn register_strategy_version(db: &Database, version: u32, params: &str) -> Result<()> {
    let stored: Vec<String> = rows(
        db,
        "SELECT VALUE params FROM memecoin_strategy_versions WHERE version = $version",
        json!({"version": version}),
    )
    .await?;
    match stored.first() {
        Some(existing) if existing == params => Ok(()),
        Some(_) => Err(MemecoinStoreError::VersionConflict),
        None => {
            run(
                db,
                "CREATE memecoin_strategy_versions SET id = $id, version = $version, params = $params, \
                 created_at = time::now() RETURN NONE",
                json!({"id": version.to_string(), "version": version, "params": params}),
            )
            .await?;
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyFamily {
    A,
    B,
    C,
}

impl StrategyFamily {
    fn as_str(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            "c" => Ok(Self::C),
            _ => Err(MemecoinStoreError::InvalidData),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyAuthor {
    Seed,
    Claude,
}

impl StrategyAuthor {
    fn as_str(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Claude => "claude",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "seed" => Ok(Self::Seed),
            "claude" => Ok(Self::Claude),
            _ => Err(MemecoinStoreError::InvalidData),
        }
    }
}

/// A strategy version with its lineage. Never changes once recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct StrategyVersion {
    /// Unique across all strategies.
    pub version: u32,
    pub strategy_id: String,
    pub family: StrategyFamily,
    pub parent_version: Option<u32>,
    pub author: StrategyAuthor,
    /// Canonical parameter JSON.
    pub params: String,
    /// Display only: never fed into a prompt or a later task input.
    pub rationale: Option<String>,
    /// Canonical backtest JSON.
    pub backtest: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Strategy {
    pub id: String,
    pub active_version: u32,
    pub enabled: bool,
    /// Only the owner can enable it again.
    pub disabled_by_owner: bool,
    pub changed_at: OffsetDateTime,
    pub changed_by: String,
}

const MAX_RATIONALE: usize = 600;
const MAX_BACKTEST_BYTES: usize = 4096;
const MAX_PARAMS: usize = 8192;
const MAX_CHANGED_BY: usize = 64;

/// A base58 Solana address, as the schema checks it.
fn valid_address(address: &str) -> bool {
    (32..=44).contains(&address.len())
        && address
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() && !b"0OIl".contains(&b))
}

fn valid_strategy_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Bounded display text without control characters.
fn valid_text(text: &str, max_chars: usize) -> bool {
    text.chars().count() <= max_chars && !text.chars().any(char::is_control)
}

fn validate_version(version: &StrategyVersion) -> Result<()> {
    let valid = version.version >= 1
        && valid_strategy_id(&version.strategy_id)
        && version
            .parent_version
            .is_none_or(|parent| parent >= 1 && parent < version.version)
        && (2..=MAX_PARAMS).contains(&version.params.chars().count())
        && version
            .rationale
            .as_deref()
            .is_none_or(|text| valid_text(text, MAX_RATIONALE))
        && version.backtest.as_deref().is_none_or(|json| {
            (2..=MAX_BACKTEST_BYTES).contains(&json.len())
                && serde_json::from_str::<serde::de::IgnoredAny>(json).is_ok()
        });
    if valid {
        Ok(())
    } else {
        Err(MemecoinStoreError::InvalidData)
    }
}

#[derive(Deserialize)]
struct StrategyVersionRow {
    version: u32,
    strategy_id: Option<String>,
    family: Option<String>,
    parent_version: Option<u32>,
    author: Option<String>,
    params: String,
    rationale: Option<String>,
    backtest: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl TryFrom<StrategyVersionRow> for StrategyVersion {
    type Error = MemecoinStoreError;

    fn try_from(row: StrategyVersionRow) -> Result<Self> {
        let (Some(strategy_id), Some(family), Some(author)) =
            (row.strategy_id, row.family, row.author)
        else {
            // A legacy version belongs to no strategy.
            return Err(MemecoinStoreError::InvalidData);
        };
        Ok(Self {
            version: row.version,
            strategy_id,
            family: StrategyFamily::parse(&family)?,
            parent_version: row.parent_version,
            author: StrategyAuthor::parse(&author)?,
            params: row.params,
            rationale: row.rationale,
            backtest: row.backtest,
            created_at: row.created_at,
        })
    }
}

const VERSION_FIELDS: &str = "version, strategy_id, family, parent_version, author, params, \
    rationale, backtest, created_at";

/// Record a strategy version, or confirm that it is already recorded with
/// exactly this content (its `created_at` aside). A version never changes.
pub async fn insert_strategy_version(db: &Database, version: &StrategyVersion) -> Result<()> {
    validate_version(version)?;
    if let Some(parent) = version.parent_version {
        // A version derives only from a recorded version of its own strategy.
        if version_strategy(db, parent).await?.as_deref() != Some(version.strategy_id.as_str()) {
            return Err(MemecoinStoreError::InvalidData);
        }
    }
    let stored: Vec<StrategyVersionRow> = rows(
        db,
        &format!(
            "SELECT {VERSION_FIELDS} FROM memecoin_strategy_versions WHERE version = $version"
        ),
        json!({"version": version.version}),
    )
    .await?;
    if let Some(row) = stored.into_iter().next() {
        let existing =
            StrategyVersion::try_from(row).map_err(|_| MemecoinStoreError::VersionConflict)?;
        return if existing
            == (StrategyVersion {
                created_at: existing.created_at,
                ..version.clone()
            }) {
            Ok(())
        } else {
            Err(MemecoinStoreError::VersionConflict)
        };
    }
    run(
        db,
        "CREATE memecoin_strategy_versions SET id = $id, version = $version, params = $params, \
         created_at = <datetime>$created_at, strategy_id = $strategy_id, family = $family, \
         parent_version = $parent_version, author = $author, rationale = $rationale, \
         backtest = $backtest RETURN NONE",
        json!({
            "id": version.version.to_string(),
            "version": version.version,
            "params": version.params,
            "created_at": rfc3339(version.created_at)?,
            "strategy_id": version.strategy_id,
            "family": version.family.as_str(),
            "parent_version": version.parent_version,
            "author": version.author.as_str(),
            "rationale": version.rationale,
            "backtest": version.backtest,
        }),
    )
    .await?;
    Ok(())
}

/// Newest versions of one strategy first, at most [`MAX_LIST`].
pub async fn list_strategy_versions(
    db: &Database,
    strategy_id: &str,
    limit: usize,
) -> Result<Vec<StrategyVersion>> {
    let rows: Vec<StrategyVersionRow> = rows(
        db,
        &format!(
            "SELECT {VERSION_FIELDS} FROM memecoin_strategy_versions WHERE strategy_id = $strategy_id \
             ORDER BY version DESC LIMIT $limit"
        ),
        json!({"strategy_id": strategy_id, "limit": limit.min(MAX_LIST)}),
    )
    .await?;
    rows.into_iter().map(StrategyVersion::try_from).collect()
}

/// The strategy (`None` for a legacy version) and parameter JSON recorded
/// for `version`, if it is recorded.
pub async fn version_params(
    db: &Database,
    version: u32,
) -> Result<Option<(Option<String>, String)>> {
    #[derive(Deserialize)]
    struct Row {
        strategy_id: Option<String>,
        params: String,
    }
    let rows: Vec<Row> = rows(
        db,
        "SELECT strategy_id, params FROM memecoin_strategy_versions WHERE version = $version",
        json!({"version": version}),
    )
    .await?;
    Ok(rows
        .into_iter()
        .next()
        .map(|row| (row.strategy_id, row.params)))
}

/// The strategy `version` belongs to, if it is recorded with one.
async fn version_strategy(db: &Database, version: u32) -> Result<Option<String>> {
    let owners: Vec<Option<String>> = rows(
        db,
        "SELECT VALUE strategy_id FROM memecoin_strategy_versions WHERE version = $version",
        json!({"version": version}),
    )
    .await?;
    Ok(owners.into_iter().next().flatten())
}

/// Create a strategy that trades `active_version`, a version of its own.
/// `false` when the strategy already exists. At most [`MAX_LIST`] exist, so
/// [`list_strategies`] always returns all of them. Only the paper task writes
/// strategies, so the check and the write cannot race.
pub async fn insert_strategy(
    db: &Database,
    id: &str,
    active_version: u32,
    enabled: bool,
    changed_by: &str,
) -> Result<bool> {
    if !valid_strategy_id(id)
        || !(1..=MAX_CHANGED_BY).contains(&changed_by.chars().count())
        || version_strategy(db, active_version).await?.as_deref() != Some(id)
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    let existing: Vec<String> = record_rows(
        db,
        "SELECT VALUE record::id(id) FROM $rid",
        "memecoin_strategies",
        id,
        json!({}),
    )
    .await?;
    if !existing.is_empty() {
        return Ok(false);
    }
    if list_strategies(db).await?.len() >= MAX_LIST {
        return Err(MemecoinStoreError::InvalidData);
    }
    run(
        db,
        "CREATE memecoin_strategies SET id = $id, active_version = $active_version, \
         enabled = $enabled, disabled_by_owner = false, changed_at = time::now(), \
         changed_by = $changed_by RETURN NONE",
        json!({
            "id": id,
            "active_version": active_version,
            "enabled": enabled,
            "changed_by": changed_by,
        }),
    )
    .await?;
    Ok(true)
}

#[derive(Deserialize)]
struct StrategyRow {
    id: String,
    active_version: u32,
    enabled: bool,
    disabled_by_owner: bool,
    #[serde(with = "time::serde::rfc3339")]
    changed_at: OffsetDateTime,
    changed_by: String,
}

/// Every strategy by id.
pub async fn list_strategies(db: &Database) -> Result<Vec<Strategy>> {
    let rows: Vec<StrategyRow> = rows(
        db,
        "SELECT record::id(id) AS id, active_version, enabled, disabled_by_owner, changed_at, \
         changed_by FROM memecoin_strategies ORDER BY id LIMIT $limit",
        json!({"limit": MAX_LIST}),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Strategy {
            id: row.id,
            active_version: row.active_version,
            enabled: row.enabled,
            disabled_by_owner: row.disabled_by_owner,
            changed_at: row.changed_at,
            changed_by: row.changed_by,
        })
        .collect())
}

/// Switch a strategy to `version`, which must be one of its own. A strategy
/// the owner disabled keeps the owner's attribution. `false` when there is
/// no such strategy.
pub async fn set_active_version(
    db: &Database,
    id: &str,
    version: u32,
    changed_by: &str,
) -> Result<bool> {
    if !(1..=MAX_CHANGED_BY).contains(&changed_by.chars().count())
        || version_strategy(db, version).await?.as_deref() != Some(id)
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    let changed: Vec<String> = record_rows(
        db,
        "UPDATE $rid SET active_version = $version, \
         changed_at = IF disabled_by_owner THEN changed_at ELSE time::now() END, \
         changed_by = IF disabled_by_owner THEN changed_by ELSE $changed_by END \
         RETURN VALUE record::id(id)",
        "memecoin_strategies",
        id,
        json!({"version": version, "changed_by": changed_by}),
    )
    .await?;
    Ok(changed.len() == 1)
}

/// Enable or disable a strategy. Pass `owner = true` only for the owner's
/// own change: an enable only after the owner's device signature for it was
/// verified, a disable only from an authenticated owner session (an owner's
/// disable needs no signature). A strategy the owner disabled is only
/// changed by the owner, so it keeps the owner's disable and attribution. `false` when
/// nothing changed or there is no such strategy.
pub async fn set_strategy_enabled(
    db: &Database,
    id: &str,
    enabled: bool,
    owner: bool,
    changed_by: &str,
) -> Result<bool> {
    if !(1..=MAX_CHANGED_BY).contains(&changed_by.chars().count()) {
        return Err(MemecoinStoreError::InvalidData);
    }
    let changed: Vec<String> = record_rows(
        db,
        "UPDATE $rid SET enabled = $enabled, \
         disabled_by_owner = IF $owner THEN !$enabled ELSE disabled_by_owner END, \
         changed_at = time::now(), changed_by = $changed_by \
         WHERE $owner OR disabled_by_owner = false RETURN VALUE record::id(id)",
        "memecoin_strategies",
        id,
        json!({"enabled": enabled, "owner": owner, "changed_by": changed_by}),
    )
    .await?;
    Ok(changed.len() == 1)
}

pub async fn open_trip(db: &Database, trip: &Trip) -> Result<()> {
    if trip.close.is_some()
        || trip.stuck
        || trip.exit_failures != 0
        || trip.peak_value_usd.is_some()
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    run(
        db,
        "CREATE memecoin_paper_trips SET id = $id, slot = $mint, mint = $mint, strategy_version = $strategy_version, \
         status = 'open', tokens = $tokens, cost_usd = $cost_usd, entry_liquidity_usd = $entry_liquidity_usd, \
         exit_failures = 0, opened_at = <datetime>$opened_at, closed_at = NONE, usd_out = NONE, exit_fees_usd = NONE, \
         net_pnl_usd = NONE, exit_reason = NONE RETURN NONE",
        json!({
            "id": trip.id,
            "mint": trip.mint,
            "strategy_version": trip.strategy_version,
            "tokens": trip.tokens.to_string(),
            "cost_usd": trip.cost_usd.to_string(),
            "entry_liquidity_usd": trip.entry_liquidity_usd.to_string(),
            "opened_at": rfc3339(trip.opened_at)?,
        }),
    )
    .await?;
    Ok(())
}

/// Close an open trip. `false` when no open trip has this id.
pub async fn close_trip(db: &Database, id: &str, close: &TripClose) -> Result<bool> {
    if !EXIT_REASONS.contains(&close.exit_reason) {
        return Err(MemecoinStoreError::InvalidData);
    }
    let closed: Vec<String> = record_rows(
        db,
        "UPDATE $rid SET status = 'closed', slot = $id, closed_at = <datetime>$closed_at, \
         usd_out = $usd_out, exit_fees_usd = $exit_fees_usd, net_pnl_usd = $net_pnl_usd, \
         exit_reason = $exit_reason WHERE status = 'open' RETURN VALUE record::id(id)",
        "memecoin_paper_trips",
        id,
        json!({
            "id": id,
            "closed_at": rfc3339(close.closed_at)?,
            "usd_out": close.usd_out.to_string(),
            "exit_fees_usd": close.exit_fees_usd.to_string(),
            "net_pnl_usd": close.net_pnl_usd.to_string(),
            "exit_reason": close.exit_reason,
        }),
    )
    .await?;
    Ok(closed.len() == 1)
}

/// Count a failed or unpriceable exit of an open trip. At `cap` failures the
/// trip becomes stuck and is no longer retried. Returns whether it is now
/// stuck, or `None` when no open trip has this id.
pub async fn record_exit_failure(db: &Database, id: &str, cap: u32) -> Result<Option<bool>> {
    let status: Vec<String> = record_rows(
        db,
        "UPDATE $rid SET exit_failures += 1, \
         status = IF exit_failures >= $cap THEN 'stuck' ELSE 'open' END \
         WHERE status = 'open' RETURN VALUE status",
        "memecoin_paper_trips",
        id,
        json!({"cap": cap}),
    )
    .await?;
    Ok(status.first().map(|status| status == "stuck"))
}

/// Store a new peak value of an open trip. `false` when no open trip has
/// this id.
pub async fn set_peak_value(db: &Database, id: &str, peak_value_usd: Decimal) -> Result<bool> {
    if peak_value_usd < Decimal::ZERO {
        return Err(MemecoinStoreError::InvalidData);
    }
    let changed: Vec<String> = record_rows(
        db,
        "UPDATE $rid SET peak_value_usd = $peak WHERE status = 'open' RETURN VALUE record::id(id)",
        "memecoin_paper_trips",
        id,
        json!({"peak": peak_value_usd.to_string()}),
    )
    .await?;
    Ok(changed.len() == 1)
}

#[derive(Deserialize)]
struct TripRow {
    id: String,
    mint: String,
    strategy_version: u32,
    tokens: String,
    cost_usd: String,
    entry_liquidity_usd: String,
    status: String,
    exit_failures: u32,
    #[serde(with = "time::serde::rfc3339")]
    opened_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    closed_at: Option<OffsetDateTime>,
    usd_out: Option<String>,
    exit_fees_usd: Option<String>,
    net_pnl_usd: Option<String>,
    exit_reason: Option<String>,
    peak_value_usd: Option<String>,
}

impl TryFrom<TripRow> for Trip {
    type Error = MemecoinStoreError;

    fn try_from(row: TripRow) -> Result<Self> {
        let close = match (
            row.closed_at,
            row.usd_out,
            row.exit_fees_usd,
            row.net_pnl_usd,
            row.exit_reason,
        ) {
            (None, None, None, None, None) => None,
            (Some(closed_at), Some(usd_out), Some(fees), Some(pnl), Some(reason)) => {
                Some(TripClose {
                    closed_at,
                    usd_out: decimal(&usd_out)?,
                    exit_fees_usd: decimal(&fees)?,
                    net_pnl_usd: decimal(&pnl)?,
                    exit_reason: known(&reason, EXIT_REASONS)?,
                })
            }
            _ => return Err(MemecoinStoreError::InvalidData),
        };
        Ok(Self {
            id: row.id,
            mint: row.mint,
            strategy_version: row.strategy_version,
            tokens: decimal(&row.tokens)?,
            cost_usd: decimal(&row.cost_usd)?,
            entry_liquidity_usd: decimal(&row.entry_liquidity_usd)?,
            opened_at: row.opened_at,
            exit_failures: row.exit_failures,
            peak_value_usd: optional_decimal(row.peak_value_usd.as_deref())?,
            stuck: match (row.status.as_str(), &close) {
                ("stuck", None) => true,
                ("open", None) | ("closed", Some(_)) => false,
                _ => return Err(MemecoinStoreError::InvalidData),
            },
            close,
        })
    }
}

const TRIP_FIELDS: &str = "record::id(id) AS id, mint, strategy_version, tokens, cost_usd, \
    entry_liquidity_usd, status, exit_failures, opened_at, closed_at, usd_out, exit_fees_usd, net_pnl_usd, exit_reason, \
    peak_value_usd";

fn bounded<T>(rows: Vec<T>) -> Result<Vec<T>> {
    if rows.len() > MAX_LEDGER_ROWS {
        return Err(MemecoinStoreError::TooLarge);
    }
    Ok(rows)
}

/// Every trip, oldest first, for rebuilding the ledger.
pub async fn load_trips(db: &Database) -> Result<Vec<Trip>> {
    let rows: Vec<TripRow> = rows(
        db,
        &format!(
            "SELECT {TRIP_FIELDS} FROM memecoin_paper_trips ORDER BY opened_at LIMIT {}",
            MAX_LEDGER_ROWS + 1
        ),
        json!({}),
    )
    .await?;
    bounded(rows)?.into_iter().map(Trip::try_from).collect()
}

/// Positions still held (open or stuck), oldest first. Paper holds at most
/// one per mint and opens at most 10, so 64 bounds even a lowered limit.
pub async fn load_held_trips(db: &Database) -> Result<Vec<Trip>> {
    let rows: Vec<TripRow> = rows(
        db,
        &format!(
            "SELECT {TRIP_FIELDS} FROM memecoin_paper_trips WHERE status != 'closed' \
             ORDER BY opened_at LIMIT 64"
        ),
        json!({}),
    )
    .await?;
    rows.into_iter().map(Trip::try_from).collect()
}

/// Newest closed trips first, at most [`MAX_LIST`].
pub async fn recent_closed_trips(db: &Database, limit: usize) -> Result<Vec<Trip>> {
    let rows: Vec<TripRow> = rows(
        db,
        &format!(
            "SELECT {TRIP_FIELDS} FROM memecoin_paper_trips WHERE status = 'closed' \
             ORDER BY closed_at DESC LIMIT $limit"
        ),
        json!({"limit": limit.min(MAX_LIST)}),
    )
    .await?;
    rows.into_iter().map(Trip::try_from).collect()
}

#[derive(Deserialize)]
struct CostRow {
    kind: String,
    usd: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    strategy_version: u32,
}

/// Add `usd` to the day's cost row of `kind`. Only the paper task writes
/// costs, so the read-modify-write cannot race.
pub async fn add_cost(
    db: &Database,
    kind: CostKind,
    strategy_version: u32,
    usd: Decimal,
    at: OffsetDateTime,
) -> Result<()> {
    if usd < Decimal::ZERO {
        return Err(MemecoinStoreError::InvalidData);
    }
    let key = format!("{strategy_version}-{}-{}", kind.as_str(), at.date());
    let stored: Vec<String> = record_rows(
        db,
        "SELECT VALUE usd FROM $rid",
        "memecoin_paper_costs",
        &key,
        json!({}),
    )
    .await?;
    let total = match stored.first() {
        Some(existing) => decimal(existing)?
            .checked_add(usd)
            .ok_or(MemecoinStoreError::InvalidData)?,
        None => usd,
    };
    record_rows::<serde::de::IgnoredAny>(
        db,
        "UPSERT $rid SET id = $id, kind = $kind, usd = $usd, at = <datetime>$at, \
         strategy_version = $strategy_version RETURN NONE",
        "memecoin_paper_costs",
        &key,
        json!({
            "id": key,
            "kind": kind.as_str(),
            "usd": total.to_string(),
            "at": rfc3339(at)?,
            "strategy_version": strategy_version,
        }),
    )
    .await?;
    Ok(())
}

/// Every cost row, oldest first, for rebuilding the ledger.
pub async fn load_costs(db: &Database) -> Result<Vec<Cost>> {
    let rows: Vec<CostRow> = rows(
        db,
        &format!(
            "SELECT kind, usd, at, strategy_version FROM memecoin_paper_costs ORDER BY at LIMIT {}",
            MAX_LEDGER_ROWS + 1
        ),
        json!({}),
    )
    .await?;
    bounded(rows)?
        .into_iter()
        .map(|row| {
            Ok(Cost {
                kind: match row.kind.as_str() {
                    "failed_tx" => CostKind::FailedTx,
                    "ai_cost" => CostKind::AiCost,
                    _ => return Err(MemecoinStoreError::InvalidData),
                },
                usd: decimal(&row.usd)?,
                at: row.at,
                strategy_version: row.strategy_version,
            })
        })
        .collect()
}

pub async fn insert_gate_snapshot(db: &Database, snapshot: &GateSnapshot) -> Result<()> {
    run(
        db,
        "CREATE memecoin_gate_snapshots SET id = $id, at = <datetime>$at, strategy_version = $strategy_version, \
         closed_trades = $closed_trades, duration_secs = $duration_secs, trading_days = $trading_days, \
         net_result_usd = $net_result_usd, net_expectancy_usd = $net_expectancy_usd RETURN NONE",
        json!({
            "id": snapshot.id,
            "at": rfc3339(snapshot.at)?,
            "strategy_version": snapshot.strategy_version,
            "closed_trades": snapshot.closed_trades,
            "duration_secs": snapshot.duration_secs,
            "trading_days": snapshot.trading_days,
            "net_result_usd": snapshot.net_result_usd.to_string(),
            "net_expectancy_usd": snapshot.net_expectancy_usd.to_string(),
        }),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct ControlRow {
    paused: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    paused_at: Option<OffsetDateTime>,
    paused_by: Option<String>,
}

/// The owner's pause. No row means not paused.
pub async fn control(db: &Database) -> Result<Control> {
    let rows: Vec<ControlRow> = rows(
        db,
        "SELECT paused, paused_at, paused_by FROM memecoin_paper_control:paper",
        json!({}),
    )
    .await?;
    Ok(rows
        .into_iter()
        .next()
        .map_or_else(Control::default, |row| Control {
            paused: row.paused,
            paused_at: row.paused_at,
            paused_by: row.paused_by,
        }))
}

/// Pause paper entries. Idempotent: an existing pause keeps its time.
pub async fn pause(db: &Database, device_id: &str) -> Result<()> {
    run(
        db,
        "UPSERT memecoin_paper_control:paper SET id = 'paper', \
         paused_by = IF paused = true THEN paused_by ELSE $device END, \
         paused_at = IF paused = true THEN paused_at ELSE time::now() END, paused = true RETURN NONE",
        json!({"device": device_id}),
    )
    .await?;
    Ok(())
}

/// Clear the pause made at exactly `paused_at`, so a resume setting can
/// never clear a later pause. `true` when that pause was cleared.
pub async fn resume(db: &Database, paused_at: OffsetDateTime) -> Result<bool> {
    let cleared: Vec<String> = rows(
        db,
        "UPDATE memecoin_paper_control:paper SET paused = false, paused_at = NONE, paused_by = NONE \
         WHERE paused = true AND paused_at = <datetime>$paused_at RETURN VALUE record::id(id)",
        json!({"paused_at": rfc3339(paused_at)?}),
    )
    .await?;
    Ok(cleared.len() == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionKind {
    Enter,
    Skip,
    Exit,
    Hold,
    Propose,
    WorkerUnavailable,
    InvalidOutput,
}

impl DecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Skip => "skip",
            Self::Exit => "exit",
            Self::Hold => "hold",
            Self::Propose => "propose",
            Self::WorkerUnavailable => "worker_unavailable",
            Self::InvalidOutput => "invalid_output",
        }
    }
}

/// One decision of the strategy agent, or why it could not decide.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub at: OffsetDateTime,
    pub call_id: String,
    pub kind: DecisionKind,
    pub strategy_version: Option<u32>,
    pub mint: Option<String>,
    /// Share of the strategy's budget; only on `enter`.
    pub size_bps: Option<u32>,
    /// snake_case, at most 32 characters.
    pub reason_code: Option<String>,
    /// Display only: never fed into a prompt or a later task input.
    pub rationale: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

const MAX_DECISION_RATIONALE: usize = 300;
const MAX_TOKENS: u64 = 100_000_000;

fn valid_decision(decision: &Decision) -> bool {
    let enter = decision.kind == DecisionKind::Enter;
    let id_like = |value: &str, extra: &[u8]| {
        (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || extra.contains(&b))
    };
    id_like(&decision.call_id, b"")
        && decision.strategy_version.is_none_or(|v| v >= 1)
        && enter == decision.size_bps.is_some()
        && decision
            .size_bps
            .is_none_or(|bps| (1..=10_000).contains(&bps))
        && (!matches!(decision.kind, DecisionKind::Enter | DecisionKind::Exit)
            || decision.mint.is_some())
        && decision.mint.as_deref().is_none_or(valid_address)
        && decision.reason_code.as_deref().is_none_or(|code| {
            (1..=32).contains(&code.len())
                && code.starts_with(|c: char| c.is_ascii_lowercase())
                && code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        && decision
            .rationale
            .as_deref()
            .is_none_or(|text| valid_text(text, MAX_DECISION_RATIONALE))
        && decision
            .model
            .as_deref()
            .is_none_or(|model| id_like(model, b".:"))
        && decision.input_tokens.is_none_or(|n| n <= MAX_TOKENS)
        && decision.output_tokens.is_none_or(|n| n <= MAX_TOKENS)
}

pub async fn append_decision(db: &Database, decision: &Decision) -> Result<()> {
    if !valid_decision(decision) {
        return Err(MemecoinStoreError::InvalidData);
    }
    run(
        db,
        "CREATE memecoin_decisions SET id = $id, at = <datetime>$at, call_id = $call_id, kind = $kind, \
         strategy_version = $strategy_version, mint = $mint, size_bps = $size_bps, \
         reason_code = $reason_code, rationale = $rationale, model = $model, \
         input_tokens = $input_tokens, output_tokens = $output_tokens RETURN NONE",
        json!({
            "id": uuid::Uuid::now_v7().to_string(),
            "at": rfc3339(decision.at)?,
            "call_id": decision.call_id,
            "kind": decision.kind.as_str(),
            "strategy_version": decision.strategy_version,
            "mint": decision.mint,
            "size_bps": decision.size_bps,
            "reason_code": decision.reason_code,
            "rationale": decision.rationale,
            "model": decision.model,
            "input_tokens": decision.input_tokens,
            "output_tokens": decision.output_tokens,
        }),
    )
    .await?;
    Ok(())
}

/// Price and liquidity of a mint at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    pub mint: String,
    pub at: OffsetDateTime,
    pub price_usd: Decimal,
    pub liquidity_usd: Decimal,
}

/// The record key of the observation kept for `mint` on its UTC day.
fn daily_key(mint: &str, at: OffsetDateTime) -> String {
    format!("{mint}-{}", at.to_offset(time::UtcOffset::UTC).date())
}

/// Record an observation. The first one of each mint and UTC day is the
/// daily one that outlives [`OBSERVATION_FULL_HOURS`]. Only the paper task
/// writes observations, so the check and the write cannot race.
pub async fn record_observation(db: &Database, observation: &Observation) -> Result<()> {
    if !valid_address(&observation.mint)
        || observation.price_usd < Decimal::ZERO
        || observation.liquidity_usd < Decimal::ZERO
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    let day = daily_key(&observation.mint, observation.at);
    let stored: Vec<String> = record_rows(
        db,
        "SELECT VALUE record::id(id) FROM $rid",
        "memecoin_observations",
        &day,
        json!({}),
    )
    .await?;
    let (key, daily) = if stored.is_empty() {
        (day, true)
    } else {
        (uuid::Uuid::now_v7().to_string(), false)
    };
    record_rows::<serde::de::IgnoredAny>(
        db,
        "CREATE $rid SET id = $id, mint = $mint, at = <datetime>$at, price_usd = $price_usd, \
         liquidity_usd = $liquidity_usd, daily = $daily RETURN NONE",
        "memecoin_observations",
        &key,
        json!({
            "id": key,
            "mint": observation.mint,
            "at": rfc3339(observation.at)?,
            "price_usd": observation.price_usd.to_string(),
            "liquidity_usd": observation.liquidity_usd.to_string(),
            "daily": daily,
        }),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct ObservationRow {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    price_usd: String,
    liquidity_usd: String,
}

/// The observations of `mint` up to `now` that signals read, oldest first:
/// all of the last [`SERIES_FULL_HOURS`] and the daily ones before; the
/// newest `limit`, at most [`MAX_SERIES_POINTS`].
pub async fn load_series(
    db: &Database,
    mint: &str,
    now: OffsetDateTime,
    limit: usize,
) -> Result<Vec<Observation>> {
    let rows: Vec<ObservationRow> = rows(
        db,
        "SELECT at, price_usd, liquidity_usd FROM memecoin_observations \
         WHERE mint = $mint AND at <= <datetime>$now AND (daily = true OR at >= <datetime>$since) \
         ORDER BY at DESC LIMIT $limit",
        json!({
            "mint": mint,
            "now": rfc3339(now)?,
            "since": rfc3339(now - time::Duration::hours(SERIES_FULL_HOURS))?,
            "limit": limit.min(MAX_SERIES_POINTS),
        }),
    )
    .await?;
    rows.into_iter()
        .rev()
        .map(|row| {
            Ok(Observation {
                mint: mint.to_owned(),
                at: row.at,
                price_usd: decimal(&row.price_usd)?,
                liquidity_usd: decimal(&row.liquidity_usd)?,
            })
        })
        .collect()
}

/// A recently migrated token discovery may consider.
#[derive(Debug, Clone, PartialEq)]
pub struct UniverseEntry {
    pub mint: String,
    pub pool: String,
    pub program: String,
    pub migrated_at: OffsetDateTime,
}

/// Record or replace the universe entry of a mint.
pub async fn upsert_universe(db: &Database, entry: &UniverseEntry) -> Result<()> {
    if ![&entry.mint, &entry.pool, &entry.program]
        .iter()
        .all(|address| valid_address(address))
    {
        return Err(MemecoinStoreError::InvalidData);
    }
    record_rows::<serde::de::IgnoredAny>(
        db,
        "UPSERT $rid SET id = $mint, mint = $mint, pool = $pool, program = $program, \
         migrated_at = <datetime>$migrated_at RETURN NONE",
        "memecoin_universe",
        &entry.mint,
        json!({
            "mint": entry.mint,
            "pool": entry.pool,
            "program": entry.program,
            "migrated_at": rfc3339(entry.migrated_at)?,
        }),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct UniverseRow {
    mint: String,
    pool: String,
    program: String,
    #[serde(with = "time::serde::rfc3339")]
    migrated_at: OffsetDateTime,
}

/// Universe entries migrated at or after `since`, newest first, at most
/// `limit`.
pub async fn load_universe(
    db: &Database,
    since: OffsetDateTime,
    limit: usize,
) -> Result<Vec<UniverseEntry>> {
    let rows: Vec<UniverseRow> = rows(
        db,
        "SELECT mint, pool, program, migrated_at FROM memecoin_universe \
         WHERE migrated_at >= <datetime>$since ORDER BY migrated_at DESC LIMIT $limit",
        json!({"since": rfc3339(since)?, "limit": limit}),
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| UniverseEntry {
            mint: row.mint,
            pool: row.pool,
            program: row.program,
            migrated_at: row.migrated_at,
        })
        .collect())
}

/// How one strategy did in paper, over all of its versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrategyPerformance {
    /// Closed round trips, in close order.
    pub closed: Performance,
    /// Fees of failed attempts and AI cost recorded for its versions.
    pub costs_usd: Decimal,
    /// `closed.net_usd` minus `costs_usd`.
    pub net_usd: Decimal,
}

/// Trades, net result, expectancy, win rate and drawdown of one strategy.
/// Open positions are left out.
pub async fn strategy_performance(db: &Database, strategy_id: &str) -> Result<StrategyPerformance> {
    #[derive(Deserialize)]
    struct ClosedRow {
        net_pnl_usd: String,
    }
    if !valid_strategy_id(strategy_id) {
        return Err(MemecoinStoreError::InvalidData);
    }
    let mut response = run(
        db,
        &format!(
            "LET $versions = (SELECT VALUE version FROM memecoin_strategy_versions \
             WHERE strategy_id = $strategy_id); \
             SELECT net_pnl_usd, closed_at FROM memecoin_paper_trips \
             WHERE status = 'closed' AND strategy_version IN $versions ORDER BY closed_at LIMIT {limit}; \
             SELECT VALUE usd FROM memecoin_paper_costs WHERE strategy_version IN $versions LIMIT {limit};",
            limit = MAX_LEDGER_ROWS + 1
        ),
        json!({"strategy_id": strategy_id}),
    )
    .await?;
    let closed: Vec<ClosedRow> = bounded(response.take(1)?)?;
    let costs: Vec<String> = bounded(response.take(2)?)?;
    let pnls = closed
        .iter()
        .map(|row| decimal(&row.net_pnl_usd))
        .collect::<Result<Vec<_>>>()?;
    let costs_usd = costs.iter().try_fold(Decimal::ZERO, |sum, usd| {
        sum.checked_add(decimal(usd)?)
            .ok_or(MemecoinStoreError::InvalidData)
    })?;
    let closed = performance(pnls.into_iter()).map_err(|_| MemecoinStoreError::InvalidData)?;
    Ok(StrategyPerformance {
        net_usd: closed
            .net_usd
            .checked_sub(costs_usd)
            .ok_or(MemecoinStoreError::InvalidData)?,
        closed,
        costs_usd,
    })
}

/// Delete, in bounded batches: candidate decisions and gate snapshots after
/// [`RETENTION_DAYS`], agent decisions after [`DECISION_RETENTION_DAYS`],
/// observations after [`OBSERVATION_FULL_HOURS`] except the daily ones, which
/// go after [`OBSERVATION_RETENTION_DAYS`], and universe entries
/// [`UNIVERSE_RETENTION_DAYS`] after their migration. The paper ledger itself
/// (trips and costs) and the strategies are kept: the paper gate needs their
/// whole history.
pub async fn prune(db: &Database) -> Result<usize> {
    let mut total = 0;
    for (table, expired) in [
        (
            "memecoin_candidates",
            format!("observed_at < time::now() - {RETENTION_DAYS}d"),
        ),
        (
            "memecoin_gate_snapshots",
            format!("at < time::now() - {RETENTION_DAYS}d"),
        ),
        (
            "memecoin_decisions",
            format!("at < time::now() - {DECISION_RETENTION_DAYS}d"),
        ),
        (
            "memecoin_observations",
            format!("at < time::now() - {OBSERVATION_RETENTION_DAYS}d"),
        ),
        (
            "memecoin_observations",
            format!("daily = false AND at < time::now() - {OBSERVATION_FULL_HOURS}h"),
        ),
        (
            "memecoin_universe",
            format!("migrated_at < time::now() - {UNIVERSE_RETENTION_DAYS}d"),
        ),
    ] {
        let query = format!(
            "LET $ids = (SELECT VALUE id FROM {table} WHERE {expired} LIMIT {PRUNE_BATCH}); \
             DELETE $ids RETURN NONE; RETURN array::len($ids);"
        );
        for _ in 0..PRUNE_MAX_BATCHES {
            let deleted: Option<usize> = db.query(query.as_str()).await?.check()?.take(2)?;
            let deleted = deleted.unwrap_or(0);
            total += deleted;
            if deleted < PRUNE_BATCH {
                break;
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
    use time::Duration;

    use super::*;

    const MINT: &str = "So11111111111111111111111111111111111111112";

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn paper_repository_round_trips_and_prunes(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memecoin_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let now = OffsetDateTime::now_utc();

        register_strategy_version(&db, 3, r#"{"a":1}"#).await?;
        register_strategy_version(&db, 3, r#"{"a":1}"#).await?;
        assert!(matches!(
            register_strategy_version(&db, 3, r#"{"a":2}"#).await,
            Err(MemecoinStoreError::VersionConflict)
        ));

        for (id, at, reason) in [
            ("old", now - Duration::days(31), Some("lp_not_secured")),
            ("new", now, None),
        ] {
            insert_candidate(
                &db,
                &Candidate {
                    id: id.into(),
                    mint: MINT.into(),
                    observed_at: at,
                    strategy_version: 3,
                    reason,
                    price_usd: Some(d("0.000012345")),
                    liquidity_usd: None,
                },
            )
            .await?;
        }
        let listed = recent_candidates(&db, 10).await?;
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].reason, None);
        assert_eq!(listed[1].reason.as_deref(), Some("lp_not_secured"));
        assert_eq!(listed[0].price_usd, Some(d("0.000012345")));

        let trip = Trip {
            id: "t1".into(),
            mint: MINT.into(),
            strategy_version: 3,
            tokens: d("123456.000000000001"),
            cost_usd: d("25.011"),
            entry_liquidity_usd: d("80000"),
            opened_at: now - Duration::hours(2),
            exit_failures: 0,
            stuck: false,
            peak_value_usd: None,
            close: None,
        };
        open_trip(&db, &trip).await?;
        assert!(open_trip(
            &db,
            &Trip {
                id: "t2".into(),
                ..trip.clone()
            }
        )
        .await
        .is_err());
        let close = TripClose {
            closed_at: now - Duration::hours(1),
            usd_out: d("40"),
            exit_fees_usd: d("0.011"),
            net_pnl_usd: d("14.978"),
            exit_reason: "take_profit",
        };
        assert!(close_trip(&db, "t1", &close).await?);
        assert!(!close_trip(&db, "t1", &close).await?);
        open_trip(
            &db,
            &Trip {
                id: "t2".into(),
                opened_at: now,
                ..trip.clone()
            },
        )
        .await?;
        let trips = load_trips(&db).await?;
        assert_eq!(trips.len(), 2);
        assert_eq!(trips[0].close, Some(close.clone()));
        assert_eq!(trips[0].tokens, trip.tokens);
        assert_eq!(trips[1].close, None);
        assert_eq!(recent_closed_trips(&db, 10).await?.len(), 1);

        // Exit failures count up to the cap, then the trip is stuck: still
        // held, never closed or counted again.
        assert_eq!(record_exit_failure(&db, "t2", 2).await?, Some(false));
        assert_eq!(record_exit_failure(&db, "t2", 2).await?, Some(true));
        assert_eq!(record_exit_failure(&db, "t2", 2).await?, None);
        assert!(!close_trip(&db, "t2", &close).await?);
        let stuck = &load_trips(&db).await?[1];
        assert!(stuck.stuck && stuck.exit_failures == 2 && stuck.close.is_none());
        assert_eq!(record_exit_failure(&db, "missing", 2).await?, None);

        // Costs add up in one row per version, kind and day.
        for usd in ["0.011", "0.004"] {
            add_cost(&db, CostKind::FailedTx, 3, d(usd), now).await?;
        }
        add_cost(&db, CostKind::FailedTx, 3, d("1"), now - Duration::days(2)).await?;
        let costs = load_costs(&db).await?;
        assert_eq!(costs.len(), 2);
        assert_eq!(costs[1].usd, d("0.015"));
        assert_eq!(costs[1].kind, CostKind::FailedTx);

        for (id, at) in [("g-old", now - Duration::days(40)), ("g-new", now)] {
            insert_gate_snapshot(
                &db,
                &GateSnapshot {
                    id: id.into(),
                    at,
                    strategy_version: 3,
                    closed_trades: 1,
                    duration_secs: 3_600,
                    trading_days: 1,
                    net_result_usd: d("-0.5"),
                    net_expectancy_usd: d("-0.5"),
                },
            )
            .await?;
        }

        assert_eq!(control(&db).await?, Control::default());
        pause(&db, "device-1").await?;
        let paused = control(&db).await?;
        assert!(paused.paused);
        pause(&db, "device-2").await?;
        assert_eq!(
            control(&db).await?,
            paused,
            "an existing pause keeps its time"
        );
        assert_eq!(paused.paused_by.as_deref(), Some("device-1"));
        let paused_at = paused.paused_at.ok_or("pause has no time")?;
        assert!(!resume(&db, paused_at - Duration::seconds(1)).await?);
        assert!(resume(&db, paused_at).await?);
        assert!(!control(&db).await?.paused);

        assert_eq!(prune(&db).await?, 2);
        assert_eq!(recent_candidates(&db, 10).await?.len(), 1);
        // The ledger is never pruned.
        assert_eq!(load_trips(&db).await?.len(), 2);
        Ok(())
    }

    const OTHER: &str = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";

    fn seed(at: OffsetDateTime) -> StrategyVersion {
        StrategyVersion {
            version: 1,
            strategy_id: "momentum-a".into(),
            family: StrategyFamily::A,
            parent_version: None,
            author: StrategyAuthor::Seed,
            params: r#"{"a":1}"#.into(),
            rationale: None,
            backtest: None,
            created_at: at,
        }
    }

    fn enter(at: OffsetDateTime) -> Decision {
        Decision {
            at,
            call_id: "0193f0c2-7a1b-7c3d-8e4f-5a6b7c8d9e0f".into(),
            kind: DecisionKind::Enter,
            strategy_version: Some(3),
            mint: Some(MINT.into()),
            size_bps: Some(2_500),
            reason_code: Some("volume_spike".into()),
            rationale: Some("Volume rose.".into()),
            model: Some("claude-sonnet-4-5".into()),
            input_tokens: Some(900),
            output_tokens: Some(120),
        }
    }

    #[test]
    fn strategy_versions_are_checked_before_the_database() {
        let good = StrategyVersion {
            version: 5,
            parent_version: Some(4),
            rationale: Some("é".repeat(600)),
            backtest: Some(format!("\"{}\"", "x".repeat(4094))),
            ..seed(OffsetDateTime::UNIX_EPOCH)
        };
        assert!(validate_version(&good).is_ok());
        for bad in [
            StrategyVersion {
                strategy_id: "Momentum".into(),
                ..good.clone()
            },
            StrategyVersion {
                strategy_id: String::new(),
                ..good.clone()
            },
            StrategyVersion {
                strategy_id: "a".repeat(33),
                ..good.clone()
            },
            StrategyVersion {
                parent_version: Some(5),
                ..good.clone()
            },
            StrategyVersion {
                params: "{".into(),
                ..good.clone()
            },
            StrategyVersion {
                rationale: Some("é".repeat(601)),
                ..good.clone()
            },
            StrategyVersion {
                rationale: Some("line\nbreak".into()),
                ..good.clone()
            },
            StrategyVersion {
                backtest: Some("{trades: 3}".into()),
                ..good.clone()
            },
            // 2050 characters, but 4098 bytes.
            StrategyVersion {
                backtest: Some(format!("\"{}\"", "é".repeat(2048))),
                ..good.clone()
            },
        ] {
            assert!(validate_version(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn decisions_are_checked_before_the_database() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let hold = Decision {
            kind: DecisionKind::Hold,
            mint: None,
            size_bps: None,
            ..enter(now)
        };
        assert!(valid_decision(&enter(now)));
        assert!(valid_decision(&hold));
        for bad in [
            Decision {
                size_bps: None,
                ..enter(now)
            },
            Decision {
                size_bps: Some(10_001),
                ..enter(now)
            },
            Decision {
                mint: None,
                ..enter(now)
            },
            Decision {
                kind: DecisionKind::Exit,
                size_bps: None,
                mint: None,
                ..enter(now)
            },
            Decision {
                size_bps: Some(100),
                ..hold.clone()
            },
            Decision {
                call_id: "call 1".into(),
                ..hold.clone()
            },
            Decision {
                reason_code: Some("Volume-Spike".into()),
                ..hold.clone()
            },
            Decision {
                reason_code: Some("a".repeat(33)),
                ..hold.clone()
            },
            Decision {
                rationale: Some("r".repeat(301)),
                ..hold.clone()
            },
            Decision {
                model: Some("model/../x".into()),
                ..hold.clone()
            },
            Decision {
                input_tokens: Some(MAX_TOKENS + 1),
                ..hold.clone()
            },
            Decision {
                mint: Some("So1111111111111111111111111111111111111111O".into()),
                ..enter(now)
            },
        ] {
            assert!(!valid_decision(&bad), "{bad:?}");
        }
    }

    #[test]
    fn the_daily_observation_is_keyed_by_utc_day() -> std::result::Result<(), time::error::Error> {
        // 2026-11-09 01:00 at UTC+2 is still 2026-11-08 in UTC.
        let at = OffsetDateTime::from_unix_timestamp(1_794_178_800)?
            .to_offset(time::UtcOffset::from_hms(2, 0, 0)?);
        assert_eq!(daily_key(MINT, at), format!("{MINT}-2026-11-08"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn strategies_decisions_and_market_data_round_trip_and_prune(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memecoin_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let now = OffsetDateTime::now_utc();

        // Seeding again is a no-op; other content under a recorded version,
        // including a legacy one, is a conflict.
        insert_strategy_version(&db, &seed(now)).await?;
        insert_strategy_version(&db, &seed(now + Duration::minutes(1))).await?;
        register_strategy_version(&db, 2, "{}").await?;
        for conflict in [
            StrategyVersion {
                params: r#"{"a":2}"#.into(),
                ..seed(now)
            },
            StrategyVersion {
                version: 2,
                ..seed(now)
            },
        ] {
            assert!(matches!(
                insert_strategy_version(&db, &conflict).await,
                Err(MemecoinStoreError::VersionConflict)
            ));
        }
        let child = StrategyVersion {
            version: 3,
            parent_version: Some(1),
            author: StrategyAuthor::Claude,
            rationale: Some("Tighter stop.".into()),
            backtest: Some(r#"{"trades":12}"#.into()),
            ..seed(now)
        };
        insert_strategy_version(&db, &child).await?;
        insert_strategy_version(
            &db,
            &StrategyVersion {
                version: 4,
                strategy_id: "dip-b".into(),
                family: StrategyFamily::B,
                ..seed(now)
            },
        )
        .await?;
        assert_eq!(
            list_strategy_versions(&db, "momentum-a", 10).await?,
            [child, seed(now)]
        );

        assert!(insert_strategy(&db, "momentum-a", 1, true, "seed").await?);
        assert!(!insert_strategy(&db, "momentum-a", 1, true, "seed").await?);
        // A strategy only trades versions of its own.
        assert!(insert_strategy(&db, "dip-b", 1, true, "seed")
            .await
            .is_err());
        assert!(insert_strategy(&db, "dip-b", 4, false, "seed").await?);
        assert!(set_active_version(&db, "momentum-a", 3, "claude").await?);
        assert!(set_active_version(&db, "momentum-a", 4, "claude")
            .await
            .is_err());
        // Only the owner lifts an owner's disable.
        assert!(set_strategy_enabled(&db, "momentum-a", false, true, "device-1").await?);
        assert!(!set_strategy_enabled(&db, "momentum-a", true, false, "claude").await?);
        assert!(!set_strategy_enabled(&db, "momentum-a", false, false, "claude").await?);
        assert!(set_active_version(&db, "momentum-a", 1, "claude").await?);
        let strategies = list_strategies(&db).await?;
        assert_eq!(strategies.len(), 2);
        let momentum = &strategies[1];
        assert_eq!(
            (
                momentum.id.as_str(),
                momentum.active_version,
                momentum.enabled,
                momentum.disabled_by_owner,
                momentum.changed_by.as_str()
            ),
            ("momentum-a", 1, false, true, "device-1")
        );
        assert!(set_strategy_enabled(&db, "momentum-a", true, true, "device-1").await?);
        let momentum = &list_strategies(&db).await?[1];
        assert!(momentum.enabled && !momentum.disabled_by_owner);
        assert!(!set_strategy_enabled(&db, "missing", false, true, "device-1").await?);

        append_decision(&db, &enter(now - Duration::days(91))).await?;
        append_decision(
            &db,
            &Decision {
                kind: DecisionKind::WorkerUnavailable,
                mint: None,
                size_bps: None,
                ..enter(now)
            },
        )
        .await?;

        let five_days_ago = (now - Duration::days(5)).date();
        let observation = |at: OffsetDateTime, price: &str| Observation {
            mint: MINT.into(),
            at,
            price_usd: d(price),
            liquidity_usd: d("50000"),
        };
        for (at, price) in [
            (five_days_ago.with_hms(10, 0, 0)?.assume_utc(), "1"),
            (five_days_ago.with_hms(11, 0, 0)?.assume_utc(), "2"),
            (now - Duration::days(61), "4"),
            (now - Duration::hours(2), "5"),
            (now - Duration::hours(1), "6"),
        ] {
            record_observation(&db, &observation(at, price)).await?;
        }

        let entry = |mint: &str, migrated_at| UniverseEntry {
            mint: mint.into(),
            pool: OTHER.into(),
            program: OTHER.into(),
            migrated_at,
        };
        upsert_universe(&db, &entry(MINT, now)).await?;
        upsert_universe(&db, &entry(MINT, now - Duration::days(8))).await?;
        upsert_universe(&db, &entry(OTHER, now)).await?;

        // The 91-day decision, the 61-day and the second 5-day observation,
        // and the 8-day universe entry.
        assert_eq!(prune(&db).await?, 4);
        let mut left = db
            .query(
                "SELECT VALUE kind FROM memecoin_decisions; \
                 SELECT VALUE price_usd FROM memecoin_observations; \
                 SELECT VALUE mint FROM memecoin_universe;",
            )
            .await?
            .check()?;
        let kinds: Vec<String> = left.take(0)?;
        let mut prices: Vec<String> = left.take(1)?;
        prices.sort();
        let universe: Vec<String> = left.take(2)?;
        assert_eq!(kinds, ["worker_unavailable"]);
        assert_eq!(prices, ["1", "5", "6"]);
        assert_eq!(universe, [OTHER]);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn series_peaks_lineage_and_performance_round_trip(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("memecoin_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;

        // A series holds the last 25 hours and the daily points before, up
        // to `now`, oldest first; a limit keeps the newest.
        let at = |day: u8,
                  hour: u8,
                  minute: u8|
         -> std::result::Result<_, time::error::ComponentRange> {
            Ok(
                time::Date::from_calendar_date(2030, time::Month::January, day)?
                    .with_hms(hour, minute, 0)?
                    .assume_utc(),
            )
        };
        let now = at(10, 12, 0)?;
        let points = [
            (at(8, 10, 0)?, "1"), // daily
            (at(8, 11, 0)?, "2"), // too old and not daily
            (at(9, 13, 0)?, "3"), // daily
            (at(9, 14, 0)?, "4"),
            (at(10, 11, 0)?, "5"), // daily
            (at(10, 11, 30)?, "6"),
            (at(10, 12, 30)?, "7"), // after now
        ];
        for (at, price) in points {
            record_observation(
                &db,
                &Observation {
                    mint: MINT.into(),
                    at,
                    price_usd: d(price),
                    liquidity_usd: d("50000"),
                },
            )
            .await?;
        }
        let prices = |series: Vec<Observation>| {
            series
                .into_iter()
                .map(|o| o.price_usd.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            prices(load_series(&db, MINT, now, MAX_SERIES_POINTS).await?),
            ["1", "3", "4", "5", "6"]
        );
        assert_eq!(
            prices(load_series(&db, MINT, now, 3).await?),
            ["4", "5", "6"]
        );
        assert!(load_series(&db, OTHER, now, 10).await?.is_empty());

        // A child version must derive from its own strategy.
        let seed_a = seed(now);
        insert_strategy_version(&db, &seed_a).await?;
        let other = StrategyVersion {
            version: 2,
            strategy_id: "dip-b".into(),
            family: StrategyFamily::B,
            ..seed(now)
        };
        insert_strategy_version(&db, &other).await?;
        for parent in [2, 9] {
            let child = StrategyVersion {
                version: 10,
                parent_version: Some(parent),
                ..seed(now)
            };
            assert!(matches!(
                insert_strategy_version(&db, &child).await,
                Err(MemecoinStoreError::InvalidData)
            ));
        }
        let child = StrategyVersion {
            version: 3,
            parent_version: Some(1),
            ..seed(now)
        };
        insert_strategy_version(&db, &child).await?;

        // Trips of both versions of momentum-a count, dip-b's do not.
        let opened = OffsetDateTime::now_utc() - Duration::days(1);
        let results = [(1, "2"), (3, "-1"), (1, "-3"), (3, "4"), (2, "100")];
        for (i, (version, pnl)) in results.into_iter().enumerate() {
            let id = format!("p{i}");
            let start = opened + Duration::minutes(10 * i as i64);
            open_trip(
                &db,
                &Trip {
                    id: id.clone(),
                    mint: MINT.into(),
                    strategy_version: version,
                    tokens: d("10"),
                    cost_usd: d("10"),
                    entry_liquidity_usd: d("50000"),
                    opened_at: start,
                    exit_failures: 0,
                    stuck: false,
                    peak_value_usd: None,
                    close: None,
                },
            )
            .await?;
            if i == 0 {
                // Peaks are stored while the trip is open, never after.
                assert!(set_peak_value(&db, &id, d("12.5")).await?);
                assert_eq!(
                    load_held_trips(&db).await?[0].peak_value_usd,
                    Some(d("12.5"))
                );
            }
            let close = TripClose {
                closed_at: start + Duration::minutes(5),
                usd_out: d("10") + d(pnl),
                exit_fees_usd: d("0"),
                net_pnl_usd: d(pnl),
                exit_reason: "trailing_stop",
            };
            assert!(close_trip(&db, &id, &close).await?);
        }
        assert!(!set_peak_value(&db, "p0", d("13")).await?);
        add_cost(&db, CostKind::FailedTx, 3, d("0.5"), opened).await?;
        add_cost(&db, CostKind::FailedTx, 2, d("7"), opened).await?;

        let result = strategy_performance(&db, "momentum-a").await?;
        assert_eq!(result.closed.trades, 4);
        assert_eq!(result.closed.net_usd, d("2"));
        assert_eq!(result.closed.expectancy_usd, d("0.5"));
        assert_eq!(result.closed.win_rate_bps, 5_000);
        assert_eq!(result.closed.max_drawdown_usd, d("4"));
        assert_eq!((result.costs_usd, result.net_usd), (d("0.5"), d("1.5")));
        let none = strategy_performance(&db, "missing").await?;
        assert_eq!((none.closed.trades, none.net_usd), (0, Decimal::ZERO));
        assert!(strategy_performance(&db, "Not An Id").await.is_err());
        Ok(())
    }
}
