//! Device-bound authentication and the trusted-device unlock flow. Enrollment is
//! dev-only; login verifies an Ed25519 signature over a single-use challenge and
//! issues a session token. The unlock flow lets an already-trusted device sign a
//! nonce to admit a new one. Every state-changing step is written to the security
//! audit trail via [`record_security_event`].

use std::collections::HashMap;

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use jarvis_identity as identity;

use crate::audit::record_security_event;
use crate::error::{bad_request, internal, unauthorized};
use crate::validation;
use crate::{AppState, Authed};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnrollReq {
    name: String,
    platform: String,
    /// Hex-encoded Ed25519 public key.
    public_key: String,
}

/// Dev-only device enrollment: create the single user if needed and register
/// the calling device with its public key. Disabled in production.
pub(crate) async fn auth_enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if state.environment == "production" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "enrollment is disabled in production" })),
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
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(policy) = state.bootstrap_enrollment.as_ref() else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "bootstrap unavailable" })),
        ));
    };
    // Reconstruct the trusted-peer decision from the real socket. A direct
    // caller cannot make an XFF header trusted; Caddy is the only configured
    // loopback peer allowed to supply one.
    let client_ip = if state.trusted_proxy_hops > 0 && state.trusted_proxy_ips.contains(&peer.ip())
    {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|raw| raw.split(',').next_back())
            .and_then(|ip| ip.trim().parse().ok())
    } else {
        Some(peer.ip())
    };
    if !client_ip.is_some_and(|ip| policy.allows(ip)) {
        record_security_event(&state, None, "auth.bootstrap", "rejected_network", None).await;
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
        record_security_event(&state, None, "auth.bootstrap", "invalid_secret", None).await;
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
    let (user, device) =
        identity::bootstrap_register_first_device(&state.db, &req.name, platform, &key)
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
pub(crate) async fn pairing_create(
    State(state): State<AppState>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "first device bootstrap required" })),
        ));
    }
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
    record_security_event(
        &state,
        None,
        "device.pairing_request",
        "created",
        Some(&pairing.candidate_fingerprint[..16]),
    )
    .await;
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
        record_security_event(
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairingApproveReq {
    signature: String,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChallengeReq {
    device_id: Uuid,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginReq {
    device_id: Uuid,
    challenge_id: Uuid,
    /// Hex-encoded Ed25519 signature over the challenge nonce.
    signature: String,
}

/// Verify a signed challenge and issue a session token.
pub(crate) async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
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
    let result = match identity::login(&state.db, req.device_id, req.challenge_id, &signature).await
    {
        Ok(r) => r,
        Err(_) => {
            record_security_event(&state, Some(req.device_id), "auth.login", "fail", None).await;
            return Err(unauthorized());
        }
    };
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

/// Ask a trusted device to approve unlocking this (requesting) device.
/// Returns the request id and the nonce an approver must sign.
pub(crate) async fn unlock_request(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (id, nonce) = identity::create_unlock_request(&state.db, authed.user.id, authed.device.id)
        .await
        .map_err(internal)?;
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApproveReq {
    /// Hex-encoded Ed25519 signature over the request nonce.
    pub(crate) signature: String,
}

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
    identity::approve_unlock_request(&state.db, id, authed.user.id, authed.device.id, &signature)
        .await
        .map_err(|_| unauthorized())?;
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
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match identity::get_device(&state.db, id)
        .await
        .map_err(internal)?
    {
        Some(device) if device.user_id == authed.user.id => {
            identity::revoke_device(&state.db, id)
                .await
                .map_err(internal)?;
            record_security_event(
                &state,
                Some(authed.device.id),
                "device.revoke",
                "ok",
                Some(&id.to_string()),
            )
            .await;
            Ok(Json(json!({ "status": "revoked" })))
        }
        _ => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "device not found" })),
        )),
    }
}
