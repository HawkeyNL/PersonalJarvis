//! Append-only security and agent audit trails. Audit payloads deliberately omit
//! secrets, prompts and action content. A write failure is logged but never
//! changes the authorization or execution outcome. Security rows older than
//! [`SECURITY_AUDIT_RETENTION_DAYS`] are pruned by an hourly sweep.

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_agent as agent;

use crate::{AppState, Authed};

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
    if let Err(error) = state
        .db
        .query(
            "CREATE security_audit SET id = $id, ts = time::now(), device_id = $device_id, \
         event = $event, outcome = $outcome, detail = $detail RETURN NONE",
        )
        .bind(json!({
            "id": Uuid::now_v7().to_string(),
            "device_id": device_id.map(|id| id.to_string()), "event": event,
            "outcome": outcome, "detail": detail,
        }))
        .await
    {
        tracing::warn!(%error, event, "failed to write security audit");
    }
}

/// Rows an unauthenticated caller can cause, across all sources, per minute.
const UNAUTHENTICATED_AUDIT_PER_MIN: u32 = 10;

/// [`record_security_event`] for events any anonymous caller can trigger. They
/// share one global budget so a flood cannot grow the table without bound or
/// bury the owner's rows; the excess is dropped and later reported as one
/// `audit.unauthenticated dropped <n>` row.
pub(crate) async fn record_unauthenticated_event(
    state: &AppState,
    device_id: Option<Uuid>,
    event: &str,
    outcome: &str,
    detail: Option<&str>,
) {
    if !state.rate_limiter.check_reserved(
        "audit:unauthenticated",
        UNAUTHENTICATED_AUDIT_PER_MIN,
        std::time::Duration::from_secs(60),
    ) {
        if state.rate_limiter.note_dropped_audit() == 0 {
            tracing::warn!("unauthenticated audit budget exhausted; dropping events");
        }
        return;
    }
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
    record_security_event(state, device_id, event, outcome, detail).await;
}

/// Security audit rows are kept this long. Deliberately generous: it only
/// bounds growth and keeps months of history for investigations.
pub const SECURITY_AUDIT_RETENTION_DAYS: u32 = 180;

/// Delete security audit rows older than [`SECURITY_AUDIT_RETENTION_DAYS`].
pub async fn prune_security_audit(db: &jarvis_store::Database) -> Result<(), surrealdb::Error> {
    db.query(format!(
        "DELETE security_audit WHERE ts < time::now() - {SECURITY_AUDIT_RETENTION_DAYS}d RETURN NONE"
    ))
    .await?
    .check()?;
    Ok(())
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
