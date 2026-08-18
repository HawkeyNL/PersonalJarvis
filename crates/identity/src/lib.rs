//! Identity domain: the single Jarvis user, their trusted devices, and
//! device-bound authentication (challenge-response login + sessions).
//!
//! Device private keys never leave the device (OS keychain); only public keys
//! are stored here. Login proves possession of the private key by signing a
//! server-issued nonce. Session tokens are stored only as SHA-256 hashes.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The SurrealDB repository is the only persistence implementation.
pub mod surreal;

/// Errors returned by the identity repository.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("database error")]
    DatabaseSurreal,
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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Ios => "ios",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Android => "android",
        }
    }

    pub fn parse(s: &str) -> Result<Self, IdentityError> {
        match s {
            "macos" => Ok(Self::Macos),
            "ios" => Ok(Self::Ios),
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            "android" => Ok(Self::Android),
            other => Err(IdentityError::UnknownPlatform(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(with = "uuid::serde::hyphenated")]
    pub id: Uuid,
    pub display_name: String,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    #[serde(with = "uuid::serde::hyphenated")]
    pub id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    pub user_id: Uuid,
    pub name: String,
    pub platform: String,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceKey {
    #[serde(with = "uuid::serde::hyphenated")]
    pub id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    pub device_id: Uuid,
    pub algorithm: String,
    #[serde(with = "serde_bytes")]
    pub public_key: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(with = "uuid::serde::hyphenated")]
    pub id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    pub user_id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    pub device_id: Uuid,
    #[serde(skip_serializing)]
    #[serde(with = "serde_bytes")]
    pub token_hash: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

pub struct Challenge {
    pub id: Uuid,
    pub nonce: Vec<u8>,
}

pub struct LoginResult {
    pub token: String,
    pub session: Session,
}

pub struct Authenticated {
    pub user: User,
    pub device: Device,
    pub session_id: Uuid,
}

#[derive(Debug)]
pub struct UnlockRequest {
    pub id: Uuid,
    pub requesting_device_id: Uuid,
    pub requesting_device_name: String,
    pub requesting_device_platform: String,
    pub nonce: Vec<u8>,
    pub created_at: OffsetDateTime,
}

/// Verify an Ed25519 signature over `message` using a raw 32-byte public key.
pub(crate) fn verify_signature(
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
    verifying_key
        .verify_strict(message, &Signature::from_bytes(&sig_bytes))
        .map_err(|_| IdentityError::AuthFailed)
}

pub use surreal::{
    approve_unlock_request, authenticate, create_challenge, create_unlock_request, create_user,
    deny_unlock_request, first_user_or_create, get_device, get_user, list_active_devices, login,
    pending_unlock_requests, register_device, revoke_device, revoke_session, unlock_request_status,
    verify_device_signature,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_round_trips() {
        for platform in [
            Platform::Macos,
            Platform::Ios,
            Platform::Windows,
            Platform::Linux,
            Platform::Android,
        ] {
            assert_eq!(Platform::parse(platform.as_str()).unwrap(), platform);
        }
    }

    #[test]
    fn platform_rejects_unknown() {
        assert!(Platform::parse("symbian").is_err());
    }
}
