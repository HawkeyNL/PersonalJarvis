//! Contract of the text-only Codex chat worker. It speaks the same finite
//! request/reply protocol as the Claude worker (`claude_worker_protocol`);
//! only the socket, the identity and the reviewed official runtime differ.

use crate::claude_worker_protocol::ClaudeWorkerState;

pub const SOCKET: &str = "/run/jarvis-codex-chat.sock";

/// Environment variable holding the exact `codex --version` number the owner
/// reviewed against the documented `codex exec` flags this worker uses. The
/// worker reads it once at start; systemd sets it from the optional,
/// root-owned `EnvironmentFile` of `jarvis-codex-chat.service`. Unset, empty
/// or malformed means every run returns `incompatible_runtime` without a
/// model call. A newer CLI needs a new review; there is no "or newer".
pub const REVIEWED_VERSION_ENV: &str = "JARVIS_CODEX_REVIEWED_VERSION";

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
/// subscription; any other login (API key, Bedrock, access token) would not
/// use the plan and never counts. The CLI writes its status line to stderr,
/// possibly after `WARNING:` lines, so callers pass stdout and stderr and the
/// lines are judged one by one.
pub fn codex_subscription_status(output: &[u8]) -> &'static str {
    let Ok(text) = std::str::from_utf8(output) else {
        return "unhealthy";
    };
    let lines: Vec<String> = text
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .filter(|line| !line.is_empty() && !line.starts_with("warning:"))
        .collect();
    let chatgpt = |line: &String| line == "logged in using chatgpt";
    if lines.iter().any(|line| {
        !chatgpt(line)
            && (line.starts_with("logged in using")
                || line.contains("api key")
                || line.contains("api-key"))
    }) {
        "wrong_auth_mode"
    } else if lines.iter().any(chatgpt) {
        "connected"
    } else if lines.iter().any(|line| line == "not logged in") {
        "logged_out"
    } else {
        "unhealthy"
    }
}

/// One line of the `codex exec --json` event stream, reduced to what the
/// worker needs. Schema: codex-rs `exec` JSONL events (`thread.started`,
/// `turn.started|completed|failed`, `item.started|updated|completed`,
/// `error`). It must be re-checked against the reviewed CLI version.
#[derive(Debug, PartialEq, Eq)]
pub enum CodexEvent {
    /// Session or turn lifecycle, reasoning, or a partial agent message.
    Benign,
    /// A completed agent message: the latest one is the answer.
    AgentMessage(String),
    /// The turn finished normally.
    TurnCompleted,
    /// A structured error message, used only to classify a failure.
    Error(String),
    /// Anything else: a command, web search, MCP or tool call, file change,
    /// plan update, an unknown event or item type, or an unparseable line.
    /// The worker stops the run and discards its answer.
    Refused,
}

/// Fail closed: only the event and item types listed here are allowed.
pub fn parse_codex_event(line: &[u8]) -> CodexEvent {
    use serde_json::Value;
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return CodexEvent::Refused;
    };
    let message = |value: &Value| CodexEvent::Error(value.as_str().unwrap_or_default().into());
    match event.get("type").and_then(Value::as_str) {
        Some("thread.started" | "turn.started") => CodexEvent::Benign,
        Some("turn.completed") => CodexEvent::TurnCompleted,
        Some("turn.failed") => message(&event["error"]["message"]),
        Some("error") => message(&event["message"]),
        Some(kind @ ("item.started" | "item.updated" | "item.completed")) => {
            let item = &event["item"];
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => CodexEvent::Benign,
                Some("agent_message") if kind != "item.completed" => CodexEvent::Benign,
                Some("agent_message") => {
                    item["text"].as_str().map_or(CodexEvent::Refused, |text| {
                        CodexEvent::AgentMessage(text.into())
                    })
                }
                Some("error") => message(&item["message"]),
                _ => CodexEvent::Refused,
            }
        }
        _ => CodexEvent::Refused,
    }
}

/// Classify a failed `codex exec` run. Only trusted sources count: structured
/// error events and diagnostic lines starting with `ERROR:`. Model text never
/// reaches this function, so it cannot fake a plan limit or a missing model.
/// The text is only inspected here and then discarded; it never reaches Core.
pub fn classify_codex_failure(structured: &[String], diagnostics: &[u8]) -> ClaudeWorkerState {
    let diagnostics = String::from_utf8_lossy(diagnostics);
    structured
        .iter()
        .map(String::as_str)
        .chain(
            diagnostics
                .lines()
                .filter(|line| line.starts_with("ERROR:")),
        )
        .map(classify_error_message)
        .find(|state| !matches!(state, ClaudeWorkerState::RuntimeFailure))
        .unwrap_or(ClaudeWorkerState::RuntimeFailure)
}

fn classify_error_message(message: &str) -> ClaudeWorkerState {
    let text = message.to_ascii_lowercase();
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
        let output = b"codex-cli 1.2.3\n";
        // Unset, empty or malformed owner review: refuse.
        for reviewed in ["", " ", "v1.2.3", "1.2", "1.2.3 ", "latest"] {
            assert!(!reviewed_codex_version(output, reviewed), "{reviewed:?}");
        }
        // Exact match only.
        assert!(reviewed_codex_version(output, "1.2.3"));
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
        assert_eq!(
            codex_subscription_status(b"Logged in using Amazon Bedrock API key\n"),
            "wrong_auth_mode"
        );
        assert_eq!(
            codex_subscription_status(b"Logged in using access token\n"),
            "wrong_auth_mode"
        );
        assert_eq!(
            codex_subscription_status(b"Logged in using ChatGPT, probably"),
            "wrong_auth_mode"
        );
    }

    #[test]
    fn status_is_read_per_line_after_warnings() {
        // codex-cli 0.160.0: stdout empty, warnings then the status on stderr.
        let warning = "WARNING: proceeding, even though we could not create PATH aliases: \
                       Read-only file system (os error 30)\n";
        let sample = |status: &str| format!("{warning}{status}\n");
        assert_eq!(
            codex_subscription_status(sample("Logged in using ChatGPT").as_bytes()),
            "connected"
        );
        assert_eq!(
            codex_subscription_status(sample("  logged in using chatgpt  ").as_bytes()),
            "connected"
        );
        assert_eq!(
            codex_subscription_status(sample("Not logged in").as_bytes()),
            "logged_out"
        );
        assert_eq!(
            codex_subscription_status(sample("Logged in using an API key - sk-***").as_bytes()),
            "wrong_auth_mode"
        );
        assert_eq!(codex_subscription_status(warning.as_bytes()), "unhealthy");
        // A second, non-ChatGPT login line is never outvoted.
        assert_eq!(
            codex_subscription_status(
                b"Logged in using ChatGPT\nLogged in using an API key - sk-***\n"
            ),
            "wrong_auth_mode"
        );
        assert_eq!(codex_subscription_status(b"\xff\n"), "unhealthy");
    }

    #[test]
    fn luna_rollout_rejection_is_model_unavailable() {
        let luna = br#"ERROR: unexpected status 400 Bad Request: {"detail":"The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account."}"#;
        assert!(matches!(
            classify_codex_failure(&[], luna),
            ClaudeWorkerState::ModelUnavailable
        ));
        assert!(matches!(
            classify_codex_failure(&[], b"ERROR: You've hit your usage limit."),
            ClaudeWorkerState::PlanLimit
        ));
        assert!(matches!(
            classify_codex_failure(&["You've hit your usage limit.".into()], b""),
            ClaudeWorkerState::PlanLimit
        ));
        assert!(matches!(
            classify_codex_failure(&[], b"ERROR: 401 Unauthorized"),
            ClaudeWorkerState::SubscriptionUnavailable
        ));
        assert!(matches!(
            classify_codex_failure(&[], b"stream disconnected"),
            ClaudeWorkerState::RuntimeFailure
        ));
    }

    #[test]
    fn untrusted_text_cannot_choose_the_failure_state() {
        // Model or log text that merely mentions a limit or a missing model,
        // without the CLI's `ERROR:` prefix, stays a plain runtime failure.
        for text in [
            "You've hit your usage limit. Please try again later.",
            "The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account.",
            "  ERROR: usage limit",
            "warning: ERROR: 401 Unauthorized",
        ] {
            assert!(
                matches!(
                    classify_codex_failure(&[], text.as_bytes()),
                    ClaudeWorkerState::RuntimeFailure
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn only_text_events_are_allowed() {
        let event = |line: &str| parse_codex_event(line.as_bytes());
        for benign in [
            r#"{"type":"thread.started","thread_id":"t"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"i0","type":"reasoning","text":""}}"#,
            r#"{"type":"item.completed","item":{"id":"i0","type":"reasoning","text":"thinking"}}"#,
            r#"{"type":"item.updated","item":{"id":"i1","type":"agent_message","text":"Hel"}}"#,
        ] {
            assert_eq!(event(benign), CodexEvent::Benign, "{benign}");
        }
        assert_eq!(
            event(
                r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Hello"}}"#
            ),
            CodexEvent::AgentMessage("Hello".into())
        );
        assert_eq!(
            event(
                r#"{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}"#
            ),
            CodexEvent::TurnCompleted
        );
        assert_eq!(
            event(r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#),
            CodexEvent::Error("You've hit your usage limit.".into())
        );
        assert_eq!(
            event(r#"{"type":"error","message":"stream error"}"#),
            CodexEvent::Error("stream error".into())
        );
    }

    #[test]
    fn tool_unknown_and_malformed_events_are_refused() {
        for line in [
            r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"id","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"i3","type":"web_search","query":"x"}}"#,
            r#"{"type":"item.started","item":{"id":"i4","type":"mcp_tool_call","server":"s","tool":"t"}}"#,
            r#"{"type":"item.completed","item":{"id":"i5","type":"file_change","changes":[]}}"#,
            r#"{"type":"item.completed","item":{"id":"i6","type":"todo_list","items":[]}}"#,
            r#"{"type":"item.completed","item":{"id":"i7","type":"brand_new_tool"}}"#,
            r#"{"type":"item.completed","item":{"id":"i8"}}"#,
            r#"{"type":"item.completed","item":{"id":"i9","type":"agent_message"}}"#,
            r#"{"type":"exec_command_begin","command":["sh"]}"#,
            r#"{"msg":{"type":"agent_message"}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"x"}"#,
            "plain text output",
            "",
        ] {
            assert_eq!(
                parse_codex_event(line.as_bytes()),
                CodexEvent::Refused,
                "{line}"
            );
        }
    }
}
