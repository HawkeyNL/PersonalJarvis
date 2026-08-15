//! Agentic execution — Jarvis' hands (ADR-029). Read-only actions run autonomously
//! behind the kill switch + a configured sandbox; mutating actions are held as a
//! pending record and only run once the owner signs their nonce with a trusted,
//! biometric-gated device. Every attempt — ok, denied, pending, error — is written
//! to the append-only agent audit log. The LLM can propose; only a signed human
//! can commit.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use jarvis_agent as agent;
use jarvis_identity as identity;

use crate::audit::record_agent_audit;
use crate::error::{bad_request, db_err, unauthorized};
use crate::validation;
use crate::{AppState, Authed};

use super::auth::ApproveReq;

/// Run a single read-only agent action (ADR-029 phase 4a). Gated by the kill
/// switch + a configured sandbox; only `Auto` (read-only) actions run; every
/// attempt — ok, denied, or error — is written to the append-only audit log.
pub(crate) async fn agent_action(
    authed: Authed,
    State(state): State<AppState>,
    Json(action): Json<agent::Action>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !state.agent_enabled {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "agent disabled", "hint": "zet JARVIS_AGENT_ENABLED=true" })),
        ));
    }
    let Some(sandbox) = state.agent_sandbox.clone() else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "no workspace", "hint": "zet JARVIS_AGENT_WORKSPACE_ROOT" })),
        ));
    };
    let at = agent::action_type(&action).to_string();
    let risk = agent::classify(&action);
    let detail = serde_json::to_string(&action).ok();

    // Mutating actions (4b) need a device-signed approval: validate + preview,
    // store a pending action, and return its nonce for the owner to sign.
    if agent::is_mutating(&action) {
        let preview = match agent::preview(&sandbox, &action).await {
            Ok(p) => p,
            Err(e) => {
                // A protected/escaping target is refused now — no pending created.
                record_agent_audit(&state, authed.device.id, &at, detail, risk, "denied", Some(&e.to_string())).await;
                return Err((StatusCode::FORBIDDEN, Json(json!({ "error": e.to_string() }))));
            }
        };
        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
        let id = Uuid::now_v7();
        let action_json = serde_json::to_string(&action).unwrap_or_default();
        let res = sqlx::query(
            "INSERT INTO agent_pending_actions \
             (id, user_id, requesting_device_id, action_type, action, preview, nonce, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '5 minutes')",
        )
        .bind(id)
        .bind(authed.user.id)
        .bind(authed.device.id)
        .bind(&at)
        .bind(&action_json)
        .bind(&preview)
        .bind(&nonce[..])
        .execute(&state.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "failed to create pending action");
            return Err(db_err(e));
        }
        record_agent_audit(&state, authed.device.id, &at, detail, risk, "pending", None).await;
        return Ok(Json(json!({
            "needs_approval": true,
            "pending_id": id,
            "nonce": hex::encode(nonce),
            "action": at,
            "preview": preview,
        })));
    }

    match agent::execute(&sandbox, &action).await {
        Ok(outcome) => {
            let note = outcome.truncated.then_some("output truncated");
            record_agent_audit(&state, authed.device.id, &at, detail, risk, "ok", note).await;
            Ok(Json(json!({
                "action": at,
                "output": outcome.output,
                "truncated": outcome.truncated,
            })))
        }
        Err(e) => {
            let denied = matches!(
                e,
                agent::AgentError::Denied(_) | agent::AgentError::OutsideSandbox
            );
            let (label, code) = if denied {
                ("denied", StatusCode::FORBIDDEN)
            } else {
                ("error", StatusCode::BAD_REQUEST)
            };
            record_agent_audit(&state, authed.device.id, &at, detail, risk, label, Some(&e.to_string())).await;
            Err((code, Json(json!({ "error": e.to_string() }))))
        }
    }
}

/// Mutating actions awaiting the owner's device-signed approval (ADR-029 4b).
pub(crate) async fn agent_pending(authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT id, action_type, preview, encode(nonce, 'hex'), \
         to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') \
         FROM agent_pending_actions \
         WHERE user_id = $1 AND status = 'pending' AND expires_at > now() \
         ORDER BY created_at DESC",
    )
    .bind(authed.user.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(id, at, preview, nonce, created)| {
            json!({ "pending_id": id, "action": at, "preview": preview, "nonce": nonce, "created_at": created })
        })
        .collect();
    Json(json!({ "pending": entries }))
}

/// Approve a pending mutation by signing its nonce with a trusted device, then
/// execute it once (ADR-029 4b). The signature proves owner presence (the device
/// key is biometric-gated); the stored action is what runs — the LLM can propose,
/// only a signed human can commit.
pub(crate) async fn agent_pending_approve(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
    let signature =
        hex::decode(&req.signature).map_err(|_| bad_request("invalid signature encoding"))?;
    let Some(sandbox) = state.agent_sandbox.clone() else {
        return Err((StatusCode::FORBIDDEN, Json(json!({ "error": "no workspace" }))));
    };

    // Fetch the pending action (must be this user's, still pending + unexpired).
    let row: Option<(Vec<u8>, String, String)> = sqlx::query_as(
        "SELECT nonce, action, action_type FROM agent_pending_actions \
         WHERE id = $1 AND user_id = $2 AND status = 'pending' AND expires_at > now()",
    )
    .bind(id)
    .bind(authed.user.id)
    .fetch_optional(&state.db)
    .await
    .map_err(db_err)?;
    let (nonce, action_json, at) = row.ok_or_else(|| {
        (StatusCode::NOT_FOUND, Json(json!({ "error": "no such pending action" })))
    })?;

    // Verify the owner's device signature over the nonce.
    identity::verify_device_signature(&state.db, authed.user.id, authed.device.id, &nonce, &signature)
        .await
        .map_err(|_| unauthorized())?;

    // Consume it atomically — mark executed so it can never run twice (replay).
    let claimed = sqlx::query(
        "UPDATE agent_pending_actions SET status = 'executed', \
         approved_by_device_id = $2, resolved_at = now() \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(authed.device.id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;
    if claimed.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, Json(json!({ "error": "already resolved" }))));
    }

    let action: agent::Action = serde_json::from_str(&action_json).map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "corrupt pending action" })))
    })?;

    match agent::execute(&sandbox, &action).await {
        Ok(outcome) => {
            let note = outcome.truncated.then_some("output truncated");
            record_agent_audit(&state, authed.device.id, &at, Some(action_json), agent::RiskClass::NeedsApproval, "ok", note).await;
            Ok(Json(json!({ "action": at, "output": outcome.output, "truncated": outcome.truncated })))
        }
        Err(e) => {
            record_agent_audit(&state, authed.device.id, &at, Some(action_json), agent::RiskClass::NeedsApproval, "error", Some(&e.to_string())).await;
            Err((StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))))
        }
    }
}

/// Deny a pending mutation (no signature needed — the denier is authenticated and
/// a denial only cancels).
pub(crate) async fn agent_pending_deny(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let res = sqlx::query(
        "UPDATE agent_pending_actions SET status = 'denied', resolved_at = now() \
         WHERE id = $1 AND user_id = $2 AND status = 'pending'",
    )
    .bind(id)
    .bind(authed.user.id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;
    if res.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "no such pending action" }))));
    }
    record_agent_audit(&state, authed.device.id, "pending", None, agent::RiskClass::NeedsApproval, "denied", Some("denied by owner")).await;
    Ok(Json(json!({ "status": "denied" })))
}
