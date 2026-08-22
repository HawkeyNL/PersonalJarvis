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

use super::{
    verify_signature, Authenticated, Challenge, Device, DeviceKey, IdentityError, LoginResult,
    Platform, Session, UnlockRequest, User,
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

pub async fn create_challenge(db: &Database, device_id: Uuid) -> Result<Challenge, IdentityError> {
    let mut nonce = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let id = Uuid::now_v7();
    execute(
        db,
        "CREATE auth_challenges SET id = $id, device_id = $device_id, nonce = <bytes>$nonce, \
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

/// Verify and atomically consume a challenge before issuing a session.
pub async fn login(
    db: &Database,
    device_id: Uuid,
    challenge_id: Uuid,
    signature: &[u8],
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
    verify_signature(&key.public_key, &challenge.nonce, signature)?;
    let owner: DeviceOwner = one(
        db,
        "SELECT user_id FROM devices WHERE record::id(id) = $id AND status = 'active' LIMIT 1",
        json!({ "id": device_id.to_string() }),
    )
    .await?
    .ok_or(IdentityError::AuthFailed)?;

    // This conditional claim is the replay boundary: only one concurrent caller
    // can observe a returned record and proceed to mint a session.
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

    let mut token = vec![0_u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut token);
    let token_hash = sha2::Sha256::digest(&token).to_vec();
    execute(
        db,
        "CREATE sessions SET id = $id, user_id = $user_id, device_id = $device_id, token_hash = <bytes>$token_hash, \
         created_at = time::now(), expires_at = time::now() + 7d, last_used_at = NONE, revoked_at = NONE RETURN AFTER",
        SessionBindings {
            id: Uuid::now_v7().to_string(),
            user_id: owner.user_id.to_string(),
            device_id: device_id.to_string(),
            token_hash: token_hash.clone(),
        },
    )
    .await?;
    let session: Session = one(
        db,
        &format!("SELECT {SESSION_FIELDS} FROM sessions WHERE token_hash = $token_hash LIMIT 1"),
        TokenHashBindings {
            token_hash: token_hash.clone(),
        },
    )
    .await?
    .ok_or(IdentityError::DatabaseSurreal)?;
    Ok(LoginResult {
        token: hex::encode(token),
        session,
    })
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
    if session.revoked_at.is_some() || session.expires_at < OffsetDateTime::now_utc() {
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

pub async fn revoke_session(db: &Database, session_id: Uuid) -> Result<(), IdentityError> {
    let _: Option<serde_json::Value> = one(
        db,
        "UPDATE sessions SET revoked_at = time::now() WHERE record::id(id) = $id AND revoked_at IS NONE RETURN NONE",
        json!({ "id": session_id.to_string() }),
    )
    .await?;
    Ok(())
}

pub async fn verify_device_signature(
    db: &Database,
    user_id: Uuid,
    device_id: Uuid,
    message: &[u8],
    signature: &[u8],
) -> Result<(), IdentityError> {
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
    verify_signature(&key.public_key, message, signature)
}

/// Create a short-lived cross-device unlock request.
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
        "CREATE unlock_requests SET id = $id, user_id = $user_id, requesting_device_id = $requesting_device_id, \
         nonce = <bytes>$nonce, status = 'pending', approved_by_device_id = NONE, created_at = time::now(), \
         expires_at = time::now() + 2m, resolved_at = NONE RETURN NONE",
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
        "SELECT status, expires_at FROM unlock_requests WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1",
        serde_json::json!({ "id": id.to_string(), "user_id": user_id.to_string() }),
    )
    .await?;
    Ok(row.map(|row| {
        if row.status == "pending" && row.expires_at < OffsetDateTime::now_utc() {
            "expired".to_string()
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
    verify_device_signature(db, user_id, approver_device_id, &pending.nonce, signature).await?;

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
        let signature = signing.sign(&challenge.nonce).to_bytes();
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
            "MacBook",
            Platform::Macos,
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
        let self_signature = signing.sign(&unlock_nonce).to_bytes();
        assert!(
            approve_unlock_request(&db, unlock_id, owner.id, device.id, &self_signature)
                .await
                .is_err()
        );
        let approval_signature = approver_signing.sign(&unlock_nonce).to_bytes();
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

        // Revoking a device invalidates every existing bearer session at once;
        // a stolen token must not remain useful until its normal expiry.
        revoke_device(&db, device.id).await?;
        assert!(authenticate(&db, &result.token).await.is_err());
        Ok(())
    }
}
