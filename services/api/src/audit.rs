//! Audit trails: the agent action log (ADR-029) and the security/auth log
//! (review P6). Both are append-only and never store secrets; a write failure is
//! logged, never surfaced, so auditing can never break the request path.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{AppState, Authed};
use jarvis_agent as agent;

/// Write one append-only agent audit row. Auditing must never break the action
/// path, so a DB failure is logged, not surfaced.
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
    let risk_str = match risk {
        agent::RiskClass::Auto => "auto",
        agent::RiskClass::NeedsApproval => "needs_approval",
        agent::RiskClass::Denied => "denied",
    };
    let res = sqlx::query(
        "INSERT INTO agent_audit (device_id, action_type, detail, risk_class, outcome, note) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(device_id)
    .bind(action_type)
    .bind(detail)
    .bind(risk_str)
    .bind(outcome)
    .bind(note)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "failed to write agent audit");
    }
}

/// Append a security/auth event to the audit trail (Priority 6). Best-effort:
/// a write failure is logged but never blocks the request, and no secrets are
/// ever stored — only the actor device, the event, the outcome, and a short note.
pub(crate) async fn record_security_event(
    state: &AppState,
    device_id: Option<Uuid>,
    event: &str,
    outcome: &str,
    detail: Option<&str>,
) {
    let res = sqlx::query(
        "INSERT INTO security_audit (device_id, event, outcome, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(device_id)
    .bind(event)
    .bind(outcome)
    .bind(detail)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, event, "failed to write security audit");
    }
}

/// The recent security/auth audit trail (Priority 6) — logins, enrolment,
/// logout, and device/unlock changes. Owner-only; never contains secrets.
pub(crate) async fn security_audit_log(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    type AuditRow = (Option<Uuid>, String, String, Option<String>, String);
    let rows: Vec<AuditRow> = sqlx::query_as(
        "SELECT device_id, event, outcome, detail, \
         to_char(ts, 'YYYY-MM-DD HH24:MI:SS') \
         FROM security_audit ORDER BY ts DESC LIMIT 100",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(device_id, event, outcome, detail, ts)| {
            json!({
                "device_id": device_id,
                "event": event,
                "outcome": outcome,
                "detail": detail,
                "ts": ts,
            })
        })
        .collect();
    Json(json!({ "entries": entries }))
}

/// The recent agent audit trail (ADR-029) — what Jarvis' hands have done.
pub(crate) async fn agent_audit_log(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT action_type, risk_class, outcome, note, \
         to_char(ts, 'YYYY-MM-DD HH24:MI:SS') \
         FROM agent_audit ORDER BY ts DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(action, risk, outcome, note, ts)| {
            json!({ "action": action, "risk": risk, "outcome": outcome, "note": note, "ts": ts })
        })
        .collect();
    Json(json!({ "enabled": state.agent_enabled, "entries": entries }))
}
