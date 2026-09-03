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

/// Deterministic, bounded routing facts derived from the original user input.
/// This never replaces or summarizes the input: the selected model still
/// receives the complete original conversation. It merely establishes a
/// minimum quality floor before the router considers latency or cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRequirements {
    pub tier: Tier,
    pub needs_research: bool,
    pub likely_tool_use: bool,
    pub safety_sensitive: bool,
    pub routing_reason: &'static str,
}

/// Determine a conservative quality floor without making an extra LLM call.
/// Explicit Deep/Research requests always win. Fast mode remains fast for
/// ordinary utility questions but cannot downgrade a deterministic safety,
/// research, coding or financial-risk signal below the standard tier.
pub fn classify_task(mode: RoutingMode, original_request: &str) -> TaskRequirements {
    let normalized = original_request.to_ascii_lowercase();
    let needs_research = matches!(mode, RoutingMode::Research)
        || [
            "onderzoek",
            "research",
            "bronnen",
            "sources",
            "actueel",
            "latest",
            "nieuws",
        ]
        .iter()
        .any(|needle| normalized.contains(needle));
    let likely_tool_use = [
        "code",
        "debug",
        "repository",
        "repo",
        "bestand",
        "file",
        "analyseer data",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    let safety_sensitive = [
        "trade",
        "handelen",
        "order",
        "portfolio",
        "beveilig",
        "security",
        "secret",
        "credential",
        "wachtwoord",
        "approval",
        "goedkeur",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    let inherently_deep = needs_research
        || likely_tool_use
        || safety_sensitive
        || original_request.chars().count() > 4_000
        || ["vergelijk", "plan", "strategie", "architectuur", "redeneer"]
            .iter()
            .any(|needle| normalized.contains(needle));

    match mode {
        RoutingMode::Deep => TaskRequirements {
            tier: Tier::Hard,
            needs_research,
            likely_tool_use,
            safety_sensitive,
            routing_reason: "Deep reasoning requested; strong-quality routing floor applied.",
        },
        RoutingMode::Research => TaskRequirements {
            tier: Tier::Hard,
            needs_research: true,
            likely_tool_use: true,
            safety_sensitive,
            routing_reason: "Research requested; strong-quality routing floor and approved research tooling may be used.",
        },
        RoutingMode::Fast if !inherently_deep => TaskRequirements {
            tier: Tier::Cheap,
            needs_research,
            likely_tool_use,
            safety_sensitive,
            routing_reason: "Fast mode requested; utility-quality routing floor applied.",
        },
        RoutingMode::Fast => TaskRequirements {
            tier: Tier::Default,
            needs_research,
            likely_tool_use,
            safety_sensitive,
            routing_reason: "Fast mode retained a standard-quality floor for this task's safety or complexity.",
        },
        RoutingMode::Auto if inherently_deep => TaskRequirements {
            tier: Tier::Hard,
            needs_research,
            likely_tool_use,
            safety_sensitive,
            routing_reason: "Task complexity or safety relevance selected a strong-quality routing floor.",
        },
        RoutingMode::Auto => TaskRequirements {
            tier: Tier::Default,
            needs_research,
            likely_tool_use,
            safety_sensitive,
            routing_reason: "Automatic quality/capability routing selected a standard-quality floor.",
        },
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
    /// `huggingface`, `claude-cli`, `ollama`). Drives cost attribution — plan/local are free,
    /// only the metered API backends count against the monthly budget.
    #[serde(default)]
    pub backend: Option<String>,
    /// Non-secret Hugging Face execution policy requested by the owner. This
    /// is separate from the base model identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_route: Option<String>,
    /// Infrastructure provider only when the remote API reports it reliably.
    /// Automatic HF routing remains `None` rather than being guessed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_provider: Option<String>,
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

    #[test]
    fn fast_cannot_downgrade_a_safety_sensitive_task() {
        let task = classify_task(RoutingMode::Fast, "Voer deze trade meteen uit");
        assert_eq!(task.tier, Tier::Default);
        assert!(task.safety_sensitive);
    }

    #[test]
    fn research_and_deep_have_a_strong_floor() {
        assert_eq!(
            classify_task(RoutingMode::Research, "kort").tier,
            Tier::Hard
        );
        assert_eq!(classify_task(RoutingMode::Deep, "kort").tier, Tier::Hard);
        assert_eq!(
            classify_task(RoutingMode::Fast, "bedankt").tier,
            Tier::Cheap
        );
    }
}
