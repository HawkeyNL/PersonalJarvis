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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_agent as agent;
use jarvis_identity as identity;

use crate::audit::record_agent_audit;
use crate::error::{bad_request, unauthorized};
use crate::validation;
use crate::{AppState, Authed};

use super::auth::ApproveReq;

#[derive(Serialize)]
struct PendingCreate {
    id: String,
    user_id: String,
    requesting_device_id: String,
    action_type: String,
    action: String,
    preview: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
}

#[derive(Deserialize)]
struct PendingListRow {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    action_type: String,
    preview: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(Deserialize)]
struct PendingApprovalRow {
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    action: String,
    action_type: String,
}

#[derive(Deserialize)]
struct Claimed {
    id: String,
}

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
    // Prompts and write content can contain sensitive material. The audit trail
    // records the capability, risk and outcome, never unredacted action payloads.
    let detail = None;

    // Mutating actions (4b) need a device-signed approval: validate + preview,
    // store a pending action, and return its nonce for the owner to sign.
    if agent::is_mutating(&action) {
        let preview = match agent::preview(&sandbox, &action).await {
            Ok(p) => p,
            Err(_e) => {
                // A protected/escaping target is refused now — no pending created.
                record_agent_audit(
                    &state,
                    authed.device.id,
                    &at,
                    detail,
                    risk,
                    "denied",
                    Some("action refused"),
                )
                .await;
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({ "error": "agent action refused" })),
                ));
            }
        };
        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
        let id = Uuid::now_v7();
        let action_json = serde_json::to_string(&action).unwrap_or_default();
        let res = state
            .db
            .query(
                "CREATE agent_pending_actions SET id = $id, user_id = $user_id, \
             requesting_device_id = $requesting_device_id, action_type = $action_type, \
             action = $action, preview = $preview, nonce = <bytes>$nonce, status = 'pending', \
             created_at = time::now(), expires_at = time::now() + 5m, resolved_at = NONE, \
             approved_by_device_id = NONE RETURN NONE",
            )
            .bind(PendingCreate {
                id: id.to_string(),
                user_id: authed.user.id.to_string(),
                requesting_device_id: authed.device.id.to_string(),
                action_type: at.clone(),
                action: action_json,
                preview: preview.clone(),
                nonce: nonce.to_vec(),
            })
            .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "failed to create pending action");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            ));
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
            record_agent_audit(
                &state,
                authed.device.id,
                &at,
                detail,
                risk,
                label,
                Some("action failed"),
            )
            .await;
            Err((code, Json(json!({ "error": "agent action failed" }))))
        }
    }
}

/// Mutating actions awaiting the owner's device-signed approval (ADR-029 4b).
pub(crate) async fn agent_pending(authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<PendingListRow> = match state
        .db
        .query(
            "SELECT record::id(id) AS id, action_type, preview, nonce, created_at \
         FROM agent_pending_actions WHERE user_id = $user_id AND status = 'pending' \
         AND expires_at > time::now() ORDER BY created_at DESC",
        )
        .bind(json!({"user_id": authed.user.id.to_string()}))
        .await
    {
        Ok(mut response) => response.take(0).unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, "failed to list pending actions");
            Vec::new()
        }
    };
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({ "pending_id": row.id, "action": row.action_type, "preview": row.preview,
                "nonce": hex::encode(row.nonce), "created_at": row.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
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
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "no workspace" })),
        ));
    };

    // Fetch the pending action (must be this user's, still pending + unexpired).
    let row: Option<PendingApprovalRow> = state.db.query(
        "SELECT nonce, action, action_type FROM agent_pending_actions WHERE record::id(id) = $id \
         AND user_id = $user_id AND status = 'pending' AND expires_at > time::now() LIMIT 1",
    ).bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string()})).await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))))?
        .take(0).map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))))?;
    let PendingApprovalRow {
        nonce,
        action: action_json,
        action_type: at,
    } = row.ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such pending action" })),
        )
    })?;

    // Verify the owner's device signature over the nonce.
    identity::verify_device_signature(
        &state.db,
        authed.user.id,
        authed.device.id,
        &nonce,
        &signature,
    )
    .await
    .map_err(|_| unauthorized())?;

    // Consume it atomically — mark executed so it can never run twice (replay).
    let mut claimed_response = state.db.query(
        "UPDATE agent_pending_actions SET status = 'executed', approved_by_device_id = $device_id, \
         resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user_id \
         AND status = 'pending' AND expires_at > time::now() RETURN record::id(id) AS id",
    ).bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string(), "device_id": authed.device.id.to_string()})).await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))))?;
    let claimed: Option<Claimed> = claimed_response.take(0).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"internal error"})),
        )
    })?;
    if claimed.map(|row| row.id) != Some(id.to_string()) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "already resolved" })),
        ));
    }

    let action: agent::Action = serde_json::from_str(&action_json).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "corrupt pending action" })),
        )
    })?;

    match agent::execute(&sandbox, &action).await {
        Ok(outcome) => {
            let note = outcome.truncated.then_some("output truncated");
            record_agent_audit(
                &state,
                authed.device.id,
                &at,
                None,
                agent::RiskClass::NeedsApproval,
                "ok",
                note,
            )
            .await;
            Ok(Json(
                json!({ "action": at, "output": outcome.output, "truncated": outcome.truncated }),
            ))
        }
        Err(_e) => {
            record_agent_audit(
                &state,
                authed.device.id,
                &at,
                None,
                agent::RiskClass::NeedsApproval,
                "error",
                Some("action failed"),
            )
            .await;
            Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "agent action failed" })),
            ))
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
    let mut response = state
        .db
        .query(
            "UPDATE agent_pending_actions SET status = 'denied', resolved_at = time::now() \
         WHERE record::id(id) = $id AND user_id = $user_id AND status = 'pending' \
         AND expires_at > time::now() RETURN record::id(id) AS id",
        )
        .bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string()}))
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"internal error"})),
            )
        })?;
    let denied: Option<Claimed> = response.take(0).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"internal error"})),
        )
    })?;
    if denied.map(|row| row.id) != Some(id.to_string()) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such pending action" })),
        ));
    }
    record_agent_audit(
        &state,
        authed.device.id,
        "pending",
        None,
        agent::RiskClass::NeedsApproval,
        "denied",
        Some("denied by owner"),
    )
    .await;
    Ok(Json(json!({ "status": "denied" })))
}
