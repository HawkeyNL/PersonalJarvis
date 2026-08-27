//! Provider-neutral request/reply types for the brain.

use serde::Serialize;

/// Who authored a conversation turn. The `system` prompt is carried separately
/// on [`ChatRequest`] because the Anthropic Messages API wants it top-level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// A single conversation turn.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
        }
    }
}

/// Capability/cost tier. The provider maps each tier to a concrete model
/// (e.g. Anthropic: default→Sonnet, hard→Opus, cheap→Haiku).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    /// Balanced default "brain".
    #[default]
    Default,
    /// Hardest reasoning (slower, pricier).
    Hard,
    /// Fast, cheap tasks.
    Cheap,
}

/// User-visible routing intent.  This is deliberately provider-neutral: it
/// expresses the required depth, not a credential or a mutable model alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    #[default]
    Auto,
    Fast,
    Deep,
    Research,
}

impl RoutingMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" | "cheap" | "quick" => Self::Fast,
            "deep" | "hard" => Self::Deep,
            "research" => Self::Research,
            _ => Self::Auto,
        }
    }

    pub fn tier(self) -> Tier {
        match self {
            Self::Fast => Tier::Cheap,
            Self::Deep | Self::Research => Tier::Hard,
            Self::Auto => Tier::Default,
        }
    }
}

impl Tier {
    /// Parse a tier hint from the client (permissive; unknown ⇒ default).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "hard" | "opus" | "deep" => Tier::Hard,
            "cheap" | "fast" | "haiku" | "quick" => Tier::Cheap,
            _ => Tier::Default,
        }
    }
}

/// A brain request: an optional system prompt plus the conversation so far.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tier: Tier,
    /// Requested semantic mode.  `tier` remains for internal backwards
    /// compatibility with existing orchestration callers.
    pub mode: RoutingMode,
    pub max_tokens: u32,
    /// Explicit model override chosen by the router (ADR-028 fase 2). `None` ⇒
    /// the provider picks its own model for `tier`.
    pub model: Option<String>,
}

/// Token usage for a reply, when the provider reports it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Tokens served from the prompt cache (cheap).
    pub cache_read_tokens: u32,
    /// Tokens written into the prompt cache.
    pub cache_write_tokens: u32,
}

/// A brain reply.
#[derive(Debug, Clone, Serialize)]
pub struct ChatReply {
    /// The visible answer text.
    pub text: String,
    /// The concrete model that produced it.
    pub model: String,
    /// Which backend produced it (`anthropic-api`, `openai-api`, `deepseek-api`,
    /// `claude-cli`, `ollama`). Drives cost attribution — plan/local are free,
    /// only the metered API backends count against the monthly budget.
    #[serde(default)]
    pub backend: Option<String>,
    /// Provider stop reason, when reported.
    pub stop_reason: Option<String>,
    /// Token usage, when the provider reports it.
    pub usage: Option<Usage>,
}

/// Errors from the brain.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("llm transport failed")]
    Http(#[from] reqwest::Error),
    // Provider bodies can reflect request material or contain operational
    // details.  Keep them for in-process classification, but never render
    // them through Display: callers routinely log `LlmError`.
    #[error("llm provider returned HTTP {status}")]
    Api { status: u16, body: String },
    #[error("the model declined to answer")]
    Refused,
    #[error("the model returned no text")]
    Empty,
    #[error("llm is not configured: {0}")]
    NotConfigured(String),
}

/// Safe, provider-neutral failure information for routing and observability.
/// It intentionally excludes provider response text, prompts and credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailure {
    Transport,
    RateLimited,
    Authentication,
    Unavailable,
    ContextOverflow,
    MalformedResponse,
    Refused,
    NotConfigured,
}

impl LlmError {
    pub fn failure_category(&self) -> ProviderFailure {
        match self {
            Self::Http(_) => ProviderFailure::Transport,
            Self::Api {
                status: 401 | 403, ..
            } => ProviderFailure::Authentication,
            Self::Api {
                status: 408 | 504, ..
            } => ProviderFailure::Transport,
            Self::Api {
                status: 413 | 422,
                body,
            } if body.to_ascii_lowercase().contains("context") => ProviderFailure::ContextOverflow,
            Self::Api { status: 429, .. } => ProviderFailure::RateLimited,
            Self::Api {
                status: 500..=599, ..
            } => ProviderFailure::Unavailable,
            Self::Api { .. } | Self::Empty => ProviderFailure::MalformedResponse,
            Self::Refused => ProviderFailure::Refused,
            Self::NotConfigured(_) => ProviderFailure::NotConfigured,
        }
    }
}

/// Truncate a string to at most `max` characters (char-safe), for error bodies.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_display_redacts_body_but_classifies_it() {
        let error = LlmError::Api {
            status: 401,
            body: "recognizable-test-secret".into(),
        };
        assert_eq!(error.failure_category(), ProviderFailure::Authentication);
        assert!(!error.to_string().contains("recognizable-test-secret"));
    }

    #[test]
    fn rate_limit_and_context_overflow_are_distinct() {
        assert_eq!(
            LlmError::Api {
                status: 429,
                body: String::new()
            }
            .failure_category(),
            ProviderFailure::RateLimited
        );
        assert_eq!(
            LlmError::Api {
                status: 413,
                body: "context length exceeded".into()
            }
            .failure_category(),
            ProviderFailure::ContextOverflow
        );
    }
}
