//! Device-bound authentication and the trusted-device unlock flow. Enrollment is
//! dev-only; login verifies an Ed25519 signature over a single-use challenge and
//! issues a session token. The unlock flow lets an already-trusted device sign a
//! nonce to admit a new one. Every state-changing step is written to the security
//! audit trail via [`record_security_event`].

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{Extensions, HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) use jarvis_client_core::ApprovalRequest as ApproveReq;
use jarvis_client_core::{
    ChallengeRequest as ChallengeReq, EnrollmentRequest as EnrollReq,
    PairingApproveRequest as PairingApproveReq,
};
use jarvis_identity as identity;

use crate::audit::{record_security_event, record_unauthenticated_event};
use crate::error::{bad_request, internal, unauthorized};
use crate::rate_limit::{
    allow_authenticated_device, device_password_key, note_password_failure, note_password_verified,
    password_backoff, ACCOUNT_PASSWORD_BACKOFF, ACCOUNT_PASSWORD_KEY, DEVICE_PASSWORD_BACKOFF,
};
use crate::validation;
use crate::{AppState, Authed};

/// Pre-auth attempts on `/v1/auth/pairing/requests` (each a password check
/// when the owner has a stored password), for all sources. A correct guess
/// only yields a pending request that a device or the local root operator
/// must still approve; the budget bounds guessing and CPU.
const PAIRING_PASSWORD_CHECKS_PER_MIN: u32 = 60;

/// Retryable: no password-check capacity. Never counted as a failed login.
fn busy() -> (StatusCode, Json<Value>) {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "busy", "hint": "try again shortly" })),
    )
}

/// Dev-only device enrollment: create the single user if needed and register
/// the calling device with its public key. Off unless `JARVIS_DEV_ENROLLMENT`
/// is set, and never in production.
pub(crate) async fn auth_enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !state.dev_enrollment || state.environment == "production" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "enrollment is disabled" })),
        ));
    }
    // Bound the free-text fields and pin the key to an exact-length hex string
    // before it ever reaches the DB or crypto layer (fail closed on junk input).
    if !validation::bounded_text(&req.name, validation::MAX_DEVICE_NAME_LEN) {
        return Err(bad_request("invalid device name"));
    }
    if !validation::bounded_text(&req.platform, validation::MAX_PLATFORM_LEN) {
        return Err(bad_request("invalid platform"));
    }
    if !validation::is_hex_of_len(&req.public_key, validation::ED25519_PUBLIC_KEY_HEX_LEN) {
        return Err(bad_request("invalid public_key"));
    }
    let platform =
        identity::Platform::parse(&req.platform).map_err(|_| bad_request("unknown platform"))?;
    let public_key =
        hex::decode(&req.public_key).map_err(|_| bad_request("invalid public_key encoding"))?;

    let user = identity::first_user_or_create(&state.db, "Jarvis user")
        .await
        .map_err(internal)?;
    let (device, _key) = identity::register_device(
        &state.db,
        user.id,
        &req.name,
        platform,
        "ed25519",
        &public_key,
    )
    .await
    .map_err(internal)?;

    record_security_event(&state, Some(device.id), "auth.enroll", "ok", None).await;
    Ok(Json(json!({
        "user_id": user.id,
        "device_id": device.id,
    })))
}

/// First-owner bootstrap is deliberately unlike development enrollment: only a
/// locally provisioned verifier, an explicit LAN client range, and an empty
/// trusted-device set permit it. The raw secret is supplied in a header so it
/// never becomes JSON/audit content.
pub(crate) async fn auth_bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    extensions: Extensions,
    Json(req): Json<PasswordEnrollmentReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Production activation is provisioned locally at a fixed protected path;
    // legacy environment-only verifiers cannot silently remain valid forever.
    let policy = if state.environment == "production" {
        jarvis_config::activation::load().map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"bootstrap unavailable"})),
            )
        })?
    } else {
        state.bootstrap_enrollment.clone()
    };
    let Some(policy) = policy else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "bootstrap unavailable" })),
        ));
    };
    let expires_at = policy
        .expires_at()
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"bootstrap unavailable"})),
            )
        })?;
    let password = req
        .password
        .ok_or_else(|| bad_request("account password required"))?;
    let req = EnrollReq {
        name: req.name,
        platform: req.platform,
        public_key: req.public_key,
    };
    // The shared peer policy decides whether a forwarding header is trusted;
    // a direct caller can never make one count.
    let client_ip = crate::peer::client_ip(&extensions, &headers, &state);
    if !client_ip.is_some_and(|ip| policy.allows(ip)) {
        record_unauthenticated_event(&state, None, "auth.bootstrap", "rejected_network", None)
            .await;
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "bootstrap unavailable" })),
        ));
    }
    let secret = headers
        .get("x-jarvis-bootstrap-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !policy.verifies(secret) {
        record_unauthenticated_event(&state, None, "auth.bootstrap", "invalid_secret", None).await;
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "bootstrap unavailable" })),
        ));
    }
    validate_enrollment(&req)?;
    let platform =
        identity::Platform::parse(&req.platform).map_err(|_| bad_request("unknown platform"))?;
    let key =
        hex::decode(&req.public_key).map_err(|_| bad_request("invalid public_key encoding"))?;
    let stored = identity::password::PasswordService::shared()
        .hash(password)
        .await
        .map_err(|error| match error {
            identity::password::PasswordError::Common => {
                bad_request("password is too common; choose another")
            }
            _ => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"activation unavailable"})),
            ),
        })?;
    let (user, device) = identity::surreal::account::bootstrap_account(
        &state.db, &req.name, platform, &key, stored, expires_at,
    )
    .await
    .map_err(|_| {
        (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "bootstrap unavailable" })),
        )
    })?;
    record_security_event(&state, Some(device.id), "auth.bootstrap", "ok", None).await;
    Ok(Json(json!({ "user_id": user.id, "device_id": device.id })))
}

fn validate_enrollment(req: &EnrollReq) -> Result<(), (StatusCode, Json<Value>)> {
    if !validation::bounded_text(&req.name, validation::MAX_DEVICE_NAME_LEN) {
        return Err(bad_request("invalid device name"));
    }
    if !validation::bounded_text(&req.platform, validation::MAX_PLATFORM_LEN) {
        return Err(bad_request("invalid platform"));
    }
    if !validation::is_hex_of_len(&req.public_key, validation::ED25519_PUBLIC_KEY_HEX_LEN) {
        return Err(bad_request("invalid public_key"));
    }
    Ok(())
}

/// An untrusted candidate can request pairing but cannot select an owner: the
/// single existing Jarvis owner is resolved server-side. No bearer token is
/// accepted or needed for this non-authoritative waiting record.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordEnrollmentReq {
    name: String,
    platform: String,
    public_key: String,
    password: Option<identity::password::AccountPassword>,
}

pub(crate) async fn pairing_create(
    State(state): State<AppState>,
    Json(req): Json<PasswordEnrollmentReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let password = req.password;
    let req = EnrollReq {
        name: req.name,
        platform: req.platform,
        public_key: req.public_key,
    };
    validate_enrollment(&req)?;
    let user = identity::first_user(&state.db)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "first device bootstrap required" })),
            )
        })?;
    if identity::active_device_count(&state.db)
        .await
        .map_err(internal)?
        == 0
        && (identity::surreal::account::bootstrap_available(&state.db)
            .await
            .map_err(internal)?
            || !identity::surreal::account::password_required(&state.db, user.id)
                .await
                .map_err(internal)?)
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "first device bootstrap required" })),
        ));
    }
    // With no active device, only an activated owner (bootstrap consumed and
    // a password stored) reaches this point, so the request needs the
    // password and explicit root-peer local approval. With an active device,
    // an owner who has a stored password must supply it; an owner without
    // one (legacy setup) gets a pending request with no password check at
    // all. A request never grants device authority: an existing
    // device or the local root operator must still approve it.
    // Every attempt spends this global budget, which caps the Argon2 runs
    // across all sources so address rotation buys neither online guesses nor
    // memory pressure. Validation failures above do not spend it, and neither
    // does a request refused by the account-wide password backoff.
    if state
        .rate_limiter
        .password_backoff(ACCOUNT_PASSWORD_KEY, ACCOUNT_PASSWORD_BACKOFF)
        .is_some()
    {
        return Err(password_backoff());
    }
    if !state.rate_limiter.check_reserved(
        "pairing:global",
        PAIRING_PASSWORD_CHECKS_PER_MIN,
        std::time::Duration::from_secs(60),
    ) {
        return Err(busy());
    }
    let password_verified = match identity::surreal::account::verify_password(
        &state.db,
        identity::password::PasswordService::pre_auth(),
        user.id,
        password,
    )
    .await
    {
        Ok(Some(_)) => {
            note_password_verified(&state, None);
            true
        }
        Ok(None) => false,
        Err(identity::IdentityError::Busy) => return Err(busy()),
        Err(identity::IdentityError::PasswordRejected) => {
            note_password_failure(&state, None);
            record_unauthenticated_event(
                &state,
                None,
                "device.pairing_request",
                "password_failed",
                None,
            )
            .await;
            return Err(unauthorized());
        }
        Err(_) => return Err(unauthorized()),
    };
    let platform =
        identity::Platform::parse(&req.platform).map_err(|_| bad_request("unknown platform"))?;
    let key =
        hex::decode(&req.public_key).map_err(|_| bad_request("invalid public_key encoding"))?;
    let pairing = identity::create_pairing_request(&state.db, user.id, &req.name, platform, &key)
        .await
        .map_err(|_| {
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "pairing request unavailable" })),
            )
        })?;
    // Only a password-verified request may reach the owner's phone; an
    // unauthenticated caller must not be able to trigger pushes.
    if password_verified {
        state
            .notifier
            .notify_approval(crate::notify::NotifyEvent::PairingRequested, user.id, None);
    }
    // A password-verified request is rare and always worth a row; one without
    // a password check (legacy owner) is budgeted like other anonymous events.
    let fingerprint = Some(&pairing.candidate_fingerprint[..16]);
    if password_verified {
        record_security_event(
            &state,
            None,
            "device.pairing_request",
            "created",
            fingerprint,
        )
        .await;
    } else {
        record_unauthenticated_event(
            &state,
            None,
            "device.pairing_request",
            "created",
            fingerprint,
        )
        .await;
    }
    Ok(Json(
        json!({ "request_id": pairing.id, "nonce": hex::encode(pairing.nonce), "expires_at": pairing.expires_at.unix_timestamp() }),
    ))
}

pub(crate) async fn pairing_pending(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let requests = identity::pending_pairing_requests(&state.db, authed.user.id)
        .await
        .map_err(internal)?;
    let items: Vec<Value> = requests.iter().map(|r| json!({
        "id": r.id, "device_name": r.candidate_name, "platform": r.candidate_platform,
        "fingerprint": &r.candidate_fingerprint[..16], "nonce": hex::encode(&r.nonce),
        "candidate_public_key": hex::encode(&r.candidate_public_key), "created_at": r.created_at.unix_timestamp(), "expires_at": r.expires_at.unix_timestamp(),
    })).collect();
    Ok(Json(json!({ "requests": items })))
}

/// Candidate polling is authorised by the 256-bit request nonce in a header;
/// it is not a bearer credential and never grants API access. Keeping it out of
/// the URL prevents accidental proxy/history leakage.
pub(crate) async fn pairing_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let nonce = headers
        .get("x-jarvis-pairing-nonce")
        .and_then(|v| v.to_str().ok())
        .filter(|value| validation::is_hex_of_len(value, 64))
        .and_then(|value| hex::decode(value).ok())
        .ok_or_else(unauthorized)?;
    let Some((status, device_id, just_expired)) =
        identity::pairing_status_for_candidate(&state.db, id, &nonce)
            .await
            .map_err(|_| unauthorized())?
    else {
        return Err(unauthorized());
    };
    if just_expired {
        record_unauthenticated_event(
            &state,
            None,
            "device.pairing_request",
            "expired",
            Some(&id.to_string()),
        )
        .await;
    }
    Ok(Json(json!({ "status": status, "device_id": device_id })))
}

pub(crate) async fn pairing_approve(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<PairingApproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
    let sig = hex::decode(&req.signature).map_err(|_| bad_request("invalid signature encoding"))?;
    let device = match identity::approve_pairing_request(
        &state.db,
        id,
        authed.user.id,
        authed.device.id,
        &sig,
    )
    .await
    {
        Ok(device) => device,
        Err(_) => {
            record_security_event(
                &state,
                Some(authed.device.id),
                "device.pairing_approval",
                "failed",
                Some(&id.to_string()),
            )
            .await;
            return Err(unauthorized());
        }
    };
    record_security_event(
        &state,
        Some(authed.device.id),
        "device.pairing_approval",
        "ok",
        Some(&device.id.to_string()),
    )
    .await;
    record_security_event(&state, Some(device.id), "device.activated", "ok", None).await;
    Ok(Json(json!({ "status": "approved" })))
}

pub(crate) async fn pairing_deny(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    identity::deny_pairing_request(&state.db, id, authed.user.id, authed.device.id)
        .await
        .map_err(|_| unauthorized())?;
    record_security_event(
        &state,
        Some(authed.device.id),
        "device.pairing_request",
        "denied",
        Some(&id.to_string()),
    )
    .await;
    Ok(Json(json!({ "status": "denied" })))
}

/// Issue a login challenge (nonce) for a device.
pub(crate) async fn auth_challenge(
    State(state): State<AppState>,
    Json(req): Json<ChallengeReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let challenge = identity::create_challenge(&state.db, req.device_id)
        .await
        .map_err(|_| unauthorized())?;
    Ok(Json(json!({
        "challenge_id": challenge.id,
        "nonce": hex::encode(challenge.nonce),
    })))
}

/// Verify a signed challenge and issue a session token.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordLoginReq {
    device_id: Uuid,
    challenge_id: Uuid,
    signature: String,
    password: Option<identity::password::AccountPassword>,
}

pub(crate) async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<PasswordLoginReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
    let signature = hex::decode(&req.signature).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid signature encoding" })),
        )
    })?;
    // Only a caller holding this device's key can put it into backoff.
    if state
        .rate_limiter
        .password_backoff(&device_password_key(req.device_id), DEVICE_PASSWORD_BACKOFF)
        .is_some()
    {
        return Err(password_backoff());
    }
    let result = match identity::surreal::login_with_password(
        &state.db,
        req.device_id,
        req.challenge_id,
        &signature,
        req.password,
    )
    .await
    {
        Ok(r) => r,
        Err(identity::IdentityError::Busy) => return Err(busy()),
        // Valid device signature, wrong password: bounded by the device
        // backoff, so it is always recorded.
        Err(identity::IdentityError::PasswordRejected) => {
            note_password_failure(&state, Some(req.device_id));
            record_security_event(
                &state,
                Some(req.device_id),
                "auth.login",
                "password_failed",
                None,
            )
            .await;
            return Err(unauthorized());
        }
        Err(_) => {
            record_unauthenticated_event(&state, Some(req.device_id), "auth.login", "fail", None)
                .await;
            return Err(unauthorized());
        }
    };
    note_password_verified(&state, Some(req.device_id));
    record_security_event(&state, Some(req.device_id), "auth.login", "ok", None).await;
    Ok(Json(json!({
        "token": result.token,
        "expires_at": result.session.expires_at.unix_timestamp(),
    })))
}

/// List the authenticated user's active devices (protected).
pub(crate) async fn list_devices(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let devices = identity::list_active_devices(&state.db, authed.user.id)
        .await
        .map_err(internal)?;
    let items: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "name": d.name,
                "platform": d.platform,
                "status": d.status,
                "created_at": d.created_at.unix_timestamp(),
            })
        })
        .collect();
    Ok(Json(json!({ "devices": items })))
}

/// Push-target writes (set and delete together) one device may make per minute.
const PUSH_TARGET_WRITES_PER_MIN: u32 = 6;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PushTargetReq {
    apns_token: String,
    apns_env: String,
}

/// An APNs device token: 64 to 200 hex characters, stored lowercase.
fn apns_token(raw: &str) -> Option<String> {
    ((64..=200).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| raw.to_ascii_lowercase())
}

fn push_target_rate_limited(state: &AppState, device: Uuid) -> Option<(StatusCode, Json<Value>)> {
    (!allow_authenticated_device(state, device, "push-target", PUSH_TARGET_WRITES_PER_MIN)).then(
        || {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "rate limited" })),
            )
        },
    )
}

/// Register the calling device's APNs token. Always bound to the session's
/// own device; a push only says that an approval is waiting.
pub(crate) async fn push_target_set(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<PushTargetReq>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    if let Some(limited) = push_target_rate_limited(&state, authed.device.id) {
        return Err(limited);
    }
    let token = apns_token(&req.apns_token).ok_or_else(|| bad_request("invalid apns_token"))?;
    if !matches!(req.apns_env.as_str(), "sandbox" | "production") {
        return Err(bad_request("invalid apns_env"));
    }
    identity::set_push_target(&state.db, authed.device.id, &token, &req.apns_env)
        .await
        .map_err(internal)?;
    record_security_event(&state, Some(authed.device.id), "push.target", "set", None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove the calling device's push target; idempotent.
pub(crate) async fn push_target_delete(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    if let Some(limited) = push_target_rate_limited(&state, authed.device.id) {
        return Err(limited);
    }
    identity::delete_push_target(&state.db, authed.device.id)
        .await
        .map_err(internal)?;
    record_security_event(
        &state,
        Some(authed.device.id),
        "push.target",
        "deleted",
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Minimal authenticated identity metadata used to bind native pairing signing
/// without exposing user details to untrusted candidates.
pub(crate) async fn auth_me(authed: Authed) -> Json<Value> {
    Json(json!({ "user_id": authed.user.id, "device_id": authed.device.id }))
}

/// Log out: revoke the current session server-side.
pub(crate) async fn auth_logout(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    identity::revoke_session(&state.db, authed.session_id)
        .await
        .map_err(internal)?;
    record_security_event(&state, Some(authed.device.id), "auth.logout", "ok", None).await;
    Ok(Json(json!({ "status": "logged out" })))
}

/// Unlock requests one device may create per minute. Bounds the rows and
/// audit entries a signed-in device can create (pushes are already coalesced
/// per kind); a retry only needs one, as a new request supersedes the
/// device's older pending one.
const UNLOCK_REQUESTS_PER_MIN: u32 = 3;

/// Ask a trusted device to approve unlocking this (requesting) device.
/// Returns the request id and the nonce an approver must sign.
pub(crate) async fn unlock_request(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !allow_authenticated_device(
        &state,
        authed.device.id,
        "unlock-request",
        UNLOCK_REQUESTS_PER_MIN,
    ) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "rate limited",
                "hint": "too many attempts; try again later",
            })),
        ));
    }
    let (id, nonce) = identity::create_unlock_request(&state.db, authed.user.id, authed.device.id)
        .await
        .map_err(internal)?;
    // Enqueue only: a full queue or an unreachable ntfy never delays or fails
    // the request.
    state.notifier.notify_approval(
        crate::notify::NotifyEvent::UnlockRequested,
        authed.user.id,
        Some(authed.device.id),
    );
    record_security_event(
        &state,
        Some(authed.device.id),
        "unlock.request",
        "created",
        Some(&id.to_string()),
    )
    .await;
    Ok(Json(json!({
        "request_id": id,
        "nonce": hex::encode(nonce),
    })))
}

/// Longest a `?wait=` long-poll will hold a request open (seconds).
const UNLOCK_WAIT_CAP_SECS: u64 = 25;

/// Parse and clamp the `?wait=` long-poll seconds from query params.
fn wait_secs(params: &HashMap<String, String>) -> u64 {
    params
        .get("wait")
        .and_then(|w| w.parse::<u64>().ok())
        .unwrap_or(0)
        .min(UNLOCK_WAIT_CAP_SECS)
}

/// Poll the status of an unlock request: `pending` | `approved` | `denied` | `expired`.
///
/// With `?wait=<secs>` the request long-polls: it returns as soon as the status
/// leaves `pending` (near-instant approval), or after the timeout still pending.
pub(crate) async fn unlock_status(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut ticks = wait_secs(&params) * 2; // 500ms per tick
    loop {
        match identity::unlock_request_status(&state.db, id, authed.user.id)
            .await
            .map_err(internal)?
        {
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "unlock request not found" })),
                ))
            }
            Some(status) if status != "pending" => return Ok(Json(json!({ "status": status }))),
            Some(status) => {
                if ticks == 0 {
                    return Ok(Json(json!({ "status": status })));
                }
                ticks -= 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// Unlock requests this device can approve (same user, not itself). With
/// `?wait=<secs>` it long-polls: returns as soon as a request appears.
pub(crate) async fn unlock_pending(
    authed: Authed,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut ticks = wait_secs(&params) * 2;
    let requests = loop {
        let requests =
            identity::pending_unlock_requests(&state.db, authed.user.id, authed.device.id)
                .await
                .map_err(internal)?;
        if !requests.is_empty() || ticks == 0 {
            break requests;
        }
        ticks -= 1;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    let items: Vec<Value> = requests
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "device_name": r.requesting_device_name,
                "platform": r.requesting_device_platform,
                "nonce": hex::encode(&r.nonce),
                "created_at": r.created_at.unix_timestamp(),
            })
        })
        .collect();
    Ok(Json(json!({ "requests": items })))
}

/// A device-signed approval: a hex Ed25519 signature over a pending nonce. Shared
/// by the unlock flow and the agent pending-action approval.
/// Approve an unlock request by signing its nonce with this device's key.
pub(crate) async fn unlock_approve(
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
    if identity::approve_unlock_request(&state.db, id, authed.user.id, authed.device.id, &signature)
        .await
        .is_err()
    {
        record_security_event(
            &state,
            Some(authed.device.id),
            "unlock.approve",
            "fail",
            None,
        )
        .await;
        return Err(unauthorized());
    }
    record_security_event(&state, Some(authed.device.id), "unlock.approve", "ok", None).await;
    Ok(Json(json!({ "status": "approved" })))
}

/// Deny (cancel) a pending unlock request from this device.
pub(crate) async fn unlock_deny(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    identity::deny_unlock_request(&state.db, id, authed.user.id, authed.device.id)
        .await
        .map_err(|_| unauthorized())?;
    record_security_event(&state, Some(authed.device.id), "unlock.deny", "ok", None).await;
    Ok(Json(json!({ "status": "denied" })))
}

/// Revoke one of the authenticated user's devices.
pub(crate) async fn delete_device(
    _authed: Authed,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Err((
        StatusCode::PRECONDITION_REQUIRED,
        Json(json!({"error":"device-signed revocation approval required"})),
    ))
}
