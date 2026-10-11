//! Append-only security and agent audit trails. Audit payloads deliberately omit
//! secrets, prompts and action content. A write failure is logged but never
//! changes the authorization or execution outcome. Security rows older than
//! [`SECURITY_AUDIT_RETENTION_DAYS`] are pruned by an hourly sweep; this
//! includes rows written by the Codex and config brokers.

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_agent as agent;

use crate::{AppState, Authed, RateLimiter};

/// Bound on one audit write; on timeout the row is logged and dropped.
const AUDIT_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_agent_audit(
    state: &AppState,
    device_id: Uuid,
    action_type: &str,
    detail: Option<String>,
    risk: agent::RiskClass,
    outcome: &str,
    note: Option<&str>,
) {
    let risk_class = match risk {
        agent::RiskClass::Auto => "auto",
        agent::RiskClass::NeedsApproval => "needs_approval",
        agent::RiskClass::Denied => "denied",
    };
    if let Err(error) = state
        .db
        .query(
            "CREATE agent_audit SET id = $id, ts = time::now(), device_id = $device_id, \
         action_type = $action_type, detail = $detail, risk_class = $risk_class, \
         outcome = $outcome, note = $note RETURN NONE",
        )
        .bind(json!({
            "id": Uuid::now_v7().to_string(), "device_id": device_id.to_string(),
            "action_type": action_type, "detail": detail, "risk_class": risk_class,
            "outcome": outcome, "note": note,
        }))
        .await
    {
        tracing::warn!(%error, "failed to write agent audit");
    }
}

pub(crate) async fn record_security_event(
    state: &AppState,
    device_id: Option<Uuid>,
    event: &str,
    outcome: &str,
    detail: Option<&str>,
) {
    let write = state
        .db
        .query(
            "CREATE security_audit SET id = $id, ts = time::now(), device_id = $device_id, \
         event = $event, outcome = $outcome, detail = $detail RETURN NONE",
        )
        .bind(json!({
            "id": Uuid::now_v7().to_string(),
            "device_id": device_id.map(|id| id.to_string()), "event": event,
            "outcome": outcome, "detail": detail,
        }));
    match tokio::time::timeout(AUDIT_WRITE_TIMEOUT, async { write.await }).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(%error, event, "failed to write security audit"),
        Err(_) => tracing::warn!(event, outcome, "security audit write timed out; dropped"),
    }
}

/// Rows per event kind an unauthenticated caller can cause, across all
/// sources, per minute.
const UNAUTHENTICATED_AUDIT_PER_MIN: u32 = 10;

/// One budget per (event, outcome). Both are code literals, so the set of
/// keys is fixed and one noisy kind cannot starve the others.
fn admit_unauthenticated(
    limiter: &RateLimiter,
    event: &'static str,
    outcome: &'static str,
) -> bool {
    let admitted = limiter.check_reserved(
        &format!("audit:unauthenticated:{event}:{outcome}"),
        UNAUTHENTICATED_AUDIT_PER_MIN,
        std::time::Duration::from_secs(60),
    );
    if !admitted && limiter.note_dropped_audit() == 0 {
        tracing::warn!(
            event,
            outcome,
            "unauthenticated audit budget exhausted; dropping events"
        );
    }
    admitted
}

/// Write one `audit.unauthenticated dropped <n>` row for events dropped since
/// the last summary.
async fn flush_dropped_audit(state: &AppState) {
    let dropped = state.rate_limiter.take_dropped_audit();
    if dropped > 0 {
        let dropped = dropped.to_string();
        record_security_event(
            state,
            None,
            "audit.unauthenticated",
            "dropped",
            Some(&dropped),
        )
        .await;
    }
}

/// [`record_security_event`] for events any anonymous caller can trigger.
/// Budgeted per kind so a flood cannot grow the table without bound or bury
/// the owner's rows; the excess is dropped and reported in a summary row.
pub(crate) async fn record_unauthenticated_event(
    state: &AppState,
    device_id: Option<Uuid>,
    event: &'static str,
    outcome: &'static str,
    detail: Option<&str>,
) {
    if !admit_unauthenticated(&state.rate_limiter, event, outcome) {
        return;
    }
    flush_dropped_audit(state).await;
    record_security_event(state, device_id, event, outcome, detail).await;
}

/// Security audit rows are kept this long. Deliberately generous: it only
/// bounds growth and keeps months of history for investigations.
pub const SECURITY_AUDIT_RETENTION_DAYS: u32 = 180;
/// Rows deleted per statement, and statements per sweep, so one sweep never
/// holds a huge transaction; a backlog drains over several hours.
const RETENTION_BATCH: usize = 5_000;
const RETENTION_MAX_BATCHES: usize = 20;

fn prune_query() -> String {
    format!(
        "LET $ids = (SELECT VALUE id FROM security_audit \
         WHERE ts < time::now() - {SECURITY_AUDIT_RETENTION_DAYS}d LIMIT {RETENTION_BATCH}); \
         DELETE $ids RETURN NONE; RETURN array::len($ids);"
    )
}

/// Delete security audit rows older than [`SECURITY_AUDIT_RETENTION_DAYS`],
/// in bounded batches. Returns the number of rows deleted.
pub async fn prune_security_audit(db: &jarvis_store::Database) -> Result<usize, surrealdb::Error> {
    let query = prune_query();
    let mut total = 0;
    for _ in 0..RETENTION_MAX_BATCHES {
        let mut response = db.query(query.as_str()).await?.check()?;
        let deleted: Option<usize> = response.take(2)?;
        let deleted = deleted.unwrap_or(0);
        total += deleted;
        if deleted < RETENTION_BATCH {
            break;
        }
    }
    Ok(total)
}

/// Hourly audit housekeeping: report dropped unauthenticated events even when
/// no new one arrives, then apply retention.
pub async fn run_audit_maintenance(state: &AppState) {
    flush_dropped_audit(state).await;
    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        prune_security_audit(&state.db),
    )
    .await
    {
        Ok(Ok(deleted)) if deleted > 0 => tracing::info!(deleted, "security audit retention"),
        Ok(Ok(_)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "security audit retention failed"),
        Err(_) => tracing::warn!("security audit retention timed out"),
    }
}

#[derive(Deserialize)]
struct SecurityRow {
    device_id: Option<String>,
    event: String,
    outcome: String,
    detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    ts: OffsetDateTime,
}

pub(crate) async fn security_audit_log(
    _authed: Authed,
    State(state): State<AppState>,
) -> Json<Value> {
    let mut response = match state.db.query(
        "SELECT device_id, event, outcome, detail, ts FROM security_audit ORDER BY ts DESC LIMIT 100",
    ).await { Ok(response) => response, Err(error) => { tracing::warn!(%error, "failed to read security audit"); return Json(json!({"entries": []})); } };
    let rows: Vec<SecurityRow> = response.take(0).unwrap_or_default();
    Json(json!({"entries": rows.into_iter().map(|row| json!({
        "device_id": row.device_id, "event": row.event, "outcome": row.outcome,
        "detail": row.detail, "ts": row.ts.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
    })).collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
struct AgentRow {
    action_type: String,
    risk_class: String,
    outcome: String,
    note: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    ts: OffsetDateTime,
}

pub(crate) async fn agent_audit_log(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let mut response = match state.db.query(
        "SELECT action_type, risk_class, outcome, note, ts FROM agent_audit ORDER BY ts DESC LIMIT 50",
    ).await { Ok(response) => response, Err(error) => { tracing::warn!(%error, "failed to read agent audit"); return Json(json!({"enabled": state.agent_enabled, "entries": []})); } };
    let rows: Vec<AgentRow> = response.take(0).unwrap_or_default();
    Json(
        json!({"enabled": state.agent_enabled, "entries": rows.into_iter().map(|row| json!({
        "action": row.action_type, "risk": row.risk_class, "outcome": row.outcome,
        "note": row.note, "ts": row.ts.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
    })).collect::<Vec<_>>() }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthenticated_budgets_are_per_event_and_count_drops() {
        let limiter = RateLimiter::new();
        for _ in 0..UNAUTHENTICATED_AUDIT_PER_MIN {
            assert!(admit_unauthenticated(&limiter, "auth.login", "fail"));
        }
        assert!(!admit_unauthenticated(&limiter, "auth.login", "fail"));
        assert!(!admit_unauthenticated(&limiter, "auth.login", "fail"));
        // A flood of one kind leaves the others their own budget.
        assert!(admit_unauthenticated(
            &limiter,
            "device.pairing_request",
            "password_failed"
        ));
        assert_eq!(limiter.take_dropped_audit(), 2);
    }

    #[test]
    fn retention_query_parses() {
        let query = prune_query();
        assert!(query.contains("180d LIMIT 5000"));
        surrealdb::syn::parse(&query).unwrap();
    }
}
