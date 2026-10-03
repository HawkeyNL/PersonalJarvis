//! Finite local protocol between Core and the subscription-only Claude worker.
//! Account mutation and credential material are deliberately absent.

use serde::{Deserialize, Serialize};

pub const SOCKET: &str = "/run/jarvis-claude.sock";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;

/// The reviewed CLI contract is Claude Code 2.1.248+ within its 2.1 line.
/// `--restricted` first appeared in 2.1.248. A later minor/major line needs
/// explicit review before the subscription worker can run it.
pub fn reviewed_claude_version(output: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let versions = text
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter_map(|candidate| semver::Version::parse(candidate).ok())
        .collect::<Vec<_>>();
    matches!(versions.as_slice(), [version] if version.major == 2 && version.minor == 1 && version.patch >= 248)
}

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
            && valid_worker_model(&self.model)
    }
}

/// The model ids a subscription worker accepts. The owner's `jarvis models
/// register` applies the same rule, so a registered pair is always usable.
pub fn valid_worker_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 80
        // Never let a model id read as a CLI option.
        && !model.starts_with('-')
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeWorkerState {
    Completed,
    SubscriptionUnavailable,
    PlanLimit,
    IncompatibleRuntime,
    /// The subscription account cannot use the requested model (for example
    /// a staged model rollout). Never a reason to try a paid API instead.
    ModelUnavailable,
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
        assert!(!ClaudeWorkerRequest {
            protocol: 1,
            model: "-c".into(),
            system: None,
            prompt: "hello".into(),
        }
        .valid());
    }

    #[test]
    fn runtime_version_review_gate_is_fail_closed() {
        assert!(reviewed_claude_version(b"2.1.248 (Claude Code)"));
        assert!(reviewed_claude_version(b"Claude Code v2.1.300\n"));
        assert!(!reviewed_claude_version(b"2.1.247 (Claude Code)"));
        assert!(!reviewed_claude_version(b"2.2.0 (Claude Code)"));
        assert!(!reviewed_claude_version(b"2.1.248 9.9.9"));
        assert!(!reviewed_claude_version(b"unrecognized"));
    }
}
