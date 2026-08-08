//! Identity domain: the single Jarvis user, their trusted devices, and
//! device-bound authentication (challenge-response login + sessions).
//!
//! Device private keys never leave the device (OS keychain); only public keys
//! are stored here. Login proves possession of the private key by signing a
//! server-issued nonce. Session tokens are stored only as SHA-256 hashes.

// `sqlx::Error` is a large third-party error we surface via `IdentityError`.
#![allow(clippy::result_large_err)]

use ed25519_dalek::{Signature, VerifyingKey};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// Errors returned by the identity repository.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("unknown platform: {0}")]
    UnknownPlatform(String),
    /// Deliberately opaque so callers can't distinguish failure reasons.
    #[error("authentication failed")]
    AuthFailed,
}

/// A device platform. Persisted as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Ios,
    Windows,
    Linux,
    Android,
}

impl Platform {
    /// The canonical lowercase string stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Macos => "macos",
            Platform::Ios => "ios",
            Platform::Windows => "windows",
            Platform::Linux => "linux",
            Platform::Android => "android",
        }
    }

    /// Parse a platform from its database/string form.
    pub fn parse(s: &str) -> Result<Self, IdentityError> {
        match s {
            "macos" => Ok(Platform::Macos),
            "ios" => Ok(Platform::Ios),
            "windows" => Ok(Platform::Windows),
            "linux" => Ok(Platform::Linux),
            "android" => Ok(Platform::Android),
            other => Err(IdentityError::UnknownPlatform(other.to_string())),
        }
    }
}

/// The account owner. Jarvis is single-user, but the model stays relational.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A trusted device belonging to a user.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub platform: String,
    pub status: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
}

/// A device's registered public key. The private key never leaves the device.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DeviceKey {
    pub id: Uuid,
    pub device_id: Uuid,
    pub algorithm: String,
    pub public_key: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

/// An authenticated session bound to a user and device.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub device_id: Uuid,
    /// SHA-256 of the token. Never serialised out.
    #[serde(skip_serializing)]
    pub token_hash: Vec<u8>,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

/// A freshly created login challenge: a nonce the device must sign.
pub struct Challenge {
    pub id: Uuid,
    pub nonce: Vec<u8>,
}

/// The result of a successful login.
pub struct LoginResult {
    /// Raw session token (hex) to hand to the client. Shown only once.
    pub token: String,
    pub session: Session,
}

/// Create a new user.
pub async fn create_user(pool: &PgPool, display_name: &str) -> Result<User, IdentityError> {
    let user = sqlx::query_as::<_, User>(
        "insert into users (id, display_name) values ($1, $2) returning *",
    )
    .bind(Uuid::now_v7())
    .bind(display_name)
    .fetch_one(pool)
    .await?;
    Ok(user)
}

/// Fetch a user by id.
pub async fn get_user(pool: &PgPool, id: Uuid) -> Result<Option<User>, IdentityError> {
    let user = sqlx::query_as::<_, User>("select * from users where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(user)
}

/// Return the (single) existing user, or create one. Jarvis is single-user, so
/// device enrollment attaches every device to this account.
pub async fn first_user_or_create(
    pool: &PgPool,
    display_name: &str,
) -> Result<User, IdentityError> {
    if let Some(user) =
        sqlx::query_as::<_, User>("select * from users order by created_at asc limit 1")
            .fetch_optional(pool)
            .await?
    {
        return Ok(user);
    }
    create_user(pool, display_name).await
}

/// Register a device for a user together with its initial public key.
/// The device and its key are created atomically.
pub async fn register_device(
    pool: &PgPool,
    user_id: Uuid,
    name: &str,
    platform: Platform,
    algorithm: &str,
    public_key: &[u8],
) -> Result<(Device, DeviceKey), IdentityError> {
    let mut tx = pool.begin().await?;

    let device = sqlx::query_as::<_, Device>(
        "insert into devices (id, user_id, name, platform) \
         values ($1, $2, $3, $4) returning *",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(name)
    .bind(platform.as_str())
    .fetch_one(&mut *tx)
    .await?;

    let key = sqlx::query_as::<_, DeviceKey>(
        "insert into device_keys (id, device_id, algorithm, public_key) \
         values ($1, $2, $3, $4) returning *",
    )
    .bind(Uuid::now_v7())
    .bind(device.id)
    .bind(algorithm)
    .bind(public_key)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((device, key))
}

/// List a user's active (non-revoked) devices, newest first.
pub async fn list_active_devices(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<Device>, IdentityError> {
    let devices = sqlx::query_as::<_, Device>(
        "select * from devices where user_id = $1 and status = 'active' \
         order by created_at desc",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(devices)
}

/// Revoke a device and all of its keys. Idempotent.
pub async fn revoke_device(pool: &PgPool, device_id: Uuid) -> Result<(), IdentityError> {
    let mut tx = pool.begin().await?;
    sqlx::query("update devices set status = 'revoked', updated_at = now() where id = $1")
        .bind(device_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "update device_keys set revoked_at = now() \
         where device_id = $1 and revoked_at is null",
    )
    .bind(device_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Create a login challenge (random nonce) the given device must sign.
pub async fn create_challenge(pool: &PgPool, device_id: Uuid) -> Result<Challenge, IdentityError> {
    let mut nonce = vec![0u8; 32];
    OsRng.fill_bytes(&mut nonce);
    let id = Uuid::now_v7();
    let expires_at = OffsetDateTime::now_utc() + Duration::minutes(5);

    sqlx::query(
        "insert into auth_challenges (id, device_id, nonce, expires_at) \
         values ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(device_id)
    .bind(&nonce)
    .bind(expires_at)
    .execute(pool)
    .await?;

    Ok(Challenge { id, nonce })
}

/// Verify a signed challenge and, on success, issue a session.
///
/// The challenge is single-use (consumed) and must be unexpired. The signature
/// is verified against the device's most recent active public key.
pub async fn login(
    pool: &PgPool,
    device_id: Uuid,
    challenge_id: Uuid,
    signature: &[u8],
) -> Result<LoginResult, IdentityError> {
    let mut tx = pool.begin().await?;

    let challenge: Option<(Vec<u8>, OffsetDateTime, Option<OffsetDateTime>)> = sqlx::query_as(
        "select nonce, expires_at, consumed_at from auth_challenges \
         where id = $1 and device_id = $2 for update",
    )
    .bind(challenge_id)
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (nonce, expires_at, consumed_at) = challenge.ok_or(IdentityError::AuthFailed)?;
    if consumed_at.is_some() || expires_at < OffsetDateTime::now_utc() {
        return Err(IdentityError::AuthFailed);
    }

    let public_key: Option<Vec<u8>> = sqlx::query_scalar(
        "select public_key from device_keys \
         where device_id = $1 and revoked_at is null \
         order by created_at desc limit 1",
    )
    .bind(device_id)
    .fetch_optional(&mut *tx)
    .await?;
    let public_key = public_key.ok_or(IdentityError::AuthFailed)?;

    verify_signature(&public_key, &nonce, signature)?;

    let user_id: Option<Uuid> =
        sqlx::query_scalar("select user_id from devices where id = $1 and status = 'active'")
            .bind(device_id)
            .fetch_optional(&mut *tx)
            .await?;
    let user_id = user_id.ok_or(IdentityError::AuthFailed)?;

    sqlx::query("update auth_challenges set consumed_at = now() where id = $1")
        .bind(challenge_id)
        .execute(&mut *tx)
        .await?;

    let mut token = vec![0u8; 32];
    OsRng.fill_bytes(&mut token);
    let token_hash = Sha256::digest(&token).to_vec();
    let expires_at = OffsetDateTime::now_utc() + Duration::days(30);

    let session = sqlx::query_as::<_, Session>(
        "insert into sessions (id, user_id, device_id, token_hash, expires_at) \
         values ($1, $2, $3, $4, $5) returning *",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(device_id)
    .bind(&token_hash)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(LoginResult {
        token: hex::encode(token),
        session,
    })
}

/// The authenticated principal behind a valid session token.
pub struct Authenticated {
    pub user: User,
    pub device: Device,
    pub session_id: Uuid,
}

/// Validate a raw session token; returns the authenticated principal.
pub async fn authenticate(pool: &PgPool, token: &str) -> Result<Authenticated, IdentityError> {
    let raw = hex::decode(token).map_err(|_| IdentityError::AuthFailed)?;
    let token_hash = Sha256::digest(&raw).to_vec();

    let session = sqlx::query_as::<_, Session>("select * from sessions where token_hash = $1")
        .bind(&token_hash)
        .fetch_optional(pool)
        .await?
        .ok_or(IdentityError::AuthFailed)?;

    if session.revoked_at.is_some() || session.expires_at < OffsetDateTime::now_utc() {
        return Err(IdentityError::AuthFailed);
    }

    sqlx::query("update sessions set last_used_at = now() where id = $1")
        .bind(session.id)
        .execute(pool)
        .await?;

    let user = get_user(pool, session.user_id)
        .await?
        .ok_or(IdentityError::AuthFailed)?;
    let device = sqlx::query_as::<_, Device>("select * from devices where id = $1")
        .bind(session.device_id)
        .fetch_one(pool)
        .await?;

    Ok(Authenticated {
        user,
        device,
        session_id: session.id,
    })
}

/// Fetch a device by id.
pub async fn get_device(pool: &PgPool, id: Uuid) -> Result<Option<Device>, IdentityError> {
    let device = sqlx::query_as::<_, Device>("select * from devices where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(device)
}

/// Revoke a session. Idempotent.
pub async fn revoke_session(pool: &PgPool, session_id: Uuid) -> Result<(), IdentityError> {
    sqlx::query("update sessions set revoked_at = now() where id = $1 and revoked_at is null")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Verify an Ed25519 signature over `message` using a raw 32-byte public key.
fn verify_signature(
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), IdentityError> {
    let key_bytes: [u8; 32] = public_key
        .try_into()
        .map_err(|_| IdentityError::AuthFailed)?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| IdentityError::AuthFailed)?;
    let sig_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| IdentityError::AuthFailed)?;
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key
        .verify_strict(message, &signature)
        .map_err(|_| IdentityError::AuthFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn new_keypair() -> SigningKey {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        SigningKey::from_bytes(&seed)
    }

    #[test]
    fn platform_round_trips() {
        for p in [
            Platform::Macos,
            Platform::Ios,
            Platform::Windows,
            Platform::Linux,
            Platform::Android,
        ] {
            assert_eq!(Platform::parse(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn platform_rejects_unknown() {
        assert!(Platform::parse("symbian").is_err());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn register_lists_and_revokes(pool: PgPool) -> Result<(), IdentityError> {
        let user = create_user(&pool, "Gus").await?;
        assert_eq!(get_user(&pool, user.id).await?.unwrap().id, user.id);

        let (device, key) = register_device(
            &pool,
            user.id,
            "Gus's iPhone",
            Platform::Ios,
            "ed25519",
            b"pk",
        )
        .await?;
        assert_eq!(device.user_id, user.id);
        assert_eq!(key.device_id, device.id);

        let active = list_active_devices(&pool, user.id).await?;
        assert_eq!(active.len(), 1);

        revoke_device(&pool, device.id).await?;
        assert!(list_active_devices(&pool, user.id).await?.is_empty());
        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn challenge_login_authenticate_flow(pool: PgPool) -> Result<(), IdentityError> {
        let user = create_user(&pool, "Gus").await?;
        let signing = new_keypair();
        let public = signing.verifying_key().to_bytes();

        let (device, _key) =
            register_device(&pool, user.id, "iPhone", Platform::Ios, "ed25519", &public).await?;

        let challenge = create_challenge(&pool, device.id).await?;
        let signature = signing.sign(&challenge.nonce).to_bytes();

        let result = login(&pool, device.id, challenge.id, &signature).await?;
        let auth = authenticate(&pool, &result.token).await?;
        assert_eq!(auth.user.id, user.id);
        assert_eq!(auth.device.id, device.id);

        // A challenge cannot be replayed.
        assert!(login(&pool, device.id, challenge.id, &signature)
            .await
            .is_err());

        // A revoked session no longer authenticates.
        revoke_session(&pool, result.session.id).await?;
        assert!(authenticate(&pool, &result.token).await.is_err());

        Ok(())
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn login_rejects_bad_signature(pool: PgPool) -> Result<(), IdentityError> {
        let user = create_user(&pool, "Gus").await?;
        let signing = new_keypair();
        let (device, _k) = register_device(
            &pool,
            user.id,
            "iPhone",
            Platform::Ios,
            "ed25519",
            &signing.verifying_key().to_bytes(),
        )
        .await?;

        let challenge = create_challenge(&pool, device.id).await?;
        // Sign the WRONG message.
        let bad = signing.sign(b"not the nonce").to_bytes();
        assert!(login(&pool, device.id, challenge.id, &bad).await.is_err());
        Ok(())
    }
}
