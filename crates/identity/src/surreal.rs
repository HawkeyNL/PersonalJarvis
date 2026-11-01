//! SurrealDB identity repository.
//!
//! These operations deliberately use conditional mutations for consuming a
//! challenge. The signature is verified before the mutation, but only the
//! successful `UPDATE ... WHERE consumed_at IS NONE` claimant may issue a
//! session. This preserves the SQL implementation's one-use challenge rule
//! without relying on a read-then-write race.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use sha2::Digest;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_store::Database;

#[path = "account.rs"]
pub mod account;

#[cfg(unix)]
#[path = "local_devices.rs"]
pub mod local_devices;

use super::{
    verify_signature, Authenticated, Challenge, Device, DeviceKey, IdentityError, LoginResult,
    PairingDetails, PairingRequest, Platform, Session, UnlockRequest, User,
};

const USER_FIELDS: &str = "record::id(id) AS id, display_name, status, created_at, updated_at";
const DEVICE_FIELDS: &str =
    "record::id(id) AS id, user_id, name, platform, status, created_at, updated_at, last_seen_at";
const KEY_FIELDS: &str =
    "record::id(id) AS id, device_id, algorithm, public_key, created_at, revoked_at";
const SESSION_FIELDS: &str = "record::id(id) AS id, user_id, device_id, token_hash, created_at, expires_at, last_used_at, revoked_at";

async fn one<T: DeserializeOwned, B: Serialize + 'static>(
    db: &Database,
    query: &str,
    bindings: B,
) -> Result<Option<T>, IdentityError> {
    let mut response = db
        .query(query)
        .bind(bindings)
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    response.take(0).map_err(|_| IdentityError::DatabaseSurreal)
}

async fn many<T: DeserializeOwned, B: Serialize + 'static>(
    db: &Database,
    query: &str,
    bindings: B,
) -> Result<Vec<T>, IdentityError> {
    let mut response = db
        .query(query)
        .bind(bindings)
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    response.take(0).map_err(|_| IdentityError::DatabaseSurreal)
}

async fn execute<B: Serialize + 'static>(
    db: &Database,
    query: &str,
    bindings: B,
) -> Result<(), IdentityError> {
    db.query(query)
        .bind(bindings)
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?
        .check()
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    Ok(())
}

#[derive(Serialize)]
struct DeviceKeyBindings {
    device_id: String,
    user_id: String,
    name: String,
    platform: String,
    key_id: String,
    algorithm: String,
    #[serde(with = "serde_bytes")]
    public_key: Vec<u8>,
}

#[derive(Serialize)]
struct ChallengeBindings {
    id: String,
    device_id: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
}

#[derive(Serialize)]
struct TokenHashBindings {
    #[serde(with = "serde_bytes")]
    token_hash: Vec<u8>,
}

#[derive(Serialize)]
struct SessionBindings {
    id: String,
    user_id: String,
    device_id: String,
    #[serde(with = "serde_bytes")]
    token_hash: Vec<u8>,
    account_revision: String,
}

#[derive(Serialize)]
struct UnlockCreateBindings {
    id: String,
    user_id: String,
    requesting_device_id: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
}

/// Create the single Jarvis owner.
pub async fn create_user(db: &Database, display_name: &str) -> Result<User, IdentityError> {
    let id = Uuid::now_v7();
    execute(
        db,
        "CREATE users SET id = $id, display_name = $display_name, status = 'active', \
         created_at = time::now(), updated_at = time::now() RETURN AFTER",
        json!({ "id": id.to_string(), "display_name": display_name }),
    )
    .await?;
    get_user(db, id)
        .await?
        .ok_or(IdentityError::DatabaseSurreal)
}

pub async fn get_user(db: &Database, id: Uuid) -> Result<Option<User>, IdentityError> {
    one(
        db,
        &format!("SELECT {USER_FIELDS} FROM users WHERE record::id(id) = $id LIMIT 1"),
        json!({ "id": id.to_string() }),
    )
    .await
}

pub async fn first_user_or_create(
    db: &Database,
    display_name: &str,
) -> Result<User, IdentityError> {
    if let Some(user) = one(
        db,
        &format!("SELECT {USER_FIELDS} FROM users ORDER BY created_at ASC LIMIT 1"),
        json!({}),
    )
    .await?
    {
        return Ok(user);
    }
    create_user(db, display_name).await
}

/// Return the existing owner without ever creating one. Public pairing must
/// not be able to manufacture an owner identity.
pub async fn first_user(db: &Database) -> Result<Option<User>, IdentityError> {
    one(
        db,
        &format!("SELECT {USER_FIELDS} FROM users ORDER BY created_at ASC LIMIT 1"),
        json!({}),
    )
    .await
}

pub async fn active_device_count(db: &Database) -> Result<u64, IdentityError> {
    #[derive(serde::Deserialize)]
    struct CountRow {
        count: u64,
    }
    let row: Option<CountRow> = one(
        db,
        "SELECT count() AS count FROM devices WHERE status = 'active' GROUP ALL",
        json!({}),
    )
    .await?;
    Ok(row.map_or(0, |row| row.count))
}

/// Claim the global bootstrap latch before registering the first device. The
/// latch is deliberately fail-closed: a process crash after the claim requires
/// local/root recovery, rather than reopening public enrollment.
pub async fn bootstrap_register_first_device(
    db: &Database,
    name: &str,
    platform: Platform,
    public_key: &[u8],
) -> Result<(User, Device), IdentityError> {
    if active_device_count(db).await? != 0 {
        return Err(IdentityError::AuthFailed);
    }
    let claim = db
        .query("CREATE ONLY bootstrap_state:owner SET id = 'owner', claimed_at = time::now(), device_id = $device_id")
        .bind(json!({ "device_id": "bootstrap-claimed" }))
        .await
        .map_err(|_| IdentityError::AuthFailed)?;
    claim.check().map_err(|_| IdentityError::AuthFailed)?;
    let user = first_user_or_create(db, "Jarvis user").await?;
    let (device, _) = register_device(db, user.id, name, platform, "ed25519", public_key).await?;
    Ok((user, device))
}

#[derive(Serialize)]
struct PairingCreateBindings {
    id: String,
    user_id: String,
    name: String,
    platform: String,
    #[serde(with = "serde_bytes")]
    public_key: Vec<u8>,
    fingerprint: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    ip: Option<String>,
    user_agent: Option<String>,
    app_version: Option<String>,
    model: Option<String>,
}

#[derive(serde::Deserialize)]
struct PairingRow {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    user_id: Uuid,
    candidate_name: String,
    candidate_platform: String,
    #[serde(with = "serde_bytes")]
    candidate_public_key: Vec<u8>,
    candidate_fingerprint: String,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    status: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    // Absent (NONE) on rows written before schema v13 or without a value.
    #[serde(default)]
    candidate_ip: Option<String>,
    #[serde(default)]
    candidate_user_agent: Option<String>,
    #[serde(default)]
    candidate_app_version: Option<String>,
    #[serde(default)]
    candidate_model: Option<String>,
}

fn pairing_from_row(row: PairingRow) -> PairingRequest {
    PairingRequest {
        id: row.id,
        user_id: row.user_id,
        candidate_name: row.candidate_name,
        candidate_platform: row.candidate_platform,
        candidate_public_key: row.candidate_public_key,
        candidate_fingerprint: row.candidate_fingerprint,
        nonce: row.nonce,
        status: row.status,
        created_at: row.created_at,
        expires_at: row.expires_at,
        details: PairingDetails {
            ip: row.candidate_ip,
            user_agent: row.candidate_user_agent,
            app_version: row.candidate_app_version,
            model: row.candidate_model,
        },
    }
}

/// Creates at most one pending request for a candidate key. A duplicate is
/// rejected instead of becoming notification spam for trusted devices.
pub async fn create_pairing_request(
    db: &Database,
    user_id: Uuid,
    name: &str,
    platform: Platform,
    public_key: &[u8],
    details: &PairingDetails,
) -> Result<PairingRequest, IdentityError> {
    let fingerprint = hex::encode(sha2::Sha256::digest(public_key));
    let duplicate: Option<PairingRow> = one(
        db,
        "SELECT record::id(id) AS id, user_id, candidate_name, candidate_platform, candidate_public_key, candidate_fingerprint, nonce, status, created_at, expires_at, candidate_ip, candidate_user_agent, candidate_app_version, candidate_model FROM device_pairing_requests WHERE candidate_fingerprint = $fingerprint AND status = 'pending' AND expires_at > time::now() LIMIT 1",
        json!({ "fingerprint": fingerprint }),
    ).await?;
    if duplicate.is_some() {
        return Err(IdentityError::AuthFailed);
    }
    let mut nonce = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let id = Uuid::now_v7();
    execute(db,
        "CREATE device_pairing_requests SET id = $id, user_id = $user_id, candidate_name = $name, candidate_platform = $platform, candidate_public_key = <bytes>$public_key, candidate_fingerprint = $fingerprint, nonce = <bytes>$nonce, status = 'pending', created_at = time::now(), expires_at = time::now() + 5m, resolved_at = NONE, approved_by_device_id = NONE, activated_device_id = NONE, candidate_ip = $ip, candidate_user_agent = $user_agent, candidate_app_version = $app_version, candidate_model = $model RETURN NONE",
        PairingCreateBindings { id: id.to_string(), user_id: user_id.to_string(), name: name.to_string(), platform: platform.as_str().to_string(), public_key: public_key.to_vec(), fingerprint, nonce, ip: details.ip.clone(), user_agent: details.user_agent.clone(), app_version: details.app_version.clone(), model: details.model.clone() },
    ).await?;
    pairing_request(db, id, user_id)
        .await?
        .ok_or(IdentityError::DatabaseSurreal)
}

async fn pairing_request(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
) -> Result<Option<PairingRequest>, IdentityError> {
    let row: Option<PairingRow> = one(db,
        "SELECT record::id(id) AS id, user_id, candidate_name, candidate_platform, candidate_public_key, candidate_fingerprint, nonce, status, created_at, expires_at, candidate_ip, candidate_user_agent, candidate_app_version, candidate_model FROM device_pairing_requests WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1",
        json!({ "id": id.to_string(), "user_id": user_id.to_string() }),
    ).await?;
    Ok(row.map(pairing_from_row))
}

pub async fn pairing_request_status(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
) -> Result<Option<String>, IdentityError> {
    Ok(pairing_request(db, id, user_id).await?.map(|request| {
        if request.status == "pending" && request.expires_at < OffsetDateTime::now_utc() {
            "expired".to_string()
        } else {
            request.status
        }
    }))
}

pub async fn pairing_status_for_candidate(
    db: &Database,
    id: Uuid,
    nonce: &[u8],
) -> Result<Option<(String, Option<Uuid>, bool)>, IdentityError> {
    #[derive(serde::Deserialize)]
    struct StatusRow {
        #[serde(with = "serde_bytes")]
        nonce: Vec<u8>,
        status: String,
        activated_device_id: Option<String>,
        #[serde(with = "time::serde::rfc3339")]
        expires_at: OffsetDateTime,
    }
    let row: Option<StatusRow> = one(
        db,
        "SELECT nonce, status, activated_device_id, expires_at FROM device_pairing_requests WHERE record::id(id) = $id LIMIT 1",
        json!({ "id": id.to_string() }),
    ).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.nonce != nonce {
        return Err(IdentityError::AuthFailed);
    }
    let just_expired = row.status == "pending" && row.expires_at <= OffsetDateTime::now_utc();
    if just_expired {
        execute(
            db,
            "UPDATE device_pairing_requests SET status = 'expired', resolved_at = time::now() WHERE record::id(id) = $id AND status = 'pending' AND expires_at <= time::now() RETURN NONE",
            json!({ "id": id.to_string() }),
        )
        .await?;
    }
    let status = if just_expired {
        "expired".to_string()
    } else {
        row.status
    };
    let device_id = row.activated_device_id.and_then(|id| id.parse().ok());
    Ok(Some((status, device_id, just_expired)))
}

pub async fn pending_pairing_requests(
    db: &Database,
    user_id: Uuid,
) -> Result<Vec<PairingRequest>, IdentityError> {
    let rows: Vec<PairingRow> = many(db,
        "SELECT record::id(id) AS id, user_id, candidate_name, candidate_platform, candidate_public_key, candidate_fingerprint, nonce, status, created_at, expires_at, candidate_ip, candidate_user_agent, candidate_app_version, candidate_model FROM device_pairing_requests WHERE user_id = $user_id AND status = 'pending' AND expires_at > time::now() ORDER BY created_at DESC LIMIT 20",
        json!({ "user_id": user_id.to_string() }),
    ).await?;
    Ok(rows.into_iter().map(pairing_from_row).collect())
}

/// Verify a request-bound approval, then conditionally consume and activate it
/// in one SurrealDB transaction. A replay, expiry or crash cannot leave a
/// consumed request without its corresponding active device.
pub async fn approve_pairing_request(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
    approver_device_id: Uuid,
    signature: &[u8],
) -> Result<Device, IdentityError> {
    let request = pairing_request(db, id, user_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    if request.status != "pending" || request.expires_at <= OffsetDateTime::now_utc() {
        return Err(IdentityError::AuthFailed);
    }
    let message = super::pairing_approval_message(
        id,
        &request.nonce,
        &request.candidate_public_key,
        user_id,
        approver_device_id,
        request.expires_at,
    )?;
    verify_device_signature(db, user_id, approver_device_id, &message, signature).await?;
    activate_pairing_request(db, request, Some(approver_device_id)).await
}

#[derive(Serialize)]
struct PairingActivationBindings {
    id: String,
    approver_device_id: Option<String>,
    fingerprint: String,
}

// Shared atomic activation: callers first prove either the remote device
// signature or the narrowly authorized local kernel-root peer.
async fn activate_pairing_request(
    db: &Database,
    request: PairingRequest,
    approver_device_id: Option<Uuid>,
) -> Result<Device, IdentityError> {
    let id = request.id;
    let user_id = request.user_id;
    let device_id = Uuid::now_v7();
    let key_id = Uuid::now_v7();
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $claim = UPDATE device_pairing_requests \
               SET status = 'approved', approved_by_device_id = $approver_device_id, resolved_at = time::now() \
               WHERE record::id(id) = $id AND user_id = $user_id AND candidate_fingerprint = $fingerprint AND status = 'pending' AND expires_at > time::now() RETURN AFTER; \
             IF array::len($claim) = 0 { THROW 'pairing request unavailable'; }; \
             CREATE devices SET id = $device_id, user_id = $user_id, name = $name, platform = $platform, \
               status = 'active', created_at = time::now(), updated_at = time::now(); \
             CREATE device_keys SET id = $key_id, device_id = $device_id, algorithm = 'ed25519', \
               public_key = <bytes>$public_key, created_at = time::now(), revoked_at = NONE; \
             UPDATE device_pairing_requests SET activated_device_id = $device_id WHERE record::id(id) = $id; \
             COMMIT TRANSACTION;",
        )
        .bind(DeviceKeyBindings {
            device_id: device_id.to_string(),
            user_id: user_id.to_string(),
            name: request.candidate_name,
            platform: Platform::parse(&request.candidate_platform)?.as_str().to_string(),
            key_id: key_id.to_string(),
            algorithm: "ed25519".to_string(),
            public_key: request.candidate_public_key,
        })
        .bind(PairingActivationBindings {
            id: id.to_string(),
            approver_device_id: approver_device_id.map(|id| id.to_string()),
            fingerprint: request.candidate_fingerprint,
        })
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    response.check().map_err(|_| IdentityError::AuthFailed)?;
    get_device(db, device_id)
        .await?
        .ok_or(IdentityError::DatabaseSurreal)
}

pub async fn deny_pairing_request(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
    denier_device_id: Uuid,
) -> Result<(), IdentityError> {
    let denier = get_device(db, denier_device_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    if denier.user_id != user_id || denier.status != "active" {
        return Err(IdentityError::AuthFailed);
    }
    let claimed: Option<ClaimedRecord> = one(db,
        "UPDATE device_pairing_requests SET status = 'denied', approved_by_device_id = $denier_device_id, resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user_id AND status = 'pending' AND expires_at > time::now() RETURN record::id(id) AS id",
        json!({ "id": id.to_string(), "user_id": user_id.to_string(), "denier_device_id": denier_device_id.to_string() }),
    ).await?;
    if claimed.map(|claim| claim.id) != Some(id.to_string()) {
        return Err(IdentityError::AuthFailed);
    }
    Ok(())
}

/// Register the device and key in one SurrealDB transaction. Neither record is
/// visible if the key insertion fails.
pub async fn register_device(
    db: &Database,
    user_id: Uuid,
    name: &str,
    platform: Platform,
    algorithm: &str,
    public_key: &[u8],
) -> Result<(Device, DeviceKey), IdentityError> {
    let device_id = Uuid::now_v7();
    let key_id = Uuid::now_v7();
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             CREATE devices SET id = $device_id, user_id = $user_id, name = $name, \
                platform = $platform, status = 'active', created_at = time::now(), \
                updated_at = time::now(); \
             CREATE device_keys SET id = $key_id, device_id = $device_id, algorithm = $algorithm, \
                public_key = <bytes>$public_key, created_at = time::now(), revoked_at = NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(DeviceKeyBindings {
            device_id: device_id.to_string(),
            user_id: user_id.to_string(),
            name: name.to_string(),
            platform: platform.as_str().to_string(),
            key_id: key_id.to_string(),
            algorithm: algorithm.to_string(),
            public_key: public_key.to_vec(),
        })
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    response
        .check()
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    let device = get_device(db, device_id)
        .await?
        .ok_or(IdentityError::DatabaseSurreal)?;
    let key: DeviceKey = one(
        db,
        &format!("SELECT {KEY_FIELDS} FROM device_keys WHERE record::id(id) = $id LIMIT 1"),
        json!({ "id": key_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::DatabaseSurreal)?;
    Ok((device, key))
}

pub async fn list_active_devices(
    db: &Database,
    user_id: Uuid,
) -> Result<Vec<Device>, IdentityError> {
    many(
        db,
        &format!("SELECT {DEVICE_FIELDS} FROM devices WHERE user_id = $user_id AND status = 'active' ORDER BY created_at DESC"),
        json!({ "user_id": user_id.to_string() }),
    )
    .await
}

pub async fn revoke_device(db: &Database, device_id: Uuid) -> Result<(), IdentityError> {
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             UPDATE devices SET status = 'revoked', updated_at = time::now() WHERE record::id(id) = $id; \
             UPDATE device_keys SET revoked_at = time::now() WHERE device_id = $id AND revoked_at IS NONE; \
             UPDATE sessions SET revoked_at = time::now() WHERE device_id = $id AND revoked_at IS NONE; \
             DELETE device_push_targets WHERE device_id = $id; \
             UPDATE account_recoveries SET status = 'expired', verifier = NONE, slot = record::id(id), \
             resolved_at = time::now() WHERE device_id = $id AND status = 'pending' RETURN NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({ "id": device_id.to_string() }))
        .await
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    response
        .check()
        .map_err(|_| IdentityError::DatabaseSurreal)?;
    Ok(())
}

/// True when `$target` is registered to a device of another user than
/// `$device`'s (or `$device` does not exist).
const FOREIGN_PUSH_TARGET: &str = "array::len(SELECT id FROM device_push_targets \
    WHERE target = $target AND device_id NOT IN (SELECT VALUE record::id(id) FROM devices \
    WHERE user_id IN (SELECT VALUE user_id FROM devices WHERE record::id(id) = $device))) != 0";

/// Register or replace the APNs target of an active device. A token another
/// device of the same user registered before (the same phone re-paired) moves
/// to this device, so one phone never receives a push twice or as the
/// requesting device. A token held by another user's device is refused with
/// `AuthFailed`: it never moves across accounts. Token and environment are
/// validated by the caller and again by the schema.
pub async fn set_push_target(
    db: &Database,
    device_id: Uuid,
    target: &str,
    apns_env: &str,
) -> Result<(), IdentityError> {
    let bindings = || json!({ "device": device_id.to_string(), "target": target, "env": apns_env });
    let result = execute(
        db,
        &format!(
            "BEGIN TRANSACTION; \
             LET $active = SELECT id FROM devices WHERE record::id(id) = $device AND status = 'active'; \
             IF array::len($active) != 1 {{ THROW 'device unavailable'; }}; \
             IF {FOREIGN_PUSH_TARGET} {{ THROW 'push target unavailable'; }}; \
             DELETE device_push_targets WHERE device_id = $device OR target = $target; \
             CREATE device_push_targets SET id = $device, device_id = $device, kind = 'apns', \
                target = $target, apns_env = $env, updated_at = time::now() RETURN NONE; \
             COMMIT TRANSACTION;"
        ),
        bindings(),
    )
    .await;
    if result.is_err()
        && one::<bool, _>(db, &format!("RETURN {FOREIGN_PUSH_TARGET}"), bindings()).await?
            == Some(true)
    {
        return Err(IdentityError::AuthFailed);
    }
    result
}

pub async fn delete_push_target(db: &Database, device_id: Uuid) -> Result<(), IdentityError> {
    execute(
        db,
        "DELETE device_push_targets WHERE device_id = $device",
        json!({ "device": device_id.to_string() }),
    )
    .await
}

/// APNs tokens of the user's active devices in one environment, without the
/// `except` device, at most `limit`.
pub async fn apns_targets(
    db: &Database,
    user_id: Uuid,
    except: Option<Uuid>,
    apns_env: &str,
    limit: usize,
) -> Result<Vec<String>, IdentityError> {
    many(
        db,
        "SELECT VALUE target FROM device_push_targets WHERE kind = 'apns' AND apns_env = $env \
         AND device_id != $except AND device_id IN \
         (SELECT VALUE record::id(id) FROM devices WHERE user_id = $user AND status = 'active') \
         LIMIT $limit",
        json!({
            "user": user_id.to_string(),
            "except": except.map(|id| id.to_string()).unwrap_or_default(),
            "env": apns_env,
            "limit": limit,
        }),
    )
    .await
}

/// Forget a token APNs reported as no longer valid.
pub async fn delete_apns_target(db: &Database, target: &str) -> Result<(), IdentityError> {
    execute(
        db,
        "DELETE device_push_targets WHERE kind = 'apns' AND target = $target",
        json!({ "target": target }),
    )
    .await
}

pub async fn create_challenge(db: &Database, device_id: Uuid) -> Result<Challenge, IdentityError> {
    let mut nonce = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let id = Uuid::now_v7();
    // Anyone may request a challenge. One for a device that cannot log in is
    // answered but never stored, so anonymous callers cannot grow the table.
    let active: Option<DeviceOwner> = one(
        db,
        "SELECT user_id FROM devices WHERE record::id(id) = $id AND status = 'active' LIMIT 1",
        json!({ "id": device_id.to_string() }),
    )
    .await?;
    if active.is_none() {
        return Ok(Challenge { id, nonce });
    }
    execute(
        db,
        // Only this device's expired challenges are dropped here (indexed by
        // device_id); the hourly `prune_expired_auth_state` sweep clears the rest.
        "DELETE auth_challenges WHERE device_id = $device_id AND expires_at < time::now(); \
         CREATE auth_challenges SET id = $id, device_id = $device_id, nonce = <bytes>$nonce, \
         created_at = time::now(), expires_at = time::now() + 5m, consumed_at = NONE RETURN AFTER",
        ChallengeBindings {
            id: id.to_string(),
            device_id: device_id.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await?;
    Ok(Challenge { id, nonce })
}

/// Unapproved pairing requests stay this long after expiry, so a candidate
/// polling late still sees `expired`, then they are deleted.
const PAIRING_REQUEST_RETENTION: &str = "1d";
/// Rows deleted per statement and statements per sweep, so one sweep never
/// holds a huge transaction; a backlog drains over several hours.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;

fn prune_queries() -> [String; 3] {
    let batch = |table: &str, filter: &str, action: &str| {
        format!(
            "LET $ids = (SELECT VALUE id FROM {table} WHERE {filter} LIMIT {PRUNE_BATCH}); \
             {action} RETURN NONE; RETURN array::len($ids);"
        )
    };
    [
        batch("auth_challenges", "expires_at < time::now()", "DELETE $ids"),
        // Anonymous callers can create pending requests; approved ones are
        // kept as the record of how a device was paired.
        batch(
            "device_pairing_requests",
            &format!(
                "status != 'approved' AND expires_at < time::now() - {PAIRING_REQUEST_RETENTION}"
            ),
            "DELETE $ids",
        ),
        // The kept approved rows lose the candidate's IP address, User-Agent,
        // app version and model after the same retention.
        batch(
            "device_pairing_requests",
            &format!(
                "status = 'approved' AND expires_at < time::now() - {PAIRING_REQUEST_RETENTION} \
                 AND (candidate_ip IS NOT NONE OR candidate_user_agent IS NOT NONE \
                 OR candidate_app_version IS NOT NONE OR candidate_model IS NOT NONE)"
            ),
            "UPDATE $ids SET candidate_ip = NONE, candidate_user_agent = NONE, \
             candidate_app_version = NONE, candidate_model = NONE",
        ),
    ]
}

/// Delete expired login challenges and stale unapproved pairing requests, and
/// clear the candidate details of stale approved ones, in bounded batches.
/// Returns the number of rows deleted or cleared.
pub async fn prune_expired_auth_state(db: &Database) -> Result<usize, IdentityError> {
    let mut total = 0;
    for query in prune_queries() {
        for _ in 0..PRUNE_MAX_BATCHES {
            let deleted: Option<usize> = db
                .query(query.as_str())
                .await
                .map_err(|_| IdentityError::DatabaseSurreal)?
                .check()
                .map_err(|_| IdentityError::DatabaseSurreal)?
                .take(2)
                .map_err(|_| IdentityError::DatabaseSurreal)?;
            let deleted = deleted.unwrap_or(0);
            total += deleted;
            if deleted < PRUNE_BATCH {
                break;
            }
        }
    }
    Ok(total)
}

#[derive(serde::Deserialize)]
struct StoredChallenge {
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    consumed_at: Option<OffsetDateTime>,
}

#[derive(serde::Deserialize)]
struct KeyRow {
    #[serde(with = "serde_bytes")]
    public_key: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(serde::Deserialize)]
struct DeviceOwner {
    #[serde(with = "uuid::serde::hyphenated")]
    user_id: Uuid,
}

#[derive(serde::Deserialize)]
struct ClaimedRecord {
    id: String,
}

/// Verify and atomically consume a challenge before issuing a session. With
/// an account password set, this is a biometric renewal (see
/// [`BIOMETRIC_RENEWAL_PLATFORMS`]) or fails.
pub async fn login(
    db: &Database,
    device_id: Uuid,
    challenge_id: Uuid,
    signature: &[u8],
) -> Result<LoginResult, IdentityError> {
    login_with_password(db, device_id, challenge_id, signature, None).await
}

pub async fn login_with_password(
    db: &Database,
    device_id: Uuid,
    challenge_id: Uuid,
    signature: &[u8],
    password: Option<crate::password::AccountPassword>,
) -> Result<LoginResult, IdentityError> {
    let challenge: StoredChallenge = one(
        db,
        "SELECT nonce, expires_at, consumed_at FROM auth_challenges WHERE record::id(id) = $id AND device_id = $device_id LIMIT 1",
        json!({ "id": challenge_id.to_string(), "device_id": device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    if challenge.consumed_at.is_some() || challenge.expires_at < OffsetDateTime::now_utc() {
        return Err(IdentityError::AuthFailed);
    }
    let key: KeyRow = one(
        db,
        "SELECT public_key, created_at FROM device_keys WHERE device_id = $device_id AND revoked_at IS NONE ORDER BY created_at DESC LIMIT 1",
        json!({ "device_id": device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    let _key_created_at = key.created_at;
    let message = jarvis_client_core::login_message(challenge_id, device_id, &challenge.nonce)
        .map_err(|_| IdentityError::AuthFailed)?;
    verify_signature(&key.public_key, &message, signature)?;
    let owner: DeviceOwner = one(
        db,
        "SELECT user_id FROM devices WHERE record::id(id) = $id AND status = 'active' LIMIT 1",
        json!({ "id": device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;

    // This conditional claim is the replay boundary: only one concurrent caller
    // can observe a returned record and proceed to mint a session. It runs
    // before the password check, so a wrong password burns the challenge and
    // every guess needs a fresh signature from the device key.
    let claimed: Option<ClaimedRecord> = one(
        db,
        "UPDATE auth_challenges SET consumed_at = time::now() WHERE record::id(id) = $id AND device_id = $device_id \
         AND consumed_at IS NONE AND expires_at > time::now() RETURN record::id(id) AS id",
        json!({ "id": challenge_id.to_string(), "device_id": device_id.to_string() }),
    )
    .await?;
    let Some(claimed) = claimed else {
        return Err(IdentityError::AuthFailed);
    };
    if claimed.id != challenge_id.to_string() {
        return Err(IdentityError::DatabaseSurreal);
    }

    // Without a password, a biometric device may renew from its last
    // password login instead. That path never checks a password, so it never
    // reports `PasswordRejected` and never counts toward the backoff.
    if password.is_none() && account::password_required(db, owner.user_id).await? {
        return renew_session(db, owner.user_id, device_id).await;
    }

    let account_revision = account::verify_password(
        db,
        crate::password::PasswordService::shared(),
        owner.user_id,
        password,
    )
    .await?;

    let (token, token_hash) = new_token();
    // A session minted after a password check anchors biometric renewal, for
    // as long as the password keeps this revision.
    let anchor = if account_revision.is_some() {
        "time::now(), password_revision = $account_revision"
    } else {
        "NONE, password_revision = NONE"
    };
    execute(
        db,
        &format!(
            "BEGIN TRANSACTION; \
             LET $account = SELECT revision FROM account_passwords WHERE user_id = $user_id; \
             IF (array::len($account) = 0 AND $account_revision != '') OR \
                (array::len($account) > 0 AND $account[0].revision != $account_revision) \
                {{ THROW 'authentication failed'; }}; \
             LET $active = SELECT id FROM devices WHERE record::id(id) = $device_id AND user_id = $user_id AND status = 'active'; \
             IF array::len($active) != 1 {{ THROW 'authentication failed'; }}; \
             CREATE sessions SET id = $id, user_id = $user_id, device_id = $device_id, token_hash = <bytes>$token_hash, \
             created_at = time::now(), expires_at = time::now() + 7d, last_used_at = NONE, revoked_at = NONE, \
             password_verified_at = {anchor}; \
             COMMIT TRANSACTION;"
        ),
        SessionBindings {
            id: Uuid::now_v7().to_string(),
            user_id: owner.user_id.to_string(),
            device_id: device_id.to_string(),
            token_hash: token_hash.clone(),
            account_revision: account_revision.unwrap_or_default(),
        },
    )
    .await?;
    issued(db, token, token_hash, false).await
}

/// Platforms whose device key needs a biometric check for every signature:
/// the iOS Keychain item is bound to `.biometryCurrentSet` and the Android
/// Keystore key requires strong biometrics per operation. Their signed login
/// challenge may renew a session without the account password. macOS is not
/// listed: Core cannot tell whether a Mac's key is biometry-bound, and Linux
/// and Windows keys are not.
pub const BIOMETRIC_RENEWAL_PLATFORMS: [&str; 2] = ["ios", "android"];

/// Days a password login keeps anchoring renewals. A renewed session never
/// outlives its anchor plus this window, so the password is asked again at
/// least this often.
const PASSWORD_RENEWAL_DAYS: u32 = 30;

#[derive(Serialize)]
struct RenewalBindings {
    id: String,
    user_id: String,
    device_id: String,
    #[serde(with = "serde_bytes")]
    token_hash: Vec<u8>,
    platforms: [&'static str; 2],
}

/// Replace the device's sessions with a new one carrying the password anchor
/// of its latest session. Refused unless that session is unrevoked, its
/// password login is less than [`PASSWORD_RENEWAL_DAYS`] old and was made
/// with the current password revision. Logout, password change or reset and
/// device revocation all revoke that session; the revision check also covers
/// a password login that committed beside a password change and so escaped
/// its revocation (write skew). Revoking the device's live sessions here
/// writes the anchor row, so a concurrent revocation conflicts with this
/// transaction instead of passing.
async fn renew_session(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<LoginResult, IdentityError> {
    let (token, token_hash) = new_token();
    execute(
        db,
        &renewal_query(),
        RenewalBindings {
            id: Uuid::now_v7().to_string(),
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
            token_hash: token_hash.clone(),
            platforms: BIOMETRIC_RENEWAL_PLATFORMS,
        },
    )
    .await
    .map_err(|_| IdentityError::AuthFailed)?;
    issued(db, token, token_hash, true).await
}

/// One transaction: check the device and its anchor, rotate, mint.
fn renewal_query() -> String {
    format!(
        "BEGIN TRANSACTION; \
         LET $active = SELECT id FROM devices WHERE record::id(id) = $device_id AND user_id = $user_id \
            AND status = 'active' AND platform IN $platforms; \
         IF array::len($active) != 1 {{ THROW 'authentication failed'; }}; \
         LET $anchor = SELECT created_at, revoked_at, password_verified_at, password_revision FROM sessions \
            WHERE device_id = $device_id AND user_id = $user_id ORDER BY created_at DESC LIMIT 1; \
         LET $verified = $anchor[0].password_verified_at; \
         LET $revision = $anchor[0].password_revision; \
         LET $account = SELECT revision FROM account_passwords WHERE user_id = $user_id; \
         IF array::len($anchor) != 1 OR $anchor[0].revoked_at IS NOT NONE OR $verified IS NONE \
            OR $verified <= time::now() - {PASSWORD_RENEWAL_DAYS}d OR $revision IS NONE \
            OR array::len($account) != 1 OR $account[0].revision != $revision \
            {{ THROW 'authentication failed'; }}; \
         UPDATE sessions SET revoked_at = time::now() WHERE device_id = $device_id AND revoked_at IS NONE RETURN NONE; \
         CREATE sessions SET id = $id, user_id = $user_id, device_id = $device_id, token_hash = <bytes>$token_hash, \
            created_at = time::now(), expires_at = time::min([time::now() + 7d, $verified + {PASSWORD_RENEWAL_DAYS}d]), \
            last_used_at = NONE, revoked_at = NONE, password_verified_at = $verified, \
            password_revision = $revision RETURN NONE; \
         COMMIT TRANSACTION;"
    )
}

/// A fresh random session token and its stored hash.
fn new_token() -> (Vec<u8>, Vec<u8>) {
    let mut token = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut token);
    let token_hash = sha2::Sha256::digest(&token).to_vec();
    (token, token_hash)
}

async fn issued(
    db: &Database,
    token: Vec<u8>,
    token_hash: Vec<u8>,
    renewed: bool,
) -> Result<LoginResult, IdentityError> {
    let session: Session = one(
        db,
        &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE token_hash = $token_hash LIMIT 1"),
        TokenHashBindings { token_hash },
    )
    .await?
    .ok_or(IdentityError::DatabaseSurreal)?;
    Ok(LoginResult {
        token: hex::encode(token),
        session,
        renewed,
    })
}

/// A session also ends after this long without use (ASVS 7.3.1). Every
/// authenticated request updates `last_used_at`; an open realtime connection
/// stays bounded by the absolute expiry and revocation.
const SESSION_IDLE: time::Duration = time::Duration::hours(24);

fn session_is_live(session: &Session, now: OffsetDateTime) -> bool {
    let last_activity = session.last_used_at.unwrap_or(session.created_at);
    session.revoked_at.is_none() && session.expires_at >= now && now - last_activity < SESSION_IDLE
}

pub async fn authenticate(db: &Database, token: &str) -> Result<Authenticated, IdentityError> {
    let raw = hex::decode(token).map_err(|_| IdentityError::AuthFailed)?;
    let token_hash = sha2::Sha256::digest(&raw).to_vec();
    let session: Session = one(
        db,
        &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE token_hash = $token_hash LIMIT 1"),
        TokenHashBindings { token_hash },
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    if !session_is_live(&session, OffsetDateTime::now_utc()) {
        return Err(IdentityError::AuthFailed);
    }
    let _updated: Option<serde_json::Value> = one(
        db,
        "UPDATE sessions SET last_used_at = time::now() WHERE record::id(id) = $id RETURN NONE",
        json!({ "id": session.id.to_string() }),
    )
    .await?;
    let user = get_user(db, session.user_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    let device: Device = one(
        db,
        &format!("SELECT {DEVICE_FIELDS} FROM devices WHERE record::id(id) = $id AND status = 'active' LIMIT 1"),
        json!({ "id": session.device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    Ok(Authenticated {
        user,
        device,
        session_id: session.id,
    })
}

pub async fn get_device(db: &Database, id: Uuid) -> Result<Option<Device>, IdentityError> {
    one(
        db,
        &format!("SELECT {DEVICE_FIELDS} FROM devices WHERE record::id(id) = $id LIMIT 1"),
        json!({ "id": id.to_string() }),
    )
    .await
}

/// Revalidate a previously authenticated long-lived connection without
/// retaining its bearer token. This does not authenticate a new request.
pub async fn session_is_active(
    db: &Database,
    session_id: Uuid,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<bool, IdentityError> {
    let session: Option<Session> = one(
        db,
        &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE record::id(id) = $id LIMIT 1"),
        json!({"id": session_id.to_string()}),
    )
    .await?;
    let Some(session) = session else {
        return Ok(false);
    };
    if session.user_id != user_id
        || session.device_id != device_id
        || session.revoked_at.is_some()
        || session.expires_at <= OffsetDateTime::now_utc()
    {
        return Ok(false);
    }
    let device: Option<Device> = one(
        db,
        &format!("SELECT {DEVICE_FIELDS} FROM devices WHERE record::id(id) = $id AND status = 'active' LIMIT 1"),
        json!({"id": device_id.to_string()}),
    ).await?;
    Ok(device.is_some_and(|device| device.user_id == user_id))
}

/// Log out: revoke the session and every other live session of its device,
/// so no older session of that device can anchor a biometric renewal.
pub async fn revoke_session(db: &Database, session_id: Uuid) -> Result<(), IdentityError> {
    let _: Option<serde_json::Value> = one(
        db,
        "UPDATE sessions SET revoked_at = time::now() WHERE revoked_at IS NONE AND device_id IN \
         (SELECT VALUE device_id FROM sessions WHERE record::id(id) = $id) RETURN NONE",
        json!({ "id": session_id.to_string() }),
    )
    .await?;
    Ok(())
}

/// Platforms whose device keys may sign approvals (pairing, unlock, account
/// and agent actions). Login and the local root path do not go through here.
/// The mobile apps require a biometric prompt for every approval, and the
/// macOS app requires Touch ID without a password fallback. Linux and Windows
/// cannot enforce a biometric-only prompt, so they never sign these approvals
/// (broker approvals use their own trusted-key check).
pub const APPROVER_PLATFORMS: [&str; 3] = ["ios", "android", "macos"];

pub async fn verify_device_signature(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
    message: &[u8],
    signature: &[u8],
) -> Result<(), IdentityError> {
    let (device, public_key) = active_device_and_key(db, user_id, device_id).await?;
    if !APPROVER_PLATFORMS.contains(&device.platform.as_str()) {
        return Err(IdentityError::AuthFailed);
    }
    verify_signature(&public_key, message, signature)
}

/// The current signing key of an active device owned by `user_id`: the key
/// [`verify_device_signature`] checks signatures against.
pub async fn active_device_public_key(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<Vec<u8>, IdentityError> {
    Ok(active_device_and_key(db, user_id, device_id).await?.1)
}

async fn active_device_and_key(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
) -> Result<(Device, Vec<u8>), IdentityError> {
    let device = get_device(db, device_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    if device.user_id != user_id || device.status != "active" {
        return Err(IdentityError::AuthFailed);
    }
    let key: KeyRow = one(
        db,
        "SELECT public_key, created_at FROM device_keys WHERE device_id = $device_id \
         AND revoked_at IS NONE ORDER BY created_at DESC LIMIT 1",
        json!({ "device_id": device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    let _key_created_at = key.created_at;
    Ok((device, key.public_key))
}

/// Create a short-lived cross-device unlock request. It supersedes the
/// device's older live pending requests: they become `denied` with no
/// deciding device and leave the approvers' list. Two concurrent creates can
/// still both stay pending; a write conflict with an approval fails this
/// transaction and the caller retries. This user's rows a day past expiry are
/// purged here, so the table stays bounded without a separate sweep; the
/// grace day keeps a late poll answering `expired` instead of not found. The
/// purge stays on the user's own rows so it never conflicts with another
/// user's unlock transactions.
pub async fn create_unlock_request(
    db: &Database,
    user_id: Uuid,
    requesting_device_id: Uuid,
) -> Result<(Uuid, Vec<u8>), IdentityError> {
    let mut nonce = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let id = Uuid::now_v7();
    execute(
        db,
        "BEGIN TRANSACTION; \
         DELETE unlock_requests WHERE user_id = $user_id AND expires_at < time::now() - 1d RETURN NONE; \
         UPDATE unlock_requests SET status = 'denied', resolved_at = time::now() \
           WHERE user_id = $user_id AND requesting_device_id = $requesting_device_id \
           AND status = 'pending' AND expires_at > time::now() RETURN NONE; \
         CREATE unlock_requests SET id = $id, user_id = $user_id, requesting_device_id = $requesting_device_id, \
         nonce = <bytes>$nonce, status = 'pending', approved_by_device_id = NONE, created_at = time::now(), \
         expires_at = time::now() + 2m, resolved_at = NONE RETURN NONE; \
         COMMIT TRANSACTION;",
        UnlockCreateBindings {
            id: id.to_string(),
            user_id: user_id.to_string(),
            requesting_device_id: requesting_device_id.to_string(),
            nonce: nonce.clone(),
        },
    )
    .await?;
    Ok((id, nonce))
}

#[derive(serde::Deserialize)]
struct UnlockStatusRow {
    status: String,
    #[serde(default)]
    approved_by_device_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

pub async fn unlock_request_status(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
) -> Result<Option<String>, IdentityError> {
    let row: Option<UnlockStatusRow> = one(
        db,
        "SELECT status, approved_by_device_id, expires_at FROM unlock_requests \
         WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1",
        serde_json::json!({ "id": id.to_string(), "user_id": user_id.to_string() }),
    )
    .await?;
    Ok(row.map(|row| {
        // Convention: a deny without a deciding device was made by the system
        // (superseding, or `confirm_recovery` cancelling every pending unlock
        // request), never by a person: `deny_unlock_request` always records
        // the denier. Its requester keeps seeing `pending` until expiry, as
        // before superseding existed, so a client still polling it never
        // reports a false denial. An explicit `superseded` status would
        // need a schema migration (the `status` ASSERT in 0001), so the
        // marker is the missing device.
        let superseded = row.status == "denied" && row.approved_by_device_id.is_none();
        if row.status == "pending" || superseded {
            if row.expires_at < OffsetDateTime::now_utc() {
                "expired".to_string()
            } else {
                "pending".to_string()
            }
        } else {
            row.status
        }
    }))
}

#[derive(serde::Deserialize)]
struct PendingUnlockRow {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    requesting_device_id: Uuid,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

pub async fn pending_unlock_requests(
    db: &Database,
    user_id: Uuid,
    approver_device_id: Uuid,
) -> Result<Vec<UnlockRequest>, IdentityError> {
    let rows: Vec<PendingUnlockRow> = many(
        db,
        "SELECT record::id(id) AS id, requesting_device_id, nonce, created_at \
         FROM unlock_requests WHERE user_id = $user_id AND requesting_device_id != $approver_device_id \
           AND status = 'pending' AND expires_at > time::now() ORDER BY created_at DESC",
        serde_json::json!({
            "user_id": user_id.to_string(),
            "approver_device_id": approver_device_id.to_string(),
        }),
    )
    .await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in rows {
        let device = get_device(db, row.requesting_device_id)
            .await?
            .ok_or(IdentityError::AuthFailed)?;
        if device.user_id != user_id || device.status != "active" {
            continue;
        }
        pending.push(UnlockRequest {
            id: row.id,
            requesting_device_id: row.requesting_device_id,
            requesting_device_name: device.name,
            requesting_device_platform: device.platform,
            nonce: row.nonce,
            created_at: row.created_at,
        });
    }
    Ok(pending)
}

#[derive(serde::Deserialize)]
struct UnlockClaim {
    #[serde(with = "uuid::serde::hyphenated")]
    requesting_device_id: Uuid,
    #[serde(with = "serde_bytes")]
    nonce: Vec<u8>,
}

pub async fn approve_unlock_request(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
    approver_device_id: Uuid,
    signature: &[u8],
) -> Result<(), IdentityError> {
    let pending: UnlockClaim = one(
        db,
        "SELECT requesting_device_id, nonce FROM unlock_requests WHERE record::id(id) = $id \
         AND user_id = $user_id AND status = 'pending' AND expires_at > time::now() LIMIT 1",
        serde_json::json!({ "id": id.to_string(), "user_id": user_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;
    if pending.requesting_device_id == approver_device_id {
        return Err(IdentityError::AuthFailed);
    }
    let message =
        jarvis_client_core::unlock_approval_message(id, approver_device_id, &pending.nonce)
            .map_err(|_| IdentityError::AuthFailed)?;
    verify_device_signature(db, user_id, approver_device_id, &message, signature).await?;

    // Claiming repeats every decision-relevant predicate. A second concurrent
    // approval cannot overwrite the first device or resolve an expired request.
    let claimed: Option<ClaimedRecord> = one(
        db,
        "UPDATE unlock_requests SET status = 'approved', approved_by_device_id = $approver_device_id, \
         resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user_id \
         AND requesting_device_id != $approver_device_id AND status = 'pending' \
         AND expires_at > time::now() RETURN record::id(id) AS id",
        serde_json::json!({
            "id": id.to_string(),
            "user_id": user_id.to_string(),
            "approver_device_id": approver_device_id.to_string(),
        }),
    )
    .await?;
    if claimed.map(|claim| claim.id) != Some(id.to_string()) {
        return Err(IdentityError::AuthFailed);
    }
    Ok(())
}

pub async fn deny_unlock_request(
    db: &Database,
    id: Uuid,
    user_id: Uuid,
    denier_device_id: Uuid,
) -> Result<(), IdentityError> {
    let claimed: Option<ClaimedRecord> = one(
        db,
        "UPDATE unlock_requests SET status = 'denied', approved_by_device_id = $denier_device_id, \
         resolved_at = time::now() WHERE record::id(id) = $id AND user_id = $user_id \
         AND requesting_device_id != $denier_device_id AND status = 'pending' \
         RETURN record::id(id) AS id",
        serde_json::json!({
            "id": id.to_string(),
            "user_id": user_id.to_string(),
            "denier_device_id": denier_device_id.to_string(),
        }),
    )
    .await?;
    if claimed.map(|claim| claim.id) != Some(id.to_string()) {
        return Err(IdentityError::AuthFailed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::env;

    use ed25519_dalek::{Signer, SigningKey};
    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;

    #[test]
    fn renewal_query_parses() {
        let query = renewal_query();
        assert!(query.contains("time::now() - 30d"));
        surrealdb::syn::parse(&query).unwrap();
    }

    #[test]
    fn sessions_end_on_revocation_expiry_or_idleness() {
        let now = OffsetDateTime::now_utc();
        let session = |last_used_at: Option<OffsetDateTime>| Session {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
            token_hash: Vec::new(),
            created_at: now - time::Duration::days(5),
            expires_at: now + time::Duration::days(2),
            last_used_at,
            revoked_at: None,
        };
        assert!(session_is_live(
            &session(Some(now - time::Duration::hours(1))),
            now
        ));
        assert!(!session_is_live(&session(Some(now - SESSION_IDLE)), now));
        // Never used since login: idleness counts from creation.
        assert!(!session_is_live(&session(None), now));
        let mut revoked = session(Some(now));
        revoked.revoked_at = Some(now);
        assert!(!session_is_live(&revoked, now));
        let mut expired = session(Some(now));
        expired.expires_at = now - time::Duration::seconds(1);
        assert!(!session_is_live(&expired, now));
    }

    /// Exercises the real SurrealDB wire protocol and the critical one-use
    /// challenge boundary. It is deliberately opt-in: CI will run it after the
    /// SurrealDB service replaces the current PostgreSQL test service.
    #[tokio::test]
    #[ignore = "requires JARVIS_SURREAL_TEST_ENDPOINT and a disposable SurrealDB server"]
    async fn challenge_login_cannot_be_replayed() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = env::var("JARVIS_SURREAL_TEST_ENDPOINT")?;
        let username = env::var("JARVIS_SURREAL_TEST_USER")?;
        let password = env::var("JARVIS_SURREAL_TEST_PASS")?;
        let namespace = format!("jarvis_test_{}", Uuid::now_v7().simple());
        let db = Surreal::new::<Ws>(&endpoint).await?;
        db.signin(Root {
            username: &username,
            password: &password,
        })
        .await?;
        db.use_ns(&namespace).use_db("core").await?;
        jarvis_store::apply_baseline_schema(&db).await?;

        let owner = create_user(&db, "Gus").await?;
        let signing = SigningKey::from_bytes(&rand::random());
        let (device, _) = register_device(
            &db,
            owner.id,
            "iPhone",
            Platform::Ios,
            "ed25519",
            &signing.verifying_key().to_bytes(),
        )
        .await?;
        let challenge = create_challenge(&db, device.id).await?;
        // A signature over the bare nonce is not a login proof.
        let raw_nonce = signing.sign(&challenge.nonce).to_bytes();
        assert!(matches!(
            login(&db, device.id, challenge.id, &raw_nonce).await,
            Err(IdentityError::AuthFailed)
        ));
        let signature = signing
            .sign(&jarvis_client_core::login_message(
                challenge.id,
                device.id,
                &challenge.nonce,
            )?)
            .to_bytes();
        let first_db = db.clone();
        let second_db = db.clone();
        let first_signature = signature;
        let second_signature = signature;
        let (first, second) = tokio::join!(
            login(&first_db, device.id, challenge.id, &first_signature),
            login(&second_db, device.id, challenge.id, &second_signature),
        );
        // The conditional UPDATE is the replay/concurrency boundary: exactly
        // one request can consume the challenge and obtain a session.
        let result = match (first, second) {
            (Ok(result), Err(_)) | (Err(_), Ok(result)) => result,
            _ => panic!("exactly one concurrent login must succeed"),
        };
        assert_eq!(authenticate(&db, &result.token).await?.user.id, owner.id);
        assert!(login(&db, device.id, challenge.id, &signature)
            .await
            .is_err());

        let approver_signing = SigningKey::from_bytes(&rand::random());
        let (approver, _) = register_device(
            &db,
            owner.id,
            "Pixel",
            Platform::Android,
            "ed25519",
            &approver_signing.verifying_key().to_bytes(),
        )
        .await?;
        let (unlock_id, unlock_nonce) = create_unlock_request(&db, owner.id, device.id).await?;
        assert_eq!(
            pending_unlock_requests(&db, owner.id, approver.id)
                .await?
                .len(),
            1
        );
        let unlock_message = |device_id| {
            jarvis_client_core::unlock_approval_message(unlock_id, device_id, &unlock_nonce)
        };
        let self_signature = signing.sign(&unlock_message(device.id)?).to_bytes();
        assert!(
            approve_unlock_request(&db, unlock_id, owner.id, device.id, &self_signature)
                .await
                .is_err()
        );
        // A signature over the bare nonce is not an approval.
        let raw_nonce = approver_signing.sign(&unlock_nonce).to_bytes();
        assert!(matches!(
            approve_unlock_request(&db, unlock_id, owner.id, approver.id, &raw_nonce).await,
            Err(IdentityError::AuthFailed)
        ));
        // A login proof over the same id, device and nonce is not an approval.
        let login_proof = approver_signing
            .sign(&jarvis_client_core::login_message(
                unlock_id,
                approver.id,
                &unlock_nonce,
            )?)
            .to_bytes();
        assert!(
            approve_unlock_request(&db, unlock_id, owner.id, approver.id, &login_proof)
                .await
                .is_err()
        );
        let approval_signature = approver_signing
            .sign(&unlock_message(approver.id)?)
            .to_bytes();
        approve_unlock_request(&db, unlock_id, owner.id, approver.id, &approval_signature).await?;
        assert_eq!(
            unlock_request_status(&db, unlock_id, owner.id)
                .await?
                .as_deref(),
            Some("approved")
        );
        // The conditional claim also rejects approval replay.
        assert!(
            approve_unlock_request(&db, unlock_id, owner.id, approver.id, &approval_signature,)
                .await
                .is_err()
        );

        // A new request supersedes only the same device's older pending one;
        // resolved requests and other devices' requests are left alone. The
        // superseded one leaves the approvers' list and cannot be approved,
        // but its requester still reads `pending` until it expires.
        let (other, _) = create_unlock_request(&db, owner.id, approver.id).await?;
        let (older, older_nonce) = create_unlock_request(&db, owner.id, device.id).await?;
        let (newer, _) = create_unlock_request(&db, owner.id, device.id).await?;
        let status = |id| unlock_request_status(&db, id, owner.id);
        assert_eq!(status(older).await?.as_deref(), Some("pending"));
        assert_eq!(status(newer).await?.as_deref(), Some("pending"));
        assert_eq!(status(other).await?.as_deref(), Some("pending"));
        assert_eq!(status(unlock_id).await?.as_deref(), Some("approved"));
        let pending: Vec<Uuid> = pending_unlock_requests(&db, owner.id, approver.id)
            .await?
            .iter()
            .map(|request| request.id)
            .collect();
        assert_eq!(pending, vec![newer]);
        let older_message =
            jarvis_client_core::unlock_approval_message(older, approver.id, &older_nonce)?;
        assert!(approve_unlock_request(
            &db,
            older,
            owner.id,
            approver.id,
            &approver_signing.sign(&older_message).to_bytes(),
        )
        .await
        .is_err());

        // Creating a request purges rows a day past expiry and keeps newer
        // expired ones answering `expired`.
        let (stale, recent) = (Uuid::now_v7(), Uuid::now_v7());
        db.query(
            "CREATE unlock_requests SET id = $stale, user_id = $user, requesting_device_id = $device, \
             nonce = <bytes>'n', status = 'pending', approved_by_device_id = NONE, \
             created_at = time::now() - 2d, expires_at = time::now() - 2d, resolved_at = NONE; \
             CREATE unlock_requests SET id = $recent, user_id = $user, requesting_device_id = $device, \
             nonce = <bytes>'n', status = 'pending', approved_by_device_id = NONE, \
             created_at = time::now() - 1h, expires_at = time::now() - 1h, resolved_at = NONE;",
        )
        .bind(("stale", stale.to_string()))
        .bind(("recent", recent.to_string()))
        .bind(("user", owner.id.to_string()))
        .bind(("device", approver.id.to_string()))
        .await?
        .check()?;
        assert_eq!(status(stale).await?.as_deref(), Some("expired"));
        create_unlock_request(&db, owner.id, device.id).await?;
        assert_eq!(status(stale).await?, None);
        assert_eq!(status(recent).await?.as_deref(), Some("expired"));

        // Pairing uses a different, canonical protocol. Its conditional update,
        // device creation and request activation are one database transaction:
        // exactly one concurrent approval may create an active candidate.
        let candidate = SigningKey::from_bytes(&rand::random());
        let pairing = create_pairing_request(
            &db,
            owner.id,
            "New Mac",
            Platform::Macos,
            &candidate.verifying_key().to_bytes(),
            &PairingDetails::default(),
        )
        .await?;
        assert_eq!(pairing.details, PairingDetails::default());
        let pairing_message = super::super::pairing_approval_message(
            pairing.id,
            &pairing.nonce,
            &pairing.candidate_public_key,
            owner.id,
            approver.id,
            pairing.expires_at,
        )?;
        let pairing_signature = approver_signing.sign(&pairing_message).to_bytes();
        let first_db = db.clone();
        let second_db = db.clone();
        let (first, second) = tokio::join!(
            approve_pairing_request(
                &first_db,
                pairing.id,
                owner.id,
                approver.id,
                &pairing_signature
            ),
            approve_pairing_request(
                &second_db,
                pairing.id,
                owner.id,
                approver.id,
                &pairing_signature
            ),
        );
        let paired = match (first, second) {
            (Ok(device), Err(_)) | (Err(_), Ok(device)) => device,
            _ => panic!("exactly one concurrent pairing approval must succeed"),
        };
        assert_eq!(paired.user_id, owner.id);
        assert_eq!(
            pairing_request_status(&db, pairing.id, owner.id)
                .await?
                .as_deref(),
            Some("approved")
        );
        assert!(approve_pairing_request(
            &db,
            pairing.id,
            owner.id,
            approver.id,
            &pairing_signature
        )
        .await
        .is_err());

        // Local host approval shares the same one-shot transaction, but must
        // not fabricate an approving enrolled-device ID. Exercise the NONE
        // binding against the real schema, independently of root IPC proof.
        let local_request = create_pairing_request(
            &db,
            owner.id,
            "Local fixture",
            Platform::Ios,
            &SigningKey::from_bytes(&rand::random())
                .verifying_key()
                .to_bytes(),
            &PairingDetails {
                ip: Some("2001:db8::1".into()),
                user_agent: Some("Jarvis/0.1.14".into()),
                app_version: Some("0.1.14".into()),
                model: Some("iPhone 16".into()),
            },
        )
        .await?;
        // Candidate details round-trip unchanged for the approving owner.
        let listed = pending_pairing_requests(&db, owner.id).await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].details, local_request.details);
        assert_eq!(local_request.details.model.as_deref(), Some("iPhone 16"));
        let local_id = local_request.id;
        let local_device = activate_pairing_request(&db, local_request, None).await?;
        assert_eq!(local_device.user_id, owner.id);
        let consumed = pairing_request(&db, local_id, owner.id).await?.unwrap();
        assert_eq!(consumed.status, "approved");
        assert!(activate_pairing_request(&db, consumed, None).await.is_err());

        // Only mobile and macOS device keys sign approvals.
        let message = b"approval fixture";
        for platform in [
            Platform::Linux,
            Platform::Windows,
            Platform::Macos,
            Platform::Ios,
            Platform::Android,
        ] {
            let signing = SigningKey::from_bytes(&rand::random());
            let (device, _) = register_device(
                &db,
                owner.id,
                platform.as_str(),
                platform,
                "ed25519",
                &signing.verifying_key().to_bytes(),
            )
            .await?;
            let signature = signing.sign(message).to_bytes();
            let result =
                verify_device_signature(&db, owner.id, device.id, message, &signature).await;
            if matches!(
                platform,
                Platform::Ios | Platform::Android | Platform::Macos
            ) {
                result?;
            } else {
                assert!(
                    matches!(result, Err(IdentityError::AuthFailed)),
                    "{platform:?}"
                );
                // The trusted-key and broker key lookup is unaffected.
                assert_eq!(
                    active_device_public_key(&db, owner.id, device.id).await?,
                    signing.verifying_key().to_bytes()
                );
            }
        }

        // A Linux desktop cannot approve pairing; the local root path still can.
        let desktop = SigningKey::from_bytes(&rand::random());
        let (desktop_device, _) = register_device(
            &db,
            owner.id,
            "Desktop",
            Platform::Linux,
            "ed25519",
            &desktop.verifying_key().to_bytes(),
        )
        .await?;
        let request = create_pairing_request(
            &db,
            owner.id,
            "New phone",
            Platform::Ios,
            &SigningKey::from_bytes(&rand::random())
                .verifying_key()
                .to_bytes(),
            &PairingDetails::default(),
        )
        .await?;
        let signature = desktop
            .sign(&super::super::pairing_approval_message(
                request.id,
                &request.nonce,
                &request.candidate_public_key,
                owner.id,
                desktop_device.id,
                request.expires_at,
            )?)
            .to_bytes();
        assert!(matches!(
            approve_pairing_request(&db, request.id, owner.id, desktop_device.id, &signature).await,
            Err(IdentityError::AuthFailed)
        ));
        let root_paired = activate_pairing_request(&db, request, None).await?;
        assert_eq!(root_paired.user_id, owner.id);

        // Revoking a device invalidates every existing bearer session at once;
        // a stolen token must not remain useful until its normal expiry.
        revoke_device(&db, device.id).await?;
        assert!(authenticate(&db, &result.token).await.is_err());
        Ok(())
    }

    #[test]
    fn prune_keeps_live_and_approved_rows() {
        let [challenges, pairing, details] = prune_queries();
        assert!(challenges.contains("FROM auth_challenges WHERE expires_at < time::now()"));
        assert!(pairing.contains("status != 'approved'"));
        assert!(pairing.contains("expires_at < time::now() - 1d"));
        // Approved rows are never deleted, only cleared of candidate details.
        assert!(details.contains("status = 'approved'"));
        assert!(details.contains("expires_at < time::now() - 1d"));
        assert!(
            details.contains("UPDATE $ids SET candidate_ip = NONE") && !details.contains("DELETE")
        );
        for query in [challenges, pairing, details] {
            assert!(query.contains(&format!("LIMIT {PRUNE_BATCH}")));
        }
    }

    #[tokio::test]
    #[ignore = "requires JARVIS_SURREAL_TEST_ENDPOINT and a disposable SurrealDB server"]
    async fn anonymous_auth_state_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let endpoint = env::var("JARVIS_SURREAL_TEST_ENDPOINT")?;
        let username = env::var("JARVIS_SURREAL_TEST_USER")?;
        let password = env::var("JARVIS_SURREAL_TEST_PASS")?;
        let namespace = format!("jarvis_test_{}", Uuid::now_v7().simple());
        let db = Surreal::new::<Ws>(&endpoint).await?;
        db.signin(Root {
            username: &username,
            password: &password,
        })
        .await?;
        db.use_ns(&namespace).use_db("core").await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let count = |table: &'static str| {
            let db = db.clone();
            async move {
                let rows: Option<usize> = db
                    .query(format!("RETURN count(SELECT id FROM {table})"))
                    .await?
                    .take(0)?;
                Ok::<usize, surrealdb::Error>(rows.unwrap_or(0))
            }
        };

        // A challenge for a device that cannot log in is answered, not stored.
        create_challenge(&db, Uuid::now_v7()).await?;
        assert_eq!(count("auth_challenges").await?, 0);
        let owner = create_user(&db, "Gus").await?;
        let signing = SigningKey::from_bytes(&rand::random());
        let (device, _) = register_device(
            &db,
            owner.id,
            "iPhone",
            Platform::Ios,
            "ed25519",
            &signing.verifying_key().to_bytes(),
        )
        .await?;
        create_challenge(&db, device.id).await?;
        assert_eq!(count("auth_challenges").await?, 1);

        // Expired challenges and stale unapproved pairing requests are swept;
        // live challenges and approved requests stay.
        db.query(
            "CREATE auth_challenges SET id = 'expired', device_id = $device, \
             nonce = <bytes>'n', created_at = time::now() - 1h, \
             expires_at = time::now() - 55m, consumed_at = NONE; \
             FOR $status IN ['pending', 'denied', 'approved'] { \
             CREATE device_pairing_requests SET id = 'old-' + $status, user_id = $user, \
             candidate_name = 'x', candidate_platform = 'linux', \
             candidate_public_key = <bytes>'k', candidate_fingerprint = $status, \
             nonce = <bytes>'n', status = $status, created_at = time::now() - 9d, \
             expires_at = time::now() - 8d, resolved_at = NONE, \
             approved_by_device_id = NONE, activated_device_id = NONE, \
             candidate_ip = '192.0.2.9', candidate_user_agent = 'ua'; };",
        )
        .bind(("device", device.id.to_string()))
        .bind(("user", owner.id.to_string()))
        .await?
        .check()?;
        // Three rows deleted, one approved row cleared of candidate details.
        assert_eq!(prune_expired_auth_state(&db).await?, 4);
        assert_eq!(count("auth_challenges").await?, 1);
        let mut kept = db
            .query(
                "SELECT VALUE status FROM device_pairing_requests; \
                 SELECT VALUE status FROM device_pairing_requests \
                 WHERE candidate_ip = NONE AND candidate_user_agent = NONE;",
            )
            .await?;
        let all: Vec<String> = kept.take(0)?;
        let cleared: Vec<String> = kept.take(1)?;
        assert_eq!(all, vec!["approved".to_string()]);
        assert_eq!(cleared, all);
        assert_eq!(prune_expired_auth_state(&db).await?, 0);
        Ok(())
    }
}
