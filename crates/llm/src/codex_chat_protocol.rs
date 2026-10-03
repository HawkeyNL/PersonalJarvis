//! Contract of the text-only Codex chat worker. It speaks the same finite
//! request/reply protocol as the Claude worker (`claude_worker_protocol`);
//! only the socket, the identity and the reviewed official runtime differ.

use crate::claude_worker_protocol::ClaudeWorkerState;

pub const SOCKET: &str = "/run/jarvis-codex-chat.sock";

/// The exact `codex --version` the owner reviewed against the documented
/// `codex exec` flags this worker uses.
///
/// PLACEHOLDER, deliberately empty: no Codex CLI version has been verified
/// for this worker yet, so every run returns `incompatible_runtime` without a
/// model call. The owner sets the exact version (for example `0.0.0`, digits
/// only) in a reviewed change after checking that installed version's
/// `codex exec --help` and the official non-interactive documentation. A
/// newer CLI needs a new review; there is no "or newer".
pub const REVIEWED_CODEX_VERSION: &str = "";

/// Fail-closed version gate: exactly one version-looking token in the bounded
/// output, equal to the reviewed version. An empty review never matches.
pub fn reviewed_codex_version(output: &[u8], reviewed: &str) -> bool {
    let Ok(reviewed) = semver::Version::parse(reviewed) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let versions = text
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter_map(|candidate| semver::Version::parse(candidate).ok())
        .collect::<Vec<_>>();
    matches!(versions.as_slice(), [version] if *version == reviewed)
}

/// Conservative projection of `codex login status`. Only a ChatGPT login is a
/// subscription; an API-key login would bill the paid API and never counts.
pub fn codex_subscription_status(output: &[u8]) -> &'static str {
    let Ok(text) = std::str::from_utf8(output) else {
        return "unhealthy";
    };
    let line = text.trim().to_ascii_lowercase();
    if line == "logged in using chatgpt" {
        "connected"
    } else if line.contains("api key") || line.contains("api-key") {
        "wrong_auth_mode"
    } else if line.contains("not logged in") {
        "logged_out"
    } else {
        "unhealthy"
    }
}

/// Classify a failed `codex exec` run from its bounded diagnostic output.
/// The text is only inspected here and then discarded; it never reaches Core.
pub fn classify_codex_failure(diagnostics: &[u8]) -> ClaudeWorkerState {
    let text = String::from_utf8_lossy(diagnostics).to_ascii_lowercase();
    // openai/codex#47784: "The 'gpt-6-luna' model is not supported when
    // using Codex with a ChatGPT account." during a staged rollout.
    if text.contains("model_not_found")
        || (text.contains("model")
            && (text.contains("not supported") || text.contains("does not exist")))
    {
        ClaudeWorkerState::ModelUnavailable
    } else if text.contains("usage limit") || text.contains("rate limit") || text.contains("429") {
        ClaudeWorkerState::PlanLimit
    } else if text.contains("not logged in")
        || text.contains("unauthorized")
        || text.contains("401")
        || text.contains("login")
    {
        ClaudeWorkerState::SubscriptionUnavailable
    } else {
        ClaudeWorkerState::RuntimeFailure
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gate_requires_an_exact_owner_review() {
        assert!(!reviewed_codex_version(
            b"codex-cli 1.2.3\n",
            REVIEWED_CODEX_VERSION
        ));
        assert!(!reviewed_codex_version(b"codex-cli 1.2.3\n", ""));
        assert!(reviewed_codex_version(b"codex-cli 1.2.3\n", "1.2.3"));
        assert!(!reviewed_codex_version(b"codex-cli 1.2.4\n", "1.2.3"));
        assert!(!reviewed_codex_version(b"codex-cli 1.2.3 9.9.9", "1.2.3"));
        assert!(!reviewed_codex_version(b"unrecognized", "1.2.3"));
    }

    #[test]
    fn only_a_chatgpt_login_is_a_subscription() {
        assert_eq!(
            codex_subscription_status(b"Logged in using ChatGPT\n"),
            "connected"
        );
        assert_eq!(
            codex_subscription_status(b"Logged in using an API key - sk-***"),
            "wrong_auth_mode"
        );
        assert_eq!(codex_subscription_status(b"Not logged in"), "logged_out");
        assert_eq!(codex_subscription_status(b""), "unhealthy");
    }

    #[test]
    fn luna_rollout_rejection_is_model_unavailable() {
        let luna = br#"ERROR: unexpected status 400 Bad Request: {"detail":"The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account."}"#;
        assert!(matches!(
            classify_codex_failure(luna),
            ClaudeWorkerState::ModelUnavailable
        ));
        assert!(matches!(
            classify_codex_failure(b"ERROR: You've hit your usage limit."),
            ClaudeWorkerState::PlanLimit
        ));
        assert!(matches!(
            classify_codex_failure(b"ERROR: 401 Unauthorized"),
            ClaudeWorkerState::SubscriptionUnavailable
        ));
        assert!(matches!(
            classify_codex_failure(b"stream disconnected"),
            ClaudeWorkerState::RuntimeFailure
        ));
    }
}
