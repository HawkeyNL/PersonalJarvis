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

/// An untrusted candidate device waiting for an approval from an active owner
/// device. This record itself confers no login capability.
#[derive(Debug, Clone)]
pub struct PairingRequest {
    pub id: Uuid,
    pub user_id: Uuid,
    pub candidate_name: String,
    pub candidate_platform: String,
    pub candidate_public_key: Vec<u8>,
    pub candidate_fingerprint: String,
    pub nonce: Vec<u8>,
    pub status: String,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// Canonical, domain-separated bytes that an approving device signs. This is
/// intentionally not JSON and cannot be confused with a login, unlock or agent
/// approval signature.
pub fn pairing_approval_message(
    request_id: Uuid,
    nonce: &[u8],
    candidate_public_key: &[u8],
    user_id: Uuid,
    approver_device_id: Uuid,
    expires_at: OffsetDateTime,
) -> Result<Vec<u8>, IdentityError> {
    if nonce.len() != 32 || candidate_public_key.len() != 32 {
        return Err(IdentityError::AuthFailed);
    }
    let mut message = Vec::with_capacity(26 + 16 + 32 + 32 + 16 + 16 + 8);
    message.extend_from_slice(b"jarvis-device-pairing-v1\0");
    message.extend_from_slice(request_id.as_bytes());
    message.extend_from_slice(nonce);
    message.extend_from_slice(candidate_public_key);
    message.extend_from_slice(user_id.as_bytes());
    message.extend_from_slice(approver_device_id.as_bytes());
    message.extend_from_slice(&expires_at.unix_timestamp().to_be_bytes());
    Ok(message)
}

/// Canonical, domain-separated bytes for a privileged Home Node configuration
/// mutation.  Unlike an ordinary session, this proves that a trusted device
/// explicitly approved this exact request.  It deliberately is not JSON: JSON
/// object ordering or omitted fields must never change what is signed.
#[allow(clippy::too_many_arguments)] // protocol fields intentionally remain explicit and ordered
pub fn privileged_config_approval_message(
    action: &str,
    payload_hash: &[u8; 32],
    request_id: Uuid,
    nonce: &[u8],
    user_id: Uuid,
    device_id: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    target_state_hash: &[u8; 32],
) -> Result<Vec<u8>, IdentityError> {
    if action.is_empty() || action.len() > 64 || nonce.len() != 32 || expires_at < issued_at {
        return Err(IdentityError::AuthFailed);
    }
    let mut message = Vec::with_capacity(64 + action.len());
    message.extend_from_slice(b"jarvis-privileged-config-v1\0");
    message.extend_from_slice(&(action.len() as u16).to_be_bytes());
    message.extend_from_slice(action.as_bytes());
    message.extend_from_slice(payload_hash);
    message.extend_from_slice(request_id.as_bytes());
    message.extend_from_slice(nonce);
    message.extend_from_slice(user_id.as_bytes());
    message.extend_from_slice(device_id.as_bytes());
    message.extend_from_slice(&issued_at.unix_timestamp().to_be_bytes());
    message.extend_from_slice(&expires_at.unix_timestamp().to_be_bytes());
    message.extend_from_slice(target_state_hash);
    Ok(message)
}

/// Canonical, domain-separated bytes for a privileged Codex/OpenSandbox coding
/// operation. A signature for configuration, pairing or agent approval can
/// therefore never be replayed as approval to start coding work.
#[allow(clippy::too_many_arguments)]
pub fn codex_coding_approval_message(
    action: &str,
    payload_hash: &[u8; 32],
    request_id: Uuid,
    nonce: &[u8],
    user_id: Uuid,
    device_id: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    target_state_hash: &[u8; 32],
) -> Result<Vec<u8>, IdentityError> {
    if action.is_empty() || action.len() > 64 || nonce.len() != 32 || expires_at < issued_at {
        return Err(IdentityError::AuthFailed);
    }
    let mut message = Vec::with_capacity(64 + action.len());
    message.extend_from_slice(b"jarvis-codex-coding-v1\0");
    message.extend_from_slice(&(action.len() as u16).to_be_bytes());
    message.extend_from_slice(action.as_bytes());
    message.extend_from_slice(payload_hash);
    message.extend_from_slice(request_id.as_bytes());
    message.extend_from_slice(nonce);
    message.extend_from_slice(user_id.as_bytes());
    message.extend_from_slice(device_id.as_bytes());
    message.extend_from_slice(&issued_at.unix_timestamp().to_be_bytes());
    message.extend_from_slice(&expires_at.unix_timestamp().to_be_bytes());
    message.extend_from_slice(target_state_hash);
    Ok(message)
}

/// Verify an Ed25519 signature over `message` using a raw 32-byte public key.
pub fn verify_signature(
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
    active_device_count, approve_pairing_request, approve_unlock_request, authenticate,
    bootstrap_register_first_device, create_challenge, create_pairing_request,
    create_unlock_request, create_user, deny_pairing_request, deny_unlock_request, first_user,
    first_user_or_create, get_device, get_user, list_active_devices, login, pairing_request_status,
    pairing_status_for_candidate, pending_pairing_requests, pending_unlock_requests,
    register_device, revoke_device, revoke_session, unlock_request_status, verify_device_signature,
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

    #[test]
    fn pairing_message_is_domain_separated_and_argument_bound() {
        let request = Uuid::now_v7();
        let user = Uuid::now_v7();
        let approver = Uuid::now_v7();
        let nonce = [7_u8; 32];
        let key = [9_u8; 32];
        let expires = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let message =
            pairing_approval_message(request, &nonce, &key, user, approver, expires).unwrap();
        assert!(message.starts_with(b"jarvis-device-pairing-v1\0"));
        assert_ne!(message, nonce);
        assert_ne!(
            message,
            pairing_approval_message(request, &nonce, &[8; 32], user, approver, expires).unwrap()
        );
        assert!(
            pairing_approval_message(request, &[0; 31], &key, user, approver, expires).is_err()
        );
    }

    #[test]
    fn privileged_message_binds_every_security_relevant_field() {
        let payload = [1_u8; 32];
        let state = [2_u8; 32];
        let request = Uuid::now_v7();
        let user = Uuid::now_v7();
        let device = Uuid::now_v7();
        let issued = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let expires = issued + time::Duration::minutes(2);
        let message = privileged_config_approval_message(
            "model.set_enabled",
            &payload,
            request,
            &[3; 32],
            user,
            device,
            issued,
            expires,
            &state,
        )
        .unwrap();
        assert!(message.starts_with(b"jarvis-privileged-config-v1\0"));
        assert_ne!(
            message,
            privileged_config_approval_message(
                "model.set_enabled",
                &[4; 32],
                request,
                &[3; 32],
                user,
                device,
                issued,
                expires,
                &state,
            )
            .unwrap()
        );
        assert!(privileged_config_approval_message(
            "model.set_enabled",
            &payload,
            request,
            &[3; 31],
            user,
            device,
            issued,
            expires,
            &state,
        )
        .is_err());
    }

    #[test]
    fn coding_message_has_a_distinct_domain() {
        let issued = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let coding = codex_coding_approval_message(
            "coding.start",
            &[1; 32],
            Uuid::nil(),
            &[2; 32],
            Uuid::nil(),
            Uuid::nil(),
            issued,
            issued + time::Duration::minutes(1),
            &[3; 32],
        )
        .unwrap();
        assert!(coding.starts_with(b"jarvis-codex-coding-v1\0"));
        assert_ne!(
            coding,
            privileged_config_approval_message(
                "coding.start",
                &[1; 32],
                Uuid::nil(),
                &[2; 32],
                Uuid::nil(),
                Uuid::nil(),
                issued,
                issued + time::Duration::minutes(1),
                &[3; 32],
            )
            .unwrap()
        );
    }
}
