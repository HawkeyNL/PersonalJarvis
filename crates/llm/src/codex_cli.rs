//! Subscription-backed Codex chat (for example GPT-6 Luna through a ChatGPT
//! plan) through the local text-only worker. Core never executes the official
//! CLI or reads its credential store, and this backend is never metered.

use async_trait::async_trait;

use crate::{
    claude_cli::SubscriptionWorker,
    codex_chat_protocol::SOCKET,
    types::{ChatReply, ChatRequest, LlmError, ResearchRequest},
    LlmProvider,
};

const CODEX_WORKER: SubscriptionWorker = SubscriptionWorker {
    socket: SOCKET,
    backend: "codex-cli",
    name: "Codex",
};

/// Off by default: the router reaches it only through an owner-routed chain
/// entry or a pin, always with an exact owner-enabled model.
pub struct CodexCliProvider;

#[async_trait]
impl LlmProvider for CodexCliProvider {
    fn label(&self) -> &str {
        "codex-cli"
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        // No configured default model: an implicit alias could silently
        // change what the subscription runs.
        let model = req
            .model
            .clone()
            .ok_or_else(|| LlmError::NotConfigured("codex-cli needs an exact model".into()))?;
        CODEX_WORKER.chat(model, req).await
    }

    async fn research(&self, req: &ResearchRequest) -> Result<ChatReply, LlmError> {
        let model = req
            .model
            .clone()
            .ok_or_else(|| LlmError::NotConfigured("codex-cli needs an exact model".into()))?;
        CODEX_WORKER.research(model, req).await
    }
}
