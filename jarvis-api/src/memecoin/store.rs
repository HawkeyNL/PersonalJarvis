//! Typed repository for the memecoin paper trader (schema 0014).
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

use jarvis_store::Database;

/// Candidate decisions and gate snapshots are kept this long.
pub const RETENTION_DAYS: u32 = 30;
/// Rows deleted per statement and statements per table per sweep.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;
/// Replay reads the whole paper ledger; refuse an implausibly large one
/// instead of loading it without bound.
pub const MAX_LEDGER_ROWS: usize = 100_000;
/// The most rows a listing returns.
pub const MAX_LIST: usize = 100;

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

/// A cost without a fill: fees of a failed attempt or attributed AI spend.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub id: String,
    pub kind: CostKind,
    pub usd: Decimal,
    pub at: OffsetDateTime,
    pub mint: Option<String>,
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
const EXIT_REASONS: &[&str] = &["liquidity_drop", "stop_loss", "take_profit", "time_stop"];

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

pub async fn open_trip(db: &Database, trip: &Trip) -> Result<()> {
    if trip.close.is_some() {
        return Err(MemecoinStoreError::InvalidData);
    }
    run(
        db,
        "CREATE memecoin_paper_trips SET id = $id, slot = $mint, mint = $mint, strategy_version = $strategy_version, \
         status = 'open', tokens = $tokens, cost_usd = $cost_usd, entry_liquidity_usd = $entry_liquidity_usd, \
         opened_at = <datetime>$opened_at, closed_at = NONE, usd_out = NONE, exit_fees_usd = NONE, \
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
    let closed: Vec<String> = rows(
        db,
        "UPDATE memecoin_paper_trips SET status = 'closed', slot = $id, closed_at = <datetime>$closed_at, \
         usd_out = $usd_out, exit_fees_usd = $exit_fees_usd, net_pnl_usd = $net_pnl_usd, \
         exit_reason = $exit_reason WHERE record::id(id) = $id AND status = 'open' RETURN VALUE record::id(id)",
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

#[derive(Deserialize)]
struct TripRow {
    id: String,
    mint: String,
    strategy_version: u32,
    tokens: String,
    cost_usd: String,
    entry_liquidity_usd: String,
    #[serde(with = "time::serde::rfc3339")]
    opened_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    closed_at: Option<OffsetDateTime>,
    usd_out: Option<String>,
    exit_fees_usd: Option<String>,
    net_pnl_usd: Option<String>,
    exit_reason: Option<String>,
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
            close,
        })
    }
}

const TRIP_FIELDS: &str = "record::id(id) AS id, mint, strategy_version, tokens, cost_usd, \
    entry_liquidity_usd, opened_at, closed_at, usd_out, exit_fees_usd, net_pnl_usd, exit_reason";

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

pub async fn insert_cost(db: &Database, cost: &Cost) -> Result<()> {
    run(
        db,
        "CREATE memecoin_paper_costs SET id = $id, kind = $kind, usd = $usd, at = <datetime>$at, \
         mint = $mint, strategy_version = $strategy_version RETURN NONE",
        json!({
            "id": cost.id,
            "kind": cost.kind.as_str(),
            "usd": cost.usd.to_string(),
            "at": rfc3339(cost.at)?,
            "mint": cost.mint,
            "strategy_version": cost.strategy_version,
        }),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct CostRow {
    id: String,
    kind: String,
    usd: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
    mint: Option<String>,
    strategy_version: u32,
}

/// Every cost row, oldest first, for rebuilding the ledger.
pub async fn load_costs(db: &Database) -> Result<Vec<Cost>> {
    let rows: Vec<CostRow> = rows(
        db,
        &format!(
            "SELECT record::id(id) AS id, kind, usd, at, mint, strategy_version \
             FROM memecoin_paper_costs ORDER BY at LIMIT {}",
            MAX_LEDGER_ROWS + 1
        ),
        json!({}),
    )
    .await?;
    bounded(rows)?
        .into_iter()
        .map(|row| {
            Ok(Cost {
                id: row.id,
                kind: match row.kind.as_str() {
                    "failed_tx" => CostKind::FailedTx,
                    "ai_cost" => CostKind::AiCost,
                    _ => return Err(MemecoinStoreError::InvalidData),
                },
                usd: decimal(&row.usd)?,
                at: row.at,
                mint: row.mint,
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
        "SELECT paused, paused_at, paused_by FROM memecoin_paper_control WHERE record::id(id) = 'paper'",
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
        "UPSERT $record SET id = 'paper', paused_by = IF paused = true THEN paused_by ELSE $device END, \
         paused_at = IF paused = true THEN paused_at ELSE time::now() END, paused = true RETURN NONE",
        json!({
            "record": surrealdb::RecordId::from_table_key("memecoin_paper_control", "paper"),
            "device": device_id,
        }),
    )
    .await?;
    Ok(())
}

/// Clear the pause made at exactly `paused_at`, so a resume setting can
/// never clear a later pause. `true` when that pause was cleared.
pub async fn resume(db: &Database, paused_at: OffsetDateTime) -> Result<bool> {
    let cleared: Vec<String> = rows(
        db,
        "UPDATE memecoin_paper_control SET paused = false, paused_at = NONE, paused_by = NONE \
         WHERE record::id(id) = 'paper' AND paused = true AND paused_at = <datetime>$paused_at \
         RETURN VALUE record::id(id)",
        json!({"paused_at": rfc3339(paused_at)?}),
    )
    .await?;
    Ok(cleared.len() == 1)
}

/// Delete candidate decisions and gate snapshots older than
/// [`RETENTION_DAYS`], in bounded batches. The paper ledger itself (trips
/// and costs) is kept: the paper gate needs its whole history.
pub async fn prune(db: &Database) -> Result<usize> {
    let mut total = 0;
    for (table, field) in [
        ("memecoin_candidates", "observed_at"),
        ("memecoin_gate_snapshots", "at"),
    ] {
        let query = format!(
            "LET $ids = (SELECT VALUE id FROM {table} \
             WHERE {field} < time::now() - {RETENTION_DAYS}d LIMIT {PRUNE_BATCH}); \
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
        assert_eq!(trips[0].close, Some(close));
        assert_eq!(trips[0].tokens, trip.tokens);
        assert_eq!(trips[1].close, None);
        assert_eq!(recent_closed_trips(&db, 10).await?.len(), 1);

        insert_cost(
            &db,
            &Cost {
                id: "c1".into(),
                kind: CostKind::FailedTx,
                usd: d("0.011"),
                at: now,
                mint: Some(MINT.into()),
                strategy_version: 3,
            },
        )
        .await?;
        let costs = load_costs(&db).await?;
        assert_eq!(costs.len(), 1);
        assert_eq!(costs[0].kind, CostKind::FailedTx);

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
}
