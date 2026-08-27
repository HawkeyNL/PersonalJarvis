//! Narrow protocol shared by the unprivileged API and the local root config
//! broker.  It intentionally describes only allowlisted configuration actions;
//! it has no shell, path, environment or arbitrary-file operation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

pub const ACTION_MODEL_SET_ENABLED: &str = "model.set_enabled";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Operation {
    /// Change the enabled state of an already-discovered exact model pair.
    /// `expected_policy_sha256` prevents a signature for one policy version
    /// from being applied after the owner-visible policy changed.
    ModelSetEnabled {
        provider: String,
        model: String,
        enabled: bool,
        expected_policy_sha256: String,
    },
}

impl Operation {
    pub fn action(&self) -> &'static str {
        ACTION_MODEL_SET_ENABLED
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::ModelSetEnabled {
                provider,
                model,
                expected_policy_sha256,
                ..
            } => {
                if !matches!(
                    provider.as_str(),
                    "anthropic-api"
                        | "openai-api"
                        | "deepseek-api"
                        | "xai-api"
                        | "zai-api"
                        | "ollama"
                        | "ollama-cloud"
                        | "claude-cli"
                ) || model.is_empty()
                    || model.len() > 256
                    || model.contains(['\n', '\r', '\0'])
                    || expected_policy_sha256.len() != 64
                    || hex::decode(expected_policy_sha256).is_err()
                {
                    return Err(ProtocolError::InvalidOperation);
                }
            }
        }
        Ok(())
    }

    pub fn canonical_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        // This enum has a fixed field order. serde_json is only used after
        // validating scalar-only values, then its bytes are SHA-256-bound in
        // the Ed25519 message; no maps or arbitrary JSON are accepted.
        serde_json::to_vec(self).map_err(|_| ProtocolError::InvalidOperation)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRequest {
    pub request_id: Uuid,
    pub nonce_hex: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub operation: Operation,
    pub signature_hex: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid privileged operation")]
    InvalidOperation,
    #[error("invalid approval")]
    InvalidApproval,
    #[error("approval expired")]
    Expired,
}

impl SignedRequest {
    pub fn message(&self) -> Result<Vec<u8>, ProtocolError> {
        let nonce: [u8; 32] = hex::decode(&self.nonce_hex)
            .map_err(|_| ProtocolError::InvalidApproval)?
            .try_into()
            .map_err(|_| ProtocolError::InvalidApproval)?;
        let signature =
            hex::decode(&self.signature_hex).map_err(|_| ProtocolError::InvalidApproval)?;
        if signature.len() != 64
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > time::Duration::minutes(5)
        {
            return Err(ProtocolError::InvalidApproval);
        }
        let payload: [u8; 32] = Sha256::digest(self.operation.canonical_payload()?).into();
        let state = match &self.operation {
            Operation::ModelSetEnabled {
                expected_policy_sha256,
                ..
            } => hex::decode(expected_policy_sha256)
                .map_err(|_| ProtocolError::InvalidOperation)?
                .try_into()
                .map_err(|_| ProtocolError::InvalidOperation)?,
        };
        jarvis_identity::privileged_config_approval_message(
            self.operation.action(),
            &payload,
            self.request_id,
            &nonce,
            self.user_id,
            self.device_id,
            self.issued_at,
            self.expires_at,
            &state,
        )
        .map_err(|_| ProtocolError::InvalidApproval)
    }

    pub fn reject_if_expired(&self, now: OffsetDateTime) -> Result<(), ProtocolError> {
        if now > self.expires_at || now < self.issued_at - time::Duration::seconds(30) {
            Err(ProtocolError::Expired)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn request() -> SignedRequest {
        let now = OffsetDateTime::now_utc();
        SignedRequest {
            request_id: Uuid::now_v7(),
            nonce_hex: hex::encode([7_u8; 32]),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
            issued_at: now,
            expires_at: now + time::Duration::minutes(2),
            operation: Operation::ModelSetEnabled {
                provider: "openai-api".into(),
                model: "gpt-4o".into(),
                enabled: true,
                expected_policy_sha256: hex::encode([8_u8; 32]),
            },
            signature_hex: hex::encode([0_u8; 64]),
        }
    }
    #[test]
    fn signed_message_rejects_arbitrary_path_and_command_shapes() {
        let mut req = request();
        let Operation::ModelSetEnabled { model, .. } = &mut req.operation;
        *model = "../../etc/shadow\nsh -c id".into();
        assert_eq!(req.message(), Err(ProtocolError::InvalidOperation));
    }
    #[test]
    fn altered_payload_cannot_verify_with_original_signature() {
        let mut req = request();
        let key = SigningKey::from_bytes(&[9; 32]);
        req.signature_hex = hex::encode(key.sign(&req.message().unwrap()).to_bytes());
        let Operation::ModelSetEnabled { enabled, .. } = &mut req.operation;
        *enabled = false;
        let sig = hex::decode(&req.signature_hex).unwrap();
        assert!(jarvis_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &req.message().unwrap(),
            &sig
        )
        .is_err());
    }

    #[test]
    fn forged_or_wrong_device_signature_is_rejected() {
        let mut req = request();
        let owner = SigningKey::from_bytes(&[9; 32]);
        let attacker = SigningKey::from_bytes(&[10; 32]);
        req.signature_hex = hex::encode(attacker.sign(&req.message().unwrap()).to_bytes());
        let signature = hex::decode(&req.signature_hex).unwrap();
        assert!(jarvis_identity::verify_signature(
            owner.verifying_key().as_bytes(),
            &req.message().unwrap(),
            &signature,
        )
        .is_err());
    }
    #[test]
    fn expired_request_fails_closed() {
        let mut req = request();
        req.expires_at = req.issued_at - time::Duration::seconds(1);
        assert!(req.message().is_err());
    }
}
