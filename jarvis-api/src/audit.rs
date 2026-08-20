//! Append-only security and agent audit trails. Audit payloads deliberately omit
//! secrets, prompts and action content. A write failure is logged but never
//! changes the authorization or execution outcome.

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
