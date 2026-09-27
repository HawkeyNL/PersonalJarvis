//! Finite local protocol between Core and the subscription-only Claude worker.
//! Account mutation and credential material are deliberately absent.

use serde::{Deserialize, Serialize};

pub const SOCKET: &str = "/run/jarvis-claude.sock";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// Conservative projection of official CLI JSON. Unknown billing metadata
/// never authorizes a subscription run.
pub fn claude_subscription_status(bytes: &[u8]) -> &'static str {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return "unhealthy";
    };
    if value.get("loggedIn").and_then(serde_json::Value::as_bool) != Some(true) {
        return "logged_out";
    }
    if value.get("authMethod").and_then(serde_json::Value::as_str) != Some("claude.ai")
        || value.get("apiProvider").and_then(serde_json::Value::as_str) != Some("firstParty")
    {
        return "wrong_auth_mode";
    }
    match value
        .get("subscriptionType")
        .and_then(serde_json::Value::as_str)
    {
        Some("pro" | "max" | "team" | "enterprise") => "connected",
        _ => "wrong_auth_mode",
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeWorkerRequest {
    pub protocol: u8,
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
}

impl ClaudeWorkerRequest {
    pub fn valid(&self) -> bool {
        self.protocol == 1
            && !self.prompt.trim().is_empty()
            && self.prompt.len() <= 48 * 1024
            && self
                .system
                .as_deref()
                .is_none_or(|value| value.len() <= 12 * 1024)
            && !self.model.is_empty()
            && self.model.len() <= 80
            && self
                .model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeWorkerState {
    Completed,
    SubscriptionUnavailable,
    PlanLimit,
    RuntimeFailure,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeWorkerReply {
    pub protocol: u8,
    pub state: ClaudeWorkerState,
    pub text: Option<String>,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cache_read_tokens: Option<u32>,
    pub cache_write_tokens: Option<u32>,
}

impl ClaudeWorkerReply {
    pub fn failure(state: ClaudeWorkerState) -> Self {
        Self {
            protocol: 1,
            state,
            text: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_protocol_has_no_account_mutation_or_host_execution_fields() {
        let malicious =
            r#"{"protocol":1,"model":"claude","prompt":"hello","system":null,"login":true}"#;
        assert!(serde_json::from_str::<ClaudeWorkerRequest>(malicious).is_err());
        let request = ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-sonnet-5".into(),
            system: None,
            prompt: "hello".into(),
        };
        assert!(request.valid());
        assert!(!ClaudeWorkerRequest {
            model: "x;sh".into(),
            ..request
        }
        .valid());
    }
}
