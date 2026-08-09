//! A composite provider: try the primary brain, fall back to a local one.
//!
//! Falls back on transport/API failures only. A genuine safety refusal is
//! surfaced as-is — retrying a declined request locally would just launder it.

use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{ChatReply, ChatRequest, LlmError};
use crate::LlmProvider;

pub struct FallbackProvider {
    primary: Arc<dyn LlmProvider>,
    fallback: Arc<dyn LlmProvider>,
    label: String,
}

impl FallbackProvider {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback: Arc<dyn LlmProvider>) -> Self {
        let label = format!("{}→{}", primary.label(), fallback.label());
        Self {
            primary,
            fallback,
            label,
        }
    }
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        match self.primary.chat(req).await {
            Ok(reply) => Ok(reply),
            Err(LlmError::Refused) => Err(LlmError::Refused),
            Err(e) => {
                tracing::warn!(error = %e, "primary brain failed; falling back to local");
                self.fallback.chat(req).await
            }
        }
    }
}
