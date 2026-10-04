//! Platform-neutral Jarvis v1 client protocol.
//!
//! This crate contains wire shapes and deterministic protocol helpers only.
//! Networking, secure storage, biometrics, and UI policy remain owned by each
//! platform application.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

pub mod account;
pub mod model_control;
pub mod realtime;
pub mod speech;

pub const PROTOCOL_VERSION: u16 = 1;
pub const PAIRING_APPROVAL_DOMAIN: &[u8] = b"jarvis-device-pairing-v1\0";
pub const AGENT_APPROVAL_DOMAIN: &[u8] = b"jarvis/agent-approval/v1\0";
/// `approval_message` value in `GET /v1/agent/pending` for [`agent_approval_message`].
pub const AGENT_APPROVAL_MESSAGE_V1: &str = "agent-approval-v1";

pub const MAX_DEVICE_NAME_LEN: usize = 128;
pub const MAX_PLATFORM_LEN: usize = 32;
pub const ED25519_PUBLIC_KEY_HEX_LEN: usize = 64;
pub const ED25519_SIGNATURE_HEX_LEN: usize = 128;
pub const NONCE_HEX_LEN: usize = 64;
pub const MAX_CHAT_TURNS: usize = 500;
pub const MAX_CHAT_CONTENT_LEN: usize = 24_000;
pub const MAX_CHAT_PAYLOAD_LEN: usize = 128_000;

pub fn bounded_text(value: &str, max_len: usize) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.len() <= max_len
}

pub fn is_hex_of_len(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid pairing approval")]
    InvalidPairingApproval,
    #[error("invalid agent approval")]
    InvalidAgentApproval,
}

/// Canonical, domain-separated v1 bytes signed by a pairing approver.
pub fn pairing_approval_message(
    request_id: Uuid,
    nonce: &[u8],
    candidate_public_key: &[u8],
    user_id: Uuid,
    approver_device_id: Uuid,
    expires_at: OffsetDateTime,
) -> Result<Vec<u8>, ProtocolError> {
    if nonce.len() != 32 || candidate_public_key.len() != 32 {
        return Err(ProtocolError::InvalidPairingApproval);
    }
    let mut message = Vec::with_capacity(PAIRING_APPROVAL_DOMAIN.len() + 120);
    message.extend_from_slice(PAIRING_APPROVAL_DOMAIN);
    message.extend_from_slice(request_id.as_bytes());
    message.extend_from_slice(nonce);
    message.extend_from_slice(candidate_public_key);
    message.extend_from_slice(user_id.as_bytes());
    message.extend_from_slice(approver_device_id.as_bytes());
    message.extend_from_slice(&expires_at.unix_timestamp().to_be_bytes());
    Ok(message)
}

/// Canonical, domain-separated v1 bytes a trusted device signs to approve one
/// pending agent action. `action_sha256` is the SHA-256 of the pending action's
/// stored canonical JSON (compact serde_json, UTF-8), exactly as hex-encoded in
/// `action_sha256` of `GET /v1/agent/pending`. Every field has a fixed width.
pub fn agent_approval_message(
    pending_id: Uuid,
    nonce: &[u8],
    action_sha256: &[u8],
    device_id: Uuid,
) -> Result<Vec<u8>, ProtocolError> {
    if nonce.len() != 32 || action_sha256.len() != 32 {
        return Err(ProtocolError::InvalidAgentApproval);
    }
    let mut message = Vec::with_capacity(AGENT_APPROVAL_DOMAIN.len() + 96);
    message.extend_from_slice(AGENT_APPROVAL_DOMAIN);
    message.extend_from_slice(pending_id.as_bytes());
    message.extend_from_slice(nonce);
    message.extend_from_slice(action_sha256);
    message.extend_from_slice(device_id.as_bytes());
    Ok(message)
}

// Auth and enrollment wire DTOs.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub name: String,
    pub platform: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub device_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge_id: Uuid,
    pub nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub device_id: Uuid,
    pub challenge_id: Uuid,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMeResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
}

// Pairing wire DTOs.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCreateResponse {
    pub request_id: Uuid,
    pub nonce: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequestItem {
    pub id: Uuid,
    pub device_name: String,
    pub platform: String,
    pub fingerprint: String,
    pub nonce: String,
    pub candidate_public_key: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequestsResponse {
    pub requests: Vec<PairingRequestItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairingStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingStatusResponse {
    pub status: PairingStatus,
    pub device_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingApproveRequest {
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub status: String,
}

// Device wire DTOs.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceItem {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceItem>,
}

// Chat and conversation wire DTOs.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub reply: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_reason: Option<String>,
    pub stop_reason: Option<String>,
    pub conversation_id: Uuid,
    pub conversation_title: String,
    pub new_topic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationsResponse {
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationResponse {
    pub id: Uuid,
    pub title: String,
    pub messages: Vec<ConversationMessage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_approval_v1_matches_golden_bytes() {
        let request_id = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let user_id = Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap();
        let approver = Uuid::parse_str("ffeeddcc-bbaa-9988-7766-554433221100").unwrap();
        let nonce: Vec<u8> = (0_u8..32).collect();
        let public_key: Vec<u8> = (32_u8..64).collect();
        let expires_at = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();

        let encoded = hex::encode(
            pairing_approval_message(
                request_id,
                &nonce,
                &public_key,
                user_id,
                approver,
                expires_at,
            )
            .unwrap(),
        );
        assert_eq!(
            encoded,
            concat!(
                "6a61727669732d6465766963652d70616972696e672d763100",
                "00112233445566778899aabbccddeeff",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
                "102132435465768798a9bacbdcedfe0f",
                "ffeeddccbbaa99887766554433221100",
                "000000006b49d200"
            )
        );
    }

    /// Fixed agent approval vector for the Swift and Kotlin clients.
    #[test]
    fn agent_approval_v1_matches_golden_bytes() {
        use sha2::{Digest, Sha256};
        let action = br#"{"type":"write_file","path":"note.txt","content":"ok"}"#;
        let action_sha256 = Sha256::digest(action);
        assert_eq!(
            hex::encode(action_sha256),
            "8d2dd02e0f1e485d0be7f748f2fdd0d2160dd245b000ca1bff1ca15cd043dbe5"
        );
        let pending_id = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        let device_id = Uuid::parse_str("ffeeddcc-bbaa-9988-7766-554433221100").unwrap();
        let nonce: Vec<u8> = (0_u8..32).collect();
        let message =
            agent_approval_message(pending_id, &nonce, &action_sha256, device_id).unwrap();
        assert_eq!(
            hex::encode(&message),
            concat!(
                "6a61727669732f6167656e742d617070726f76616c2f763100",
                "00112233445566778899aabbccddeeff",
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8d2dd02e0f1e485d0be7f748f2fdd0d2160dd245b000ca1bff1ca15cd043dbe5",
                "ffeeddccbbaa99887766554433221100"
            )
        );
        assert_ne!(message.len(), 32, "never a raw login/unlock nonce");
        for (nonce, hash) in [
            (&nonce[..31], &action_sha256[..]),
            (&nonce[..], &[0_u8; 31][..]),
        ] {
            assert_eq!(
                agent_approval_message(pending_id, nonce, hash, device_id),
                Err(ProtocolError::InvalidAgentApproval)
            );
        }
    }

    #[test]
    fn v1_login_json_matches_golden_shape() {
        let request = LoginRequest {
            device_id: Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap(),
            challenge_id: Uuid::parse_str("10213243-5465-7687-98a9-bacbdcedfe0f").unwrap(),
            signature: "ab".repeat(64),
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            format!(
                "{{\"device_id\":\"00112233-4455-6677-8899-aabbccddeeff\",\"challenge_id\":\"10213243-5465-7687-98a9-bacbdcedfe0f\",\"signature\":\"{}\"}}",
                "ab".repeat(64)
            )
        );
    }

    #[test]
    fn validation_preserves_v1_byte_and_hex_rules() {
        assert!(bounded_text("iPhone", MAX_DEVICE_NAME_LEN));
        assert!(!bounded_text("   ", MAX_DEVICE_NAME_LEN));
        assert!(!bounded_text(
            &"x".repeat(MAX_DEVICE_NAME_LEN + 1),
            MAX_DEVICE_NAME_LEN
        ));
        assert!(is_hex_of_len(&"aB".repeat(32), ED25519_PUBLIC_KEY_HEX_LEN));
        assert!(!is_hex_of_len(&"z".repeat(64), ED25519_PUBLIC_KEY_HEX_LEN));
    }

    #[test]
    fn request_dtos_keep_v1_unknown_field_policy() {
        let enrollment = r#"{"name":"Phone","platform":"ios","public_key":"00","extra":true}"#;
        assert!(serde_json::from_str::<EnrollmentRequest>(enrollment).is_err());

        let chat = r#"{"messages":[],"future_field":true}"#;
        assert!(serde_json::from_str::<ChatRequest>(chat).is_ok());
    }
}
