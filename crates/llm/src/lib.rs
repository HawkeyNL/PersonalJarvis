//! Provider-abstracted LLM layer — Jarvis' brain (DEC-001).
//!
//! A thin async client (no heavy SDK) behind the [`LlmProvider`] trait so the
//! brain is swappable: Anthropic Claude by default, with a local Ollama
//! fallback. API keys live only in backend config/env — never in the client,
//! the webview, or logs.

mod anthropic;
mod claude_cli;
mod fallback;
mod model_policy;
mod ollama;
mod openai_compat;
mod router;
mod types;

use std::sync::Arc;

use async_trait::async_trait;

pub use anthropic::AnthropicProvider;
pub use claude_cli::ClaudeCliProvider;
pub use fallback::FallbackProvider;
pub use model_policy::{ModelAccessEntry, ModelAccessPolicy};
pub use ollama::OllamaProvider;
pub use openai_compat::OpenAiCompatProvider;
pub use router::{always_available, Availability, CatalogModel, ModelClass, RouterProvider};
pub use types::{
    ChatMessage, ChatReply, ChatRequest, LlmError, ProviderFailure, Role, RoutingMode, Tier, Usage,
};

/// A swappable brain: given a conversation, produce a reply.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Short label for telemetry / UI (e.g. `"anthropic:claude-sonnet-5"`).
    fn label(&self) -> &str;
    /// Generate a reply for the conversation at the requested tier.
    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError>;
}

/// One OpenAI-compatible backend's settings (OpenAI or DeepSeek). Key is
/// `None`/empty ⇒ that backend is disabled.
#[derive(Default, Clone)]
pub struct OpenAiBackend {
    pub api_key: Option<String>,
    pub base_url: String,
    pub model_default: String,
    pub model_hard: String,
    pub model_cheap: String,
}

/// Inputs for [`build_provider`]/[`build_router`] — flat so the API service can
/// map from its `AppConfig` without this crate depending on the config crate.
pub struct ProviderConfig {
    /// `router`/`auto` (smart), `anthropic`, `claude-cli`, `openai`, `deepseek`
    /// or `ollama`.
    pub provider: String,
    /// Anthropic API key; `None`/empty ⇒ Anthropic disabled (used as fallback).
    pub api_key: Option<String>,
    pub anthropic_base_url: String,
    pub model_default: String,
    pub model_hard: String,
    pub model_cheap: String,
    pub ollama_url: String,
    pub ollama_model: String,
    /// Path/name of the `claude` CLI (for `provider = "claude-cli"`).
    pub claude_cli_bin: String,
    /// OpenAI backend (key + base URL + per-tier models).
    pub openai: OpenAiBackend,
    /// DeepSeek backend (OpenAI-compatible; key + base URL + per-tier models).
    pub deepseek: OpenAiBackend,
    /// xAI/Grok backend (OpenAI-compatible).
    pub xai: OpenAiBackend,
    /// Z.ai/GLM backend (OpenAI-compatible when explicitly configured).
    pub zai: OpenAiBackend,
    /// Credentialed remote Ollama API.  This is intentionally distinct from
    /// the local loopback Ollama provider.
    pub ollama_cloud: OpenAiBackend,
}

/// Build the local Ollama brain, if the client constructs (no network yet).
fn build_ollama(cfg: &ProviderConfig) -> Option<Arc<dyn LlmProvider>> {
    OllamaProvider::new(&cfg.ollama_url, &cfg.ollama_model)
        .ok()
        .map(|p| Arc::new(p) as Arc<dyn LlmProvider>)
}

/// Build the Anthropic API brain whenever a key is present — as a primary or a
/// fallback. Keys stay backend-only (ADR-022).
fn build_anthropic(cfg: &ProviderConfig) -> Option<Arc<dyn LlmProvider>> {
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
}

/// Build the Claude *plan* brain via the local `claude` CLI (always constructs;
/// a missing/logged-out CLI surfaces as an error at call time).
fn build_claude_cli(cfg: &ProviderConfig) -> Arc<dyn LlmProvider> {
    Arc::new(ClaudeCliProvider::new(
        &cfg.claude_cli_bin,
        &cfg.model_default,
        &cfg.model_hard,
        &cfg.model_cheap,
    ))
}

/// Build an OpenAI-compatible brain (OpenAI or DeepSeek) if a key is present.
fn build_openai_compat(
    provider: &str,
    backend_id: &str,
    b: &OpenAiBackend,
) -> Option<Arc<dyn LlmProvider>> {
    b.api_key
        .as_deref()
        .filter(|k| !k.trim().is_empty())
        .and_then(|key| {
            OpenAiCompatProvider::new(
                provider,
                backend_id,
                key,
                &b.base_url,
                &b.model_default,
                &b.model_hard,
                &b.model_cheap,
            )
            .ok()
        })
        .map(|p| Arc::new(p) as Arc<dyn LlmProvider>)
}

/// Wire up the brain from config, cheapest-capable-first (ADR-027):
///
/// - `provider = "router"` / `"auto"` ⇒ the registry-aware [`RouterProvider`]
///   (see [`build_router`]); the smart mode that picks per task.
/// - `provider = "claude-cli"` ⇒ your Claude *plan* via the `claude` CLI, with
///   the Anthropic API (else Ollama) as the fallback when the plan is full.
/// - `provider = "anthropic"` + key ⇒ Anthropic, Ollama as local fallback.
/// - `provider = "ollama"`, or nothing else buildable ⇒ Ollama only.
/// - Nothing buildable ⇒ an [`Unconfigured`] brain that returns a clear error.
///
/// Fallback is reactive: try the primary, fall through on any error except a
/// genuine refusal. (Proactive %-of-plan routing is a later stage.)
///
/// For `router`/`auto`, callers should use [`build_router`] directly so they can
/// pass a live [`Availability`] source; this function falls back to
/// [`always_available`] so it stays a pure `ProviderConfig -> brain` map.
pub fn build_provider(cfg: ProviderConfig) -> Arc<dyn LlmProvider> {
    let ollama = build_ollama(&cfg);
    let anthropic = build_anthropic(&cfg);

    match cfg.provider.to_ascii_lowercase().as_str() {
        "router" | "auto" => build_router(cfg, always_available(), Vec::new()),
        "claude-cli" => {
            let cli = build_claude_cli(&cfg);
            match anthropic.or(ollama) {
                Some(fallback) => Arc::new(FallbackProvider::new(cli, fallback)),
                None => cli,
            }
        }
        "openai" => single_or_fallback(
            build_openai_compat("openai", "openai-api", &cfg.openai),
            ollama,
        ),
        "deepseek" => single_or_fallback(
            build_openai_compat("deepseek", "deepseek-api", &cfg.deepseek),
            ollama,
        ),
        "ollama" => ollama.unwrap_or_else(|| Arc::new(Unconfigured)),
        // Default: Anthropic API primary, Ollama fallback.
        _ => match (anthropic, ollama) {
            (Some(primary), Some(fallback)) => Arc::new(FallbackProvider::new(primary, fallback)),
            (Some(primary), None) => primary,
            (None, Some(fallback)) => fallback,
            (None, None) => Arc::new(Unconfigured),
        },
    }
}

/// A `primary` with a `fallback` behind it — or whichever exists, or Unconfigured.
fn single_or_fallback(
    primary: Option<Arc<dyn LlmProvider>>,
    fallback: Option<Arc<dyn LlmProvider>>,
) -> Arc<dyn LlmProvider> {
    match (primary, fallback) {
        (Some(p), Some(f)) => Arc::new(FallbackProvider::new(p, f)),
        (Some(p), None) => p,
        (None, Some(f)) => f,
        (None, None) => Arc::new(Unconfigured),
    }
}

/// Build the registry-aware [`RouterProvider`] over every buildable backend. The
/// router decides per request which backend to try (consulting `availability` +
/// a per-tier cost/quality policy) *and*, from `catalog`, which model — cheapest
/// sufficient, low models by default (ADR-028 fase 2). An empty catalog ⇒ each
/// provider uses its own tier model. Falls back to [`Unconfigured`] if nothing
/// is buildable.
pub fn build_router(
    cfg: ProviderConfig,
    availability: Arc<dyn Availability>,
    catalog: Vec<router::CatalogModel>,
) -> Arc<dyn LlmProvider> {
    build_router_with_policy(
        cfg,
        availability,
        catalog,
        ModelAccessPolicy::deny_by_default(),
    )
}

/// Build a router with the explicit owner model allowlist.  A configured API
/// key is deliberately insufficient: only exact enabled provider/model pairs
/// can reach a remote provider.
pub fn build_router_with_policy(
    cfg: ProviderConfig,
    availability: Arc<dyn Availability>,
    catalog: Vec<router::CatalogModel>,
    model_policy: ModelAccessPolicy,
) -> Arc<dyn LlmProvider> {
    let mut candidates = Vec::new();
    if let Some(ollama) = build_ollama(&cfg) {
        candidates.push(router::Candidate {
            id: "ollama".into(),
            provider: ollama,
        });
    }
    candidates.push(router::Candidate {
        id: "claude-cli".into(),
        provider: build_claude_cli(&cfg),
    });
    if let Some(deepseek) = build_openai_compat("deepseek", "deepseek-api", &cfg.deepseek) {
        candidates.push(router::Candidate {
            id: "deepseek-api".into(),
            provider: deepseek,
        });
    }
    if let Some(openai) = build_openai_compat("openai", "openai-api", &cfg.openai) {
        candidates.push(router::Candidate {
            id: "openai-api".into(),
            provider: openai,
        });
    }
    if let Some(anthropic) = build_anthropic(&cfg) {
        candidates.push(router::Candidate {
            id: "anthropic-api".into(),
            provider: anthropic,
        });
    }
    if let Some(xai) = build_openai_compat("xai", "xai-api", &cfg.xai) {
        candidates.push(router::Candidate {
            id: "xai-api".into(),
            provider: xai,
        });
    }
    if let Some(zai) = build_openai_compat("zai", "zai-api", &cfg.zai) {
        candidates.push(router::Candidate {
            id: "zai-api".into(),
            provider: zai,
        });
    }
    if let Some(ollama_cloud) =
        build_openai_compat("ollama-cloud", "ollama-cloud", &cfg.ollama_cloud)
    {
        candidates.push(router::Candidate {
            id: "ollama-cloud".into(),
            provider: ollama_cloud,
        });
    }
    if candidates.is_empty() {
        return Arc::new(Unconfigured);
    }
    Arc::new(RouterProvider::with_policy(
        candidates,
        availability,
        catalog,
        model_policy,
    ))
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
            backend: Some("stub".into()),
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
                mode: RoutingMode::Auto,
                max_tokens: 64,
                model: None,
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
            claude_cli_bin: "claude".into(),
            openai: OpenAiBackend::default(),
            deepseek: OpenAiBackend::default(),
            xai: OpenAiBackend::default(),
            zai: OpenAiBackend::default(),
            ollama_cloud: OpenAiBackend::default(),
        });
        // With no API key, this resolves to the Ollama-only brain.
        assert_eq!(brain.label(), "ollama:llama3.2");
    }

    #[test]
    fn claude_cli_falls_back_to_api_when_keyed() {
        let brain = build_provider(ProviderConfig {
            provider: "claude-cli".into(),
            api_key: Some("sk-ant-test".into()),
            anthropic_base_url: "https://api.anthropic.com".into(),
            model_default: "claude-sonnet-5".into(),
            model_hard: "claude-opus-5".into(),
            model_cheap: "claude-haiku-4-5".into(),
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "llama3.2".into(),
            claude_cli_bin: "claude".into(),
            openai: OpenAiBackend::default(),
            deepseek: OpenAiBackend::default(),
            xai: OpenAiBackend::default(),
            zai: OpenAiBackend::default(),
            ollama_cloud: OpenAiBackend::default(),
        });
        // CLI primary, API as the fallback ("vangnet als de CLI vol is").
        assert_eq!(
            brain.label(),
            "claude-cli:claude-sonnet-5→anthropic:claude-sonnet-5"
        );
    }

    #[test]
    fn router_mode_wires_every_backend() {
        let brain = build_provider(ProviderConfig {
            provider: "router".into(),
            api_key: Some("sk-ant-test".into()),
            anthropic_base_url: "https://api.anthropic.com".into(),
            model_default: "claude-sonnet-5".into(),
            model_hard: "claude-opus-5".into(),
            model_cheap: "claude-haiku-4-5".into(),
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "llama3.2".into(),
            claude_cli_bin: "claude".into(),
            openai: OpenAiBackend::default(),
            deepseek: OpenAiBackend::default(),
            xai: OpenAiBackend::default(),
            zai: OpenAiBackend::default(),
            ollama_cloud: OpenAiBackend::default(),
        });
        // Registry-aware router over local + plan + API, in fixed id order.
        assert_eq!(brain.label(), "router[ollama,claude-cli,anthropic-api]");
    }
}
