//! Brain via the local `claude` CLI (Claude Code) in headless mode, so Jarvis
//! runs on the user's Claude *subscription* instead of the metered API (ADR-027).
//!
//! Every CLI failure — a non-zero exit, unparseable output, or an `is_error`
//! result (which is how a plan/rate limit surfaces) — is returned as an
//! `LlmError`, so a [`FallbackProvider`](crate::FallbackProvider) falls through
//! to the API: "API als vangnet als de CLI vol is." A genuine answer is returned
//! as-is; no tools run (the neutral working dir keeps Claude Code out of the repo).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::types::{truncate, ChatMessage, ChatReply, ChatRequest, LlmError, Role, Tier, Usage};
use crate::LlmProvider;

/// How long to wait for the CLI before giving up (and letting the fallback run).
const CLI_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ClaudeCliProvider {
    bin: String,
    model_default: String,
    model_hard: String,
    model_cheap: String,
    label: String,
}

impl ClaudeCliProvider {
    pub fn new(
        bin: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
    ) -> Self {
        let bin = bin.into();
        let model_default = model_default.into();
        Self {
            label: format!("claude-cli:{model_default}"),
            bin,
            model_default,
            model_hard: model_hard.into(),
            model_cheap: model_cheap.into(),
        }
    }

    fn model_for(&self, tier: Tier) -> &str {
        match tier {
            Tier::Default => &self.model_default,
            Tier::Hard => &self.model_hard,
            Tier::Cheap => &self.model_cheap,
        }
    }
}

#[async_trait]
impl LlmProvider for ClaudeCliProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let prompt = build_prompt(&req.messages);
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.model_for(req.tier).to_string());

        let mut cmd = Command::new(&self.bin);
        cmd.arg("-p")
            .arg(&prompt)
            .arg("--output-format")
            .arg("json")
            .arg("--model")
            .arg(&model);
        if let Some(system) = req.system.as_deref().filter(|s| !s.trim().is_empty()) {
            cmd.arg("--append-system-prompt").arg(system);
        }
        // Run somewhere neutral so headless Claude Code can't wander the repo.
        cmd.current_dir(std::env::temp_dir());
        cmd.kill_on_drop(true);

        let output = tokio::time::timeout(CLI_TIMEOUT, cmd.output())
            .await
            .map_err(|_| LlmError::Api {
                status: 504,
                body: "claude CLI timed out".into(),
            })?
            .map_err(|e| LlmError::NotConfigured(format!("claude CLI '{}': {e}", self.bin)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output.status.code().unwrap_or(-1);
            return Err(LlmError::Api {
                status: 502,
                body: format!("claude CLI exit {code}: {}", truncate(stderr.trim(), 300)),
            });
        }

        parse_cli_output(&String::from_utf8_lossy(&output.stdout), &model)
    }
}

/// Flatten the conversation into a single prompt. One turn passes through as-is;
/// multi-turn becomes a labeled transcript ending on Jarvis' cue.
fn build_prompt(messages: &[ChatMessage]) -> String {
    match messages {
        [] => String::new(),
        [only] => only.content.clone(),
        many => {
            let mut s = String::new();
            for m in many {
                let who = match m.role {
                    Role::User => "Gebruiker",
                    Role::Assistant => "Jarvis",
                };
                s.push_str(who);
                s.push_str(": ");
                s.push_str(&m.content);
                s.push('\n');
            }
            s.push_str("Jarvis:");
            s
        }
    }
}

#[derive(Deserialize)]
struct CliResult {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    usage: Option<CliUsage>,
}

#[derive(Deserialize)]
struct CliUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

/// Parse `claude -p --output-format json` output into a reply. An `is_error`
/// result (e.g. a usage/plan limit) becomes an error so the fallback can run.
fn parse_cli_output(stdout: &str, model: &str) -> Result<ChatReply, LlmError> {
    let parsed: CliResult = serde_json::from_str(stdout.trim()).map_err(|e| LlmError::Api {
        status: 502,
        body: format!("unparseable claude CLI output: {e}: {}", truncate(stdout, 200)),
    })?;

    if parsed.is_error {
        let detail = if parsed.result.trim().is_empty() {
            parsed.subtype
        } else {
            parsed.result
        };
        // 429 → looks like a limit; let the fallback (API) take over.
        return Err(LlmError::Api {
            status: 429,
            body: truncate(detail.trim(), 300),
        });
    }

    let text = parsed.result.trim().to_string();
    if text.is_empty() {
        return Err(LlmError::Empty);
    }
    Ok(ChatReply {
        text,
        model: model.to_string(),
        backend: Some("claude-cli".into()),
        stop_reason: Some("end_turn".into()),
        usage: parsed.usage.map(|u| Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_write_tokens: u.cache_creation_input_tokens,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_successful_result() {
        let json = r#"{"type":"result","subtype":"success","is_error":false,
            "result":"Hallo, ik ben Jarvis.","session_id":"x",
            "usage":{"input_tokens":12,"output_tokens":7,"cache_read_input_tokens":3,"cache_creation_input_tokens":0}}"#;
        let reply = parse_cli_output(json, "claude-sonnet-5").unwrap();
        assert_eq!(reply.text, "Hallo, ik ben Jarvis.");
        assert_eq!(reply.model, "claude-sonnet-5");
        let u = reply.usage.unwrap();
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.cache_read_tokens, 3);
    }

    #[test]
    fn an_error_result_becomes_a_fallback_error() {
        let json = r#"{"type":"result","subtype":"error_usage_limit","is_error":true,
            "result":"usage limit reached"}"#;
        match parse_cli_output(json, "claude-sonnet-5") {
            Err(LlmError::Api { status, body }) => {
                assert_eq!(status, 429);
                assert!(body.contains("usage limit"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn multi_turn_prompt_is_labeled() {
        let p = build_prompt(&[
            ChatMessage::user("hoi"),
            ChatMessage::assistant("hallo"),
            ChatMessage::user("hoe gaat het?"),
        ]);
        assert!(p.contains("Gebruiker: hoi"));
        assert!(p.contains("Jarvis: hallo"));
        assert!(p.trim_end().ends_with("Jarvis:"));
    }

    #[test]
    fn single_turn_prompt_passes_through() {
        assert_eq!(build_prompt(&[ChatMessage::user("alleen dit")]), "alleen dit");
    }
}
