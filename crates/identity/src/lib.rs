//! Identity domain: the single Jarvis user and their trusted devices.
//!
//! Device private keys never leave the device (OS keychain); only public keys
//! are stored here, for device-bound sessions and approvals (JAR-101 / JAR-104).

// `sqlx::Error` is a large third-party error we surface via `IdentityError`.
#![allow(clippy::result_large_err)]

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Errors returned by the identity repository.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("unknown platform: {0}")]
    UnknownPlatform(String),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
