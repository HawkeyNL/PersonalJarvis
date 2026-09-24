//! Shared signing bytes for exact, owner-approved model policy mutations.
//! This module neither authenticates the owner nor executes an operation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

/// Public, non-secret approval data. Native clients must verify local device and
/// origin binding, obtain fresh OS authentication, then sign `message()`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelToggleApproval {
    pub request_id: Uuid,
    pub nonce_hex: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub issued_at: i64,
    pub expires_at: i64,
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    pub expected_policy_sha256: String,
}

// Field order is the existing broker v1 canonical payload order.
#[derive(Serialize)]
struct ToggleOperation<'a> {
    action: &'static str,
    provider: &'a str,
    model: &'a str,
    enabled: bool,
    expected_policy_sha256: &'a str,
}

impl ModelToggleApproval {
    fn operation(&self) -> ToggleOperation<'_> {
        ToggleOperation {
            action: "model_set_enabled",
            provider: &self.provider,
            model: &self.model,
            enabled: self.enabled,
            expected_policy_sha256: &self.expected_policy_sha256,
        }
    }

    pub fn message(&self) -> Result<Vec<u8>, &'static str> {
        if !matches!(
            self.provider.as_str(),
            "anthropic-api"
                | "openai-api"
                | "deepseek-api"
                | "xai-api"
                | "zai-api"
                | "ollama"
                | "ollama-cloud"
                | "huggingface"
                | "claude-cli"
        ) || self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err("invalid model operation");
        }
        let state: [u8; 32] = hex::decode(&self.expected_policy_sha256)
            .map_err(|_| "invalid policy hash")?
            .try_into()
            .map_err(|_| "invalid policy hash")?;
        let nonce = hex::decode(&self.nonce_hex).map_err(|_| "invalid nonce")?;
        let payload = serde_json::to_vec(&self.operation()).map_err(|_| "invalid operation")?;
        approval_message(
            "model.set_enabled",
            &Sha256::digest(payload).into(),
            self.request_id,
            &nonce,
            self.user_id,
            self.device_id,
            OffsetDateTime::from_unix_timestamp(self.issued_at).map_err(|_| "invalid time")?,
            OffsetDateTime::from_unix_timestamp(self.expires_at).map_err(|_| "invalid time")?,
            &state,
        )
    }

    /// Produce only the narrow broker request, never arbitrary signed JSON.
    pub fn signed_request(&self, signature_hex: &str) -> Result<serde_json::Value, &'static str> {
        self.message()?;
        if hex::decode(signature_hex)
            .map_err(|_| "invalid signature")?
            .len()
            != 64
        {
            return Err("invalid signature");
        }
        let format = &time::format_description::well_known::Rfc3339;
        Ok(serde_json::json!({
            "request_id": self.request_id, "nonce_hex": self.nonce_hex,
            "user_id": self.user_id, "device_id": self.device_id,
            "issued_at": OffsetDateTime::from_unix_timestamp(self.issued_at).map_err(|_| "invalid time")?.format(format).map_err(|_| "invalid time")?,
            "expires_at": OffsetDateTime::from_unix_timestamp(self.expires_at).map_err(|_| "invalid time")?.format(format).map_err(|_| "invalid time")?,
            "operation": self.operation(), "signature_hex": signature_hex,
        }))
    }
}

/// Stable v1 broker signing format. Hashes bind the operation and policy snapshot;
/// native clients must obtain fresh OS authentication before signing these bytes.
#[allow(clippy::too_many_arguments)]
pub fn approval_message(
    action: &str,
    payload_hash: &[u8; 32],
    request_id: Uuid,
    nonce: &[u8],
    user_id: Uuid,
    device_id: Uuid,
    issued_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    target_state_hash: &[u8; 32],
) -> Result<Vec<u8>, &'static str> {
    if action.is_empty()
        || action.len() > 64
        || action.chars().any(char::is_control)
        || nonce.len() != 32
        || expires_at <= issued_at
        || expires_at - issued_at > time::Duration::minutes(5)
    {
        return Err("invalid privileged approval");
    }
    let mut message = Vec::with_capacity(256);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_action_nonce_and_approval_lifetime() {
        let issued = OffsetDateTime::from_unix_timestamp(1).unwrap();
        let encode = |action: &str, nonce: &[u8], lifetime: i64| {
            approval_message(
                action,
                &[1; 32],
                Uuid::from_bytes([2; 16]),
                nonce,
                Uuid::from_bytes([3; 16]),
                Uuid::from_bytes([4; 16]),
                issued,
                issued + time::Duration::seconds(lifetime),
                &[5; 32],
            )
        };
        for action in ["", "model.set_enabled\n", "model\0set_enabled"] {
            assert!(encode(action, &[0; 32], 120).is_err());
        }
        assert!(encode(&"a".repeat(65), &[0; 32], 120).is_err());
        for length in [0, 31, 33] {
            assert!(encode("model.set_enabled", &vec![0; length], 120).is_err());
        }
        for lifetime in [-1, 0, 301] {
            assert!(encode("model.set_enabled", &[0; 32], lifetime).is_err());
        }
        assert!(encode("model.set_enabled", &[0; 32], 300).is_ok());
    }

    #[test]
    fn fixed_v1_vector_preserves_field_order_and_big_endian_timestamps() {
        let result = approval_message(
            "model.set_enabled",
            &[0x11; 32],
            Uuid::from_bytes([0x22; 16]),
            &[0x33; 32],
            Uuid::from_bytes([0x44; 16]),
            Uuid::from_bytes([0x55; 16]),
            OffsetDateTime::from_unix_timestamp(1).unwrap(),
            OffsetDateTime::from_unix_timestamp(2).unwrap(),
            &[0x66; 32],
        )
        .unwrap();
        let expected = format!(
            "{}0011{}{}{}{}{}{}00000000000000010000000000000002{}",
            hex::encode(b"jarvis-privileged-config-v1\0"),
            hex::encode(b"model.set_enabled"),
            "11".repeat(32),
            "22".repeat(16),
            "33".repeat(32),
            "44".repeat(16),
            "55".repeat(16),
            "66".repeat(32),
        );
        assert_eq!(hex::encode(result), expected);
    }
}
