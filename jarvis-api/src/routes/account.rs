//! Passwords augment device signatures; they never create trusted devices or
//! authorize management operations on their own.
use crate::{
    audit::record_security_event,
    error::{internal, unauthorized},
    AppState, Authed,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use jarvis_client_core::account::AccountAction;
use jarvis_identity::{
    password::{AccountPassword, PasswordService},
    surreal::account,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordChange {
    password: AccountPassword,
    current_password: Option<AccountPassword>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    signature: String,
}

pub(crate) async fn status(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let owner = jarvis_identity::first_user(&state.db)
        .await
        .map_err(internal)?;
    let required = match owner {
        Some(owner) => account::password_required(&state.db, owner.id)
            .await
            .map_err(internal)?,
        None => false,
    };
    Ok(Json(json!({"protocol":1, "password_required":required,
        "bootstrap_required":account::bootstrap_available(&state.db).await.map_err(internal)?})))
}

pub(crate) async fn password_request(
    auth: Authed,
    State(state): State<AppState>,
    Json(req): Json<PasswordChange>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    account::verify_password(
        &state.db,
        PasswordService::shared(),
        auth.user.id,
        req.current_password,
    )
    .await
    .map_err(|_| unauthorized())?;
    let stored = PasswordService::shared()
        .hash(req.password)
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"password service unavailable"})),
            )
        })?;
    let request = account::request_action(
        &state.db,
        auth.user.id,
        auth.device.id,
        AccountAction::PasswordSet,
        auth.user.id,
        Some(stored),
    )
    .await
    .map_err(internal)?;
    Ok(Json(json!(request)))
}

pub(crate) async fn revoke_request(
    auth: Authed,
    State(state): State<AppState>,
    Path(target): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request = account::request_action(
        &state.db,
        auth.user.id,
        auth.device.id,
        AccountAction::DeviceRevoke,
        target,
        None,
    )
    .await
    .map_err(|_| unauthorized())?;
    Ok(Json(json!(request)))
}

pub(crate) async fn approve(
    auth: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<Approval>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.signature.len() != 128 {
        return Err(unauthorized());
    }
    let signature = hex::decode(req.signature).map_err(|_| unauthorized())?;
    let approved = account::approve_action(&state.db, id, auth.user.id, auth.device.id, &signature)
        .await
        .map_err(|_| unauthorized())?;
    state.realtime.disconnect_owner(
        auth.user.id,
        match approved.action {
            AccountAction::PasswordSet => None,
            AccountAction::DeviceRevoke => Some(approved.target),
        },
    );
    record_security_event(
        &state,
        Some(auth.device.id),
        approved.action.as_str(),
        "ok",
        None,
    )
    .await;
    Ok(Json(json!({"status":"completed"})))
}
