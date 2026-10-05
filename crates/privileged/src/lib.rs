//! Narrow protocol shared by the unprivileged API and the local root config
//! broker.  It intentionally describes only allowlisted configuration actions;
//! it has no shell, path, environment or arbitrary-file operation.

use jarvis_llm::ModelRouting;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

pub mod local_devices;

pub const ACTION_MODEL_SET_ENABLED: &str = "model.set_enabled";
pub const ACTION_MODEL_ROUTING_SET: &str = "model.routing_set";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
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
    /// Replace the whole owner routing document (`routing.json`). Routing only
    /// orders already-discovered models; it never enables one.
    /// `expected_routing_sha256` binds the approval to the exact current file
    /// bytes, or to empty bytes when the file is absent, so an invalid file
    /// can still be repaired.
    ModelRoutingSet {
        routing: ModelRouting,
        expected_routing_sha256: String,
    },
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && hex::decode(value).is_ok()
}

impl Operation {
    pub fn action(&self) -> &'static str {
        match self {
            Self::ModelSetEnabled { .. } => ACTION_MODEL_SET_ENABLED,
            Self::ModelRoutingSet { .. } => ACTION_MODEL_ROUTING_SET,
        }
    }

    /// The protected file version this operation is bound to.
    pub fn expected_state_sha256(&self) -> &str {
        match self {
            Self::ModelSetEnabled {
                expected_policy_sha256,
                ..
            } => expected_policy_sha256,
            Self::ModelRoutingSet {
                expected_routing_sha256,
                ..
            } => expected_routing_sha256,
        }
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
                        | "huggingface"
                        | "claude-cli"
                        | "codex-cli"
                ) || model.is_empty()
                    || model.len() > 256
                    || model.chars().any(char::is_control)
                    || !valid_sha256_hex(expected_policy_sha256)
                {
                    return Err(ProtocolError::InvalidOperation);
                }
            }
            Self::ModelRoutingSet {
                routing,
                expected_routing_sha256,
            } => {
                if routing.validate().is_err() || !valid_sha256_hex(expected_routing_sha256) {
                    return Err(ProtocolError::InvalidOperation);
                }
            }
        }
        Ok(())
    }

    /// Compact serde_json bytes of the validated operation, SHA-256-bound in
    /// the Ed25519 message. Every struct has a fixed field order and only
    /// typed, validated values; no arbitrary maps or JSON are accepted. For
    /// `model_routing_set` an absent tier is omitted, while `paid_api` and
    /// `metered_after_subscription` are always present (see the fixed vector).
    pub fn canonical_payload(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| ProtocolError::InvalidOperation)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        let state: [u8; 32] = hex::decode(self.operation.expected_state_sha256())
            .map_err(|_| ProtocolError::InvalidOperation)?
            .try_into()
            .map_err(|_| ProtocolError::InvalidOperation)?;
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
        if now >= self.expires_at || now < self.issued_at - time::Duration::seconds(30) {
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
    fn native_model_approval_matches_broker_bytes_and_request_shape() {
        let approval = jarvis_client_core::model_control::ModelToggleApproval {
            request_id: Uuid::from_bytes([1; 16]),
            nonce_hex: "02".repeat(32),
            user_id: Uuid::from_bytes([3; 16]),
            device_id: Uuid::from_bytes([4; 16]),
            issued_at: 1,
            expires_at: 121,
            provider: "huggingface".into(),
            model: "org/fixture".into(),
            enabled: true,
            expected_policy_sha256: "05".repeat(32),
        };
        let key = SigningKey::from_bytes(&[6; 32]);
        let signature = hex::encode(key.sign(&approval.message().unwrap()).to_bytes());
        let wire = approval.signed_request(&signature).unwrap();
        let request: SignedRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(request.message().unwrap(), approval.message().unwrap());
        assert!(jarvis_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &request.message().unwrap(),
            &hex::decode(signature).unwrap()
        )
        .is_ok());
        let mut changed = approval.clone();
        changed.enabled = false;
        assert_ne!(changed.message().unwrap(), approval.message().unwrap());
        changed = approval.clone();
        changed.expected_policy_sha256 = "07".repeat(32);
        assert_ne!(changed.message().unwrap(), approval.message().unwrap());
        changed = approval;
        changed.provider = "shell".into();
        assert!(changed.message().is_err());
    }

    fn routing_approval() -> jarvis_client_core::model_control::ModelRoutingApproval {
        let routing = serde_json::from_value(serde_json::json!({
            "version": 1, "paid_api": "off", "tiers": {
                "cheap": {"chain": [{"provider": "huggingface", "model": "org/modèl"},
                                    {"provider": "claude-cli", "model": "claude-haiku-4-5"}]},
                "hard": {"chain": [{"provider": "claude-cli", "model": "claude-opus-5"},
                                   {"provider": "anthropic-api", "model": "claude-opus-5"}],
                         "metered_after_subscription": true}}}))
        .unwrap();
        jarvis_client_core::model_control::ModelRoutingApproval {
            request_id: Uuid::from_bytes([1; 16]),
            nonce_hex: "02".repeat(32),
            user_id: Uuid::from_bytes([3; 16]),
            device_id: Uuid::from_bytes([4; 16]),
            issued_at: 1,
            expires_at: 121,
            routing,
            expected_routing_sha256: hex::encode(Sha256::digest(b"")),
        }
    }

    fn routing_operation(request: &mut SignedRequest) -> (&mut ModelRouting, &mut String) {
        let Operation::ModelRoutingSet {
            routing,
            expected_routing_sha256,
        } = &mut request.operation
        else {
            unreachable!()
        };
        (routing, expected_routing_sha256)
    }

    #[test]
    fn native_routing_approval_matches_the_fixed_vector_and_broker_bytes() {
        let approval = routing_approval();
        let key = SigningKey::from_bytes(&[6; 32]);
        let signature = hex::encode(key.sign(&approval.message().unwrap()).to_bytes());
        let request: SignedRequest =
            serde_json::from_value(approval.signed_request(&signature).unwrap()).unwrap();
        assert_eq!(request.operation.action(), "model.routing_set");
        assert_eq!(
            hex::encode(Sha256::digest(
                request.operation.canonical_payload().unwrap()
            )),
            "a9c73476991bb884fbaf43378882c25f4403a8ce3daa44d1012221ce15e30a59"
        );
        assert_eq!(request.message().unwrap(), approval.message().unwrap());
        assert!(jarvis_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &request.message().unwrap(),
            &hex::decode(&signature).unwrap()
        )
        .is_ok());
        // Research on: the native client and the broker still agree byte for byte.
        let mut research = approval.clone();
        research.routing.research_web_search =
            jarvis_client_core::model_control::ResearchWebSearch::On;
        let research_request: SignedRequest =
            serde_json::from_value(research.signed_request(&signature).unwrap()).unwrap();
        assert_eq!(
            research_request.message().unwrap(),
            research.message().unwrap()
        );
        assert_ne!(research.message().unwrap(), approval.message().unwrap());
        // A different document cannot reuse the signature.
        let mut changed = request.clone();
        routing_operation(&mut changed).0.paid_api = jarvis_llm::PaidApi::Allowed;
        assert!(jarvis_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &changed.message().unwrap(),
            &hex::decode(&signature).unwrap()
        )
        .is_err());
    }

    #[test]
    fn signature_for_one_action_cannot_approve_the_other() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let mut toggle = request();
        toggle.signature_hex = hex::encode(key.sign(&toggle.message().unwrap()).to_bytes());
        let routing_request: SignedRequest = serde_json::from_value(
            routing_approval()
                .signed_request(&toggle.signature_hex)
                .unwrap(),
        )
        .unwrap();
        // Same envelope and state hash, only the operation swapped.
        let mut replay = toggle.clone();
        replay.operation = Operation::ModelRoutingSet {
            routing: serde_json::from_value(
                serde_json::to_value(routing_approval().routing).unwrap(),
            )
            .unwrap(),
            expected_routing_sha256: toggle.operation.expected_state_sha256().into(),
        };
        let signature = hex::decode(&toggle.signature_hex).unwrap();
        for forged in [&replay, &routing_request] {
            assert!(jarvis_identity::verify_signature(
                key.verifying_key().as_bytes(),
                &forged.message().unwrap(),
                &signature
            )
            .is_err());
        }
    }

    #[test]
    fn routing_operation_rejects_invalid_documents_and_hashes() {
        let base: SignedRequest =
            serde_json::from_value(routing_approval().signed_request(&"00".repeat(64)).unwrap())
                .unwrap();
        assert!(base.message().is_ok());
        let mut oversized = base.clone();
        let (routing, _) = routing_operation(&mut oversized);
        routing.tiers.default = Some(jarvis_llm::TierRoute {
            chain: (0..10)
                .map(|index| jarvis_llm::RouteEntry {
                    provider: "ollama".into(),
                    model: format!("m{index}"),
                })
                .collect(),
            metered_after_subscription: false,
        });
        let mut metered = base.clone();
        routing_operation(&mut metered)
            .0
            .tiers
            .hard
            .as_mut()
            .unwrap()
            .metered_after_subscription = false;
        let mut not_hex = base.clone();
        *routing_operation(&mut not_hex).1 = "zz".repeat(32);
        let mut short = base.clone();
        *routing_operation(&mut short).1 = "00".repeat(31);
        let mut version = base;
        routing_operation(&mut version).0.version = 2;
        for request in [oversized, metered, not_hex, short, version] {
            assert_eq!(request.message(), Err(ProtocolError::InvalidOperation));
        }
    }

    #[test]
    fn signed_message_rejects_arbitrary_path_and_command_shapes() {
        let mut req = request();
        let Operation::ModelSetEnabled { model, .. } = &mut req.operation else {
            unreachable!()
        };
        *model = "../../etc/shadow\nsh -c id".into();
        assert_eq!(req.message(), Err(ProtocolError::InvalidOperation));
    }
    #[test]
    fn altered_payload_cannot_verify_with_original_signature() {
        let mut req = request();
        let key = SigningKey::from_bytes(&[9; 32]);
        req.signature_hex = hex::encode(key.sign(&req.message().unwrap()).to_bytes());
        let Operation::ModelSetEnabled { enabled, .. } = &mut req.operation else {
            unreachable!()
        };
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

    #[test]
    fn discovered_huggingface_identity_is_supported_without_changing_route() {
        let mut req = request();
        let Operation::ModelSetEnabled {
            provider, model, ..
        } = &mut req.operation
        else {
            unreachable!()
        };
        *provider = "huggingface".into();
        *model = "fixture-org/fixture-model".into();
        assert!(req.message().is_ok());
        // Discovery and exact policy membership are still enforced by the broker.
    }

    #[test]
    fn approval_expires_at_deadline_not_after_it() {
        let req = request();
        assert_eq!(
            req.reject_if_expired(req.expires_at),
            Err(ProtocolError::Expired)
        );
        assert!(req
            .reject_if_expired(req.expires_at - time::Duration::nanoseconds(1))
            .is_ok());
    }

    #[test]
    fn unknown_fields_and_control_characters_are_rejected() {
        let req = request();
        let mut value = serde_json::to_value(&req).unwrap();
        value["approved"] = true.into();
        assert!(serde_json::from_value::<SignedRequest>(value).is_err());
        let mut value = serde_json::to_value(&req).unwrap();
        value["operation"]["shell"] = "ignored-command".into();
        assert!(serde_json::from_value::<SignedRequest>(value).is_err());
        for control in ['\t', '\u{001b}', '\u{007f}'] {
            let mut req = request();
            let Operation::ModelSetEnabled { model, .. } = &mut req.operation else {
                unreachable!()
            };
            *model = format!("fixture{control}model");
            assert_eq!(req.message(), Err(ProtocolError::InvalidOperation));
        }
    }
}
