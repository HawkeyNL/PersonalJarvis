//! Subscription-backed Claude brain through a local unprivileged worker.
//! Core never executes the official CLI or reads its credential store.

use std::time::Duration;

use async_trait::async_trait;
use tokio::{io::AsyncReadExt, io::AsyncWriteExt, net::UnixStream};

use crate::{
    claude_worker_protocol::{
        ClaudeWorkerReply, ClaudeWorkerRequest, ClaudeWorkerState, MAX_REPLY_BYTES,
        MAX_REQUEST_BYTES, SOCKET,
    },
    types::{ChatMessage, ChatReply, ChatRequest, LlmError, Role, Tier, Usage},
    LlmProvider,
};

const WORKER_TIMEOUT: Duration = Duration::from_secs(125);

pub struct ClaudeCliProvider {
    model_default: String,
    model_hard: String,
    model_cheap: String,
    label: String,
}

impl ClaudeCliProvider {
    pub fn new(
        _legacy_binary: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
    ) -> Self {
        // Preserve constructor compatibility, never execute a caller path in Core.
        let model_default = model_default.into();
        Self {
            label: format!("claude-cli:{model_default}"),
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
        let request = ClaudeWorkerRequest {
            protocol: 1,
            model: req
                .model
                .clone()
                .unwrap_or_else(|| self.model_for(req.tier).to_owned()),
            system: req.system.clone(),
            prompt: build_bounded_prompt(&req.messages),
        };
        if !request.valid() {
            return Err(LlmError::NotConfigured(
                "Claude worker request exceeds the bounded context policy".into(),
            ));
        }
        let model = request.model.clone();
        let payload = serde_json::to_vec(&request).map_err(|_| safe_failure())?;
        if payload.len() + 1 > MAX_REQUEST_BYTES {
            return Err(safe_failure());
        }
        let reply = tokio::time::timeout(WORKER_TIMEOUT, async {
            let mut socket = UnixStream::connect(SOCKET)
                .await
                .map_err(|_| safe_failure())?;
            socket
                .write_all(&payload)
                .await
                .map_err(|_| safe_failure())?;
            socket.write_all(b"\n").await.map_err(|_| safe_failure())?;
            socket.shutdown().await.map_err(|_| safe_failure())?;
            let mut response = Vec::new();
            socket
                .take((MAX_REPLY_BYTES + 1) as u64)
                .read_to_end(&mut response)
                .await
                .map_err(|_| safe_failure())?;
            if response.len() > MAX_REPLY_BYTES {
                return Err(safe_failure());
            }
            serde_json::from_slice::<ClaudeWorkerReply>(&response).map_err(|_| safe_failure())
        })
        .await
        .map_err(|_| LlmError::Api {
            status: 504,
            body: "Claude worker timed out".into(),
        })??;
        reply_to_chat(reply, model)
    }
}

fn safe_failure() -> LlmError {
    LlmError::Api {
        status: 503,
        body: "Claude subscription worker unavailable".into(),
    }
}

fn reply_to_chat(reply: ClaudeWorkerReply, model: String) -> Result<ChatReply, LlmError> {
    if reply.protocol != 1 {
        return Err(safe_failure());
    }
    match reply.state {
        ClaudeWorkerState::SubscriptionUnavailable => Err(LlmError::Api {
            status: 503,
            body: "Claude subscription unavailable".into(),
        }),
        ClaudeWorkerState::PlanLimit => Err(LlmError::Api {
            status: 429,
            body: "Claude subscription plan limit reached".into(),
        }),
        ClaudeWorkerState::IncompatibleRuntime => Err(LlmError::Api {
            status: 503,
            body: "Claude subscription runtime is incompatible".into(),
        }),
        ClaudeWorkerState::RuntimeFailure => Err(safe_failure()),
        ClaudeWorkerState::Completed => {
            let text = reply
                .text
                .filter(|text| !text.trim().is_empty())
                .ok_or(LlmError::Empty)?;
            Ok(ChatReply {
                text,
                model,
                backend: Some("claude-cli".into()),
                requested_route: None,
                actual_provider: None,
                stop_reason: Some("end_turn".into()),
                usage: Some(Usage {
                    input_tokens: reply.input_tokens.unwrap_or(0),
                    output_tokens: reply.output_tokens.unwrap_or(0),
                    cache_read_tokens: reply.cache_read_tokens.unwrap_or(0),
                    cache_write_tokens: reply.cache_write_tokens.unwrap_or(0),
                }),
            })
        }
    }
}

/// Include only the most recent eight turns, with an aggregate character cap.
fn build_bounded_prompt(messages: &[ChatMessage]) -> String {
    let mut selected = Vec::new();
    let mut remaining = 24_000;
    for message in messages.iter().rev().take(8) {
        let content: String = message
            .content
            .chars()
            .rev()
            .take(remaining)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        remaining -= content.chars().count();
        selected.push((message.role, content));
        if remaining == 0 {
            break;
        }
    }
    selected.reverse();
    let mut prompt = String::new();
    for (role, content) in selected {
        prompt.push_str(match role {
            Role::User => "Gebruiker: ",
            Role::Assistant => "Jarvis: ",
        });
        prompt.push_str(&content);
        prompt.push('\n');
    }
    prompt.push_str("Jarvis:");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_compiler_is_bounded_and_recent() {
        let mut messages = vec![ChatMessage::user("old private history")];
        messages.extend((0..9).map(|number| ChatMessage::user(format!("recent {number}"))));
        let prompt = build_bounded_prompt(&messages);
        assert!(!prompt.contains("old private history"));
        assert!(!prompt.contains("recent 0"));
        assert!(prompt.contains("recent 8"));
        let large = build_bounded_prompt(&[ChatMessage::user("x".repeat(100_000))]);
        assert!(large.len() < 25_000);
    }

    #[test]
    fn plan_limit_is_structured_and_non_secret() {
        let result = reply_to_chat(
            ClaudeWorkerReply::failure(ClaudeWorkerState::PlanLimit),
            "claude".into(),
        );
        assert!(matches!(result, Err(LlmError::Api { status: 429, .. })));
    }
}
