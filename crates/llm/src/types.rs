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
    pub max_tokens: u32,
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
    #[error("llm provider error: HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("the model declined to answer")]
    Refused,
    #[error("the model returned no text")]
    Empty,
    #[error("llm is not configured: {0}")]
    NotConfigured(String),
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
