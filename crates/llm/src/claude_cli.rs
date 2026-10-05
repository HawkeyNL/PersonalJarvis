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
    types::{
        ChatMessage, ChatReply, ChatRequest, LlmError, ResearchRequest, Role, Tier, Usage,
        RESEARCH_SYSTEM_PROMPT,
    },
    LlmProvider,
};

const WORKER_TIMEOUT: Duration = Duration::from_secs(125);
/// Research runs search the web first; the worker stops them at 300 s.
const RESEARCH_WORKER_TIMEOUT: Duration = Duration::from_secs(310);

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
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.model_for(req.tier).to_owned());
        CLAUDE_WORKER.chat(model, req).await
    }

    async fn research(&self, req: &ResearchRequest) -> Result<ChatReply, LlmError> {
        let model = req.model.clone().unwrap_or_else(|| self.model_hard.clone());
        CLAUDE_WORKER.research(model, req).await
    }
}

/// One local subscription worker: a fixed socket speaking the finite worker
/// protocol. Errors are fixed, non-secret strings.
pub(crate) struct SubscriptionWorker {
    pub(crate) socket: &'static str,
    pub(crate) backend: &'static str,
    pub(crate) name: &'static str,
}

const CLAUDE_WORKER: SubscriptionWorker = SubscriptionWorker {
    socket: SOCKET,
    backend: "claude-cli",
    name: "Claude",
};

impl SubscriptionWorker {
    pub(crate) async fn chat(
        &self,
        model: String,
        req: &ChatRequest,
    ) -> Result<ChatReply, LlmError> {
        let request = ClaudeWorkerRequest {
            protocol: 1,
            model,
            system: req.system.clone(),
            prompt: build_bounded_prompt(&req.messages),
            research: false,
        };
        self.send(request, WORKER_TIMEOUT).await
    }

    /// A research run: the bounded question and the fixed research prompt,
    /// nothing else.
    pub(crate) async fn research(
        &self,
        model: String,
        req: &ResearchRequest,
    ) -> Result<ChatReply, LlmError> {
        self.send(research_worker_request(model, req), RESEARCH_WORKER_TIMEOUT)
            .await
    }

    async fn send(
        &self,
        request: ClaudeWorkerRequest,
        timeout: Duration,
    ) -> Result<ChatReply, LlmError> {
        if !request.valid() {
            return Err(LlmError::NotConfigured(format!(
                "{} worker request exceeds the bounded context policy",
                self.name
            )));
        }
        let model = request.model.clone();
        let payload = serde_json::to_vec(&request).map_err(|_| self.failure())?;
        if payload.len() + 1 > MAX_REQUEST_BYTES {
            return Err(self.failure());
        }
        let reply = tokio::time::timeout(timeout, async {
            let mut socket = UnixStream::connect(self.socket)
                .await
                .map_err(|_| self.failure())?;
            socket
                .write_all(&payload)
                .await
                .map_err(|_| self.failure())?;
            socket.write_all(b"\n").await.map_err(|_| self.failure())?;
            socket.shutdown().await.map_err(|_| self.failure())?;
            let mut response = Vec::new();
            socket
                .take((MAX_REPLY_BYTES + 1) as u64)
                .read_to_end(&mut response)
                .await
                .map_err(|_| self.failure())?;
            if response.len() > MAX_REPLY_BYTES {
                return Err(self.failure());
            }
            serde_json::from_slice::<ClaudeWorkerReply>(&response).map_err(|_| self.failure())
        })
        .await
        .map_err(|_| LlmError::Api {
            status: 504,
            body: format!("{} worker timed out", self.name),
        })??;
        self.reply_to_chat(reply, model)
    }

    fn failure(&self) -> LlmError {
        LlmError::Api {
            status: 503,
            body: format!("{} subscription worker unavailable", self.name),
        }
    }

    fn reply_to_chat(
        &self,
        reply: ClaudeWorkerReply,
        model: String,
    ) -> Result<ChatReply, LlmError> {
        if reply.protocol != 1 {
            return Err(self.failure());
        }
        match reply.state {
            ClaudeWorkerState::SubscriptionUnavailable => Err(LlmError::Api {
                status: 503,
                body: format!("{} subscription unavailable", self.name),
            }),
            ClaudeWorkerState::PlanLimit => Err(LlmError::Api {
                status: 429,
                body: format!("{} subscription plan limit reached", self.name),
            }),
            ClaudeWorkerState::IncompatibleRuntime => Err(LlmError::Api {
                status: 503,
                body: format!("{} subscription runtime is incompatible", self.name),
            }),
            // Bounded and final for this model: never a reason to try a paid
            // API; the router only moves on to the owner's next chain entry.
            ClaudeWorkerState::ModelUnavailable => Err(LlmError::NotConfigured(format!(
                "{} subscription model {model} is unavailable for this account",
                self.name
            ))),
            ClaudeWorkerState::RuntimeFailure => Err(self.failure()),
            ClaudeWorkerState::ToolUseRefused => Err(LlmError::Api {
                status: 502,
                body: format!("{} run attempted tool use and was refused", self.name),
            }),
            ClaudeWorkerState::Completed => {
                let text = reply
                    .text
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(LlmError::Empty)?;
                Ok(ChatReply {
                    text,
                    model,
                    backend: Some(self.backend.into()),
                    requested_route: None,
                    actual_provider: None,
                    fallback_count: 0,
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
}

fn research_worker_request(model: String, req: &ResearchRequest) -> ClaudeWorkerRequest {
    ClaudeWorkerRequest {
        protocol: 1,
        model,
        system: Some(RESEARCH_SYSTEM_PROMPT.to_owned()),
        prompt: req.question().to_owned(),
        research: true,
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
            Role::User => "User: ",
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
    fn research_request_carries_only_the_question_and_the_fixed_prompt() {
        let request = research_worker_request(
            "claude-opus-5".into(),
            &ResearchRequest::new("Wat is de laatste Rust-release?").unwrap(),
        );
        assert!(request.research);
        assert!(request.valid());
        assert_eq!(request.prompt, "Wat is de laatste Rust-release?");
        assert_eq!(request.system.as_deref(), Some(RESEARCH_SYSTEM_PROMPT));
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "protocol": 1,
                "model": "claude-opus-5",
                "system": RESEARCH_SYSTEM_PROMPT,
                "prompt": "Wat is de laatste Rust-release?",
                "research": true,
            })
        );
    }

    #[test]
    fn plan_limit_is_structured_and_non_secret() {
        let result = CLAUDE_WORKER.reply_to_chat(
            ClaudeWorkerReply::failure(ClaudeWorkerState::PlanLimit),
            "claude".into(),
        );
        assert!(matches!(result, Err(LlmError::Api { status: 429, .. })));
        let unavailable = CLAUDE_WORKER.reply_to_chat(
            ClaudeWorkerReply::failure(ClaudeWorkerState::ModelUnavailable),
            "claude".into(),
        );
        assert!(matches!(unavailable, Err(LlmError::NotConfigured(_))));
    }
}
