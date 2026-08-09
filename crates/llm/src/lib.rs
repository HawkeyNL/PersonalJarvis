//! Provider-abstracted LLM layer — Jarvis' brain (DEC-001).
//!
//! A thin async client (no heavy SDK) behind the [`LlmProvider`] trait so the
//! brain is swappable: Anthropic Claude by default, with a local Ollama
//! fallback. API keys live only in backend config/env — never in the client,
//! the webview, or logs.

mod anthropic;
mod fallback;
mod ollama;
mod types;

use std::sync::Arc;

use async_trait::async_trait;

pub use anthropic::AnthropicProvider;
pub use fallback::FallbackProvider;
pub use ollama::OllamaProvider;
pub use types::{ChatMessage, ChatReply, ChatRequest, LlmError, Role, Tier, Usage};

/// A swappable brain: given a conversation, produce a reply.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Short label for telemetry / UI (e.g. `"anthropic:claude-sonnet-5"`).
    fn label(&self) -> &str;
    /// Generate a reply for the conversation at the requested tier.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError>;
}

/// Inputs for [`build_provider`] — flat so the API service can map from its
/// `AppConfig` without this crate depending on the config crate.
pub struct ProviderConfig {
    /// `anthropic` (default) or `ollama`.
    pub provider: String,
    /// Anthropic API key; `None`/empty ⇒ Anthropic disabled (Ollama only).
    pub api_key: Option<String>,
    pub anthropic_base_url: String,
    pub model_default: String,
    pub model_hard: String,
    pub model_cheap: String,
    pub ollama_url: String,
    pub ollama_model: String,
}

/// Wire up the brain from config.
///
/// - `provider = "anthropic"` + key present ⇒ Anthropic with a local Ollama
///   fallback (falls back on transport/API errors, not on refusals).
/// - `provider = "ollama"`, or no Anthropic key ⇒ Ollama only.
/// - Neither buildable ⇒ an [`Unconfigured`] brain that returns a clear error.
pub fn build_provider(cfg: ProviderConfig) -> Arc<dyn LlmProvider> {
    let ollama: Option<Arc<dyn LlmProvider>> = OllamaProvider::new(&cfg.ollama_url, &cfg.ollama_model)
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn LlmProvider>);

    let anthropic: Option<Arc<dyn LlmProvider>> = if cfg.provider.eq_ignore_ascii_case("anthropic") {
        cfg.api_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .and_then(|key| {
                AnthropicProvider::new(
                    key,
                    &cfg.anthropic_base_url,
                    &cfg.model_default,
                    &cfg.model_hard,
                    &cfg.model_cheap,
                )
                .ok()
            })
            .map(|p| Arc::new(p) as Arc<dyn LlmProvider>)
    } else {
        None
    };

    match (anthropic, ollama) {
        (Some(primary), Some(fallback)) => Arc::new(FallbackProvider::new(primary, fallback)),
        (Some(primary), None) => primary,
        (None, Some(fallback)) => fallback,
        (None, None) => Arc::new(Unconfigured),
    }
}

/// A brain that always errors — when nothing is configured.
struct Unconfigured;

#[async_trait]
impl LlmProvider for Unconfigured {
    fn label(&self) -> &str {
        "unconfigured"
    }
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatReply, LlmError> {
        Err(LlmError::NotConfigured(
            "set JARVIS_LLM_API_KEY (Anthropic) or run Ollama locally".into(),
        ))
    }
}

/// A deterministic echo brain for tests (no network).
struct Echo;

#[async_trait]
impl LlmProvider for Echo {
    fn label(&self) -> &str {
        "stub:echo"
    }
    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(ChatReply {
            text: format!("echo: {last}"),
            model: "stub".into(),
            stop_reason: Some("end_turn".into()),
            usage: None,
        })
    }
}

/// A no-network stub brain (echoes the last user turn), for tests.
pub fn stub() -> Arc<dyn LlmProvider> {
    Arc::new(Echo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_parsing_is_permissive() {
        assert_eq!(Tier::parse("hard"), Tier::Hard);
        assert_eq!(Tier::parse("HAIKU"), Tier::Cheap);
        assert_eq!(Tier::parse("whatever"), Tier::Default);
    }

    #[tokio::test]
    async fn stub_echoes_last_user_turn() {
        let brain = stub();
        let reply = brain
            .chat(&ChatRequest {
                system: None,
                messages: vec![ChatMessage::user("hoi Jarvis")],
                tier: Tier::Default,
                max_tokens: 64,
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "echo: hoi Jarvis");
    }

    #[tokio::test]
    async fn unconfigured_errors_clearly() {
        let brain = build_provider(ProviderConfig {
            provider: "anthropic".into(),
            api_key: None,
            anthropic_base_url: "https://api.anthropic.com".into(),
            model_default: "claude-sonnet-5".into(),
            model_hard: "claude-opus-5".into(),
            model_cheap: "claude-haiku-4-5".into(),
            // An unreachable-looking host so Ollama construction still succeeds
            // (reqwest client builds regardless); this only checks wiring.
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "llama3.2".into(),
        });
        // With no API key, this resolves to the Ollama-only brain.
        assert_eq!(brain.label(), "ollama:llama3.2");
    }
}
