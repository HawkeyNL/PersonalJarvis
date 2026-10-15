//! Passwords augment device signatures; they never create trusted devices or
//! authorize management operations on their own.
use crate::{
    audit::record_security_event,
    error::{bad_request, internal, unauthorized},
    rate_limit::{
        device_password_key, note_password_failure, note_password_verified, password_backoff,
        DEVICE_PASSWORD_BACKOFF,
    },
    AppState, Authed,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use jarvis_client_core::account::AccountAction;
use jarvis_identity::{
    password::{AccountPassword, PasswordError, PasswordService},
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

/// Forgotten-password recovery carries no current password: the device
/// signature and local root confirmation on the Home Node replace it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordReset {
    password: AccountPassword,
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
    if state
        .rate_limiter
        .password_backoff(
            &device_password_key(auth.device.id),
            DEVICE_PASSWORD_BACKOFF,
        )
        .is_some()
    {
        return Err(password_backoff());
    }
    match account::verify_password(
        &state.db,
        PasswordService::shared(),
        auth.user.id,
        req.current_password,
    )
    .await
    {
        Ok(_) => note_password_verified(&state, Some(auth.device.id)),
        Err(jarvis_identity::IdentityError::PasswordRejected) => {
            note_password_failure(&state, Some(auth.device.id));
            record_security_event(
                &state,
                Some(auth.device.id),
                "account.password_change",
                "password_failed",
                None,
            )
            .await;
            return Err(unauthorized());
        }
        Err(_) => return Err(unauthorized()),
    }
    let stored =
        PasswordService::shared()
            .hash(req.password)
            .await
            .map_err(|error| match error {
                PasswordError::Common => bad_request("password is too common; choose another"),
                _ => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":"password service unavailable"})),
                ),
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

pub(crate) async fn password_reset_request(
    auth: Authed,
    State(state): State<AppState>,
    Json(req): Json<PasswordReset>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let unavailable = || {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(
                json!({"error":"account recovery unavailable; one is pending or the limit is reached"}),
            ),
        )
    };
    // Bounds password hashing by a session that cannot sign; the database
    // enforces the per-day limit on signed recoveries.
    if !state.rate_limiter.check(
        &format!("account-recovery:{}", auth.user.id),
        10,
        std::time::Duration::from_secs(24 * 60 * 60),
    ) {
        return Err(unavailable());
    }
    account::recovery_available(&state.db, auth.user.id)
        .await
        .map_err(|_| unavailable())?;
    let stored =
        PasswordService::shared()
            .hash(req.password)
            .await
            .map_err(|error| match error {
                PasswordError::Common => bad_request("password is too common; choose another"),
                _ => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":"password service unavailable"})),
                ),
            })?;
    // Refused for a device that cannot sign approvals.
    let request = account::request_action(
        &state.db,
        auth.user.id,
        auth.device.id,
        AccountAction::PasswordReset,
        auth.user.id,
        Some(stored),
    )
    .await
    .map_err(|_| unauthorized())?;
    record_security_event(
        &state,
        Some(auth.device.id),
        "account.recovery.request",
        "prepared",
        None,
    )
    .await;
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
    let Ok(approved) =
        account::approve_action(&state.db, id, auth.user.id, auth.device.id, &signature).await
    else {
        record_security_event(
            &state,
            Some(auth.device.id),
            "account.approve",
            "fail",
            None,
        )
        .await;
        return Err(unauthorized());
    };
    if approved.action == AccountAction::PasswordReset {
        record_security_event(
            &state,
            Some(auth.device.id),
            "account.recovery.request",
            "signed",
            None,
        )
        .await;
        return Ok(Json(json!({"status":"pending_confirmation",
            "request_id":approved.request_id,
            "expires_in_seconds":account::RECOVERY_TTL_MINUTES * 60})));
    }
    state.realtime.disconnect_owner(
        auth.user.id,
        match approved.action {
            AccountAction::PasswordSet | AccountAction::PasswordReset => None,
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
