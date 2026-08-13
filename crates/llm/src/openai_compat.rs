//! OpenAI-compatible chat provider — covers OpenAI and DeepSeek, which share the
//! `POST {base}/chat/completions` wire format (`Authorization: Bearer <key>`).
//!
//! A thin reqwest client (no SDK). The API key lives only here, injected from
//! backend config/env — never logged. One type, two instances: OpenAI and
//! DeepSeek differ only in base URL, key, models, and the `backend`/label tag.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::types::{truncate, ChatReply, ChatRequest, LlmError, Role, Tier, Usage};
use crate::LlmProvider;

pub struct OpenAiCompatProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Backend id for cost attribution / routing (`openai-api`, `deepseek-api`).
    backend: String,
    model_default: String,
    model_hard: String,
    model_cheap: String,
    label: String,
}

impl OpenAiCompatProvider {
    /// Build a provider. Fails if the API key is empty. `backend` is the short id
    /// (e.g. `openai-api`); `provider` names it in the label (e.g. `openai`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: &str,
        backend: impl Into<String>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(LlmError::NotConfigured(format!("{provider} API key is empty")));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let model_default = model_default.into();
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            backend: backend.into(),
            label: format!("{provider}:{model_default}"),
            model_default,
            model_hard: model_hard.into(),
            model_cheap: model_cheap.into(),
        })
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
impl LlmProvider for OpenAiCompatProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let model = self.model_for(req.tier).to_string();

        // OpenAI-style messages: the system prompt is the first `system` turn.
        let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if let Some(system) = &req.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.extend(req.messages.iter().map(|m| {
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content,
            })
        }));

        let body = json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });

        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                body: truncate(&text, 400),
            });
        }

        let v: Value = resp.json().await?;
        let (text, finish) = extract_choice(&v);
        // Content filters decline with an empty message + `content_filter`.
        if finish.as_deref() == Some("content_filter") && text.is_empty() {
            return Err(LlmError::Refused);
        }
        if text.is_empty() {
            return Err(LlmError::Empty);
        }
        let usage = v.get("usage").map(|u| {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            // `prompt_tokens` includes cached ones; split them out so cost can
            // bill cache reads at the cheaper rate.
            let cached = u
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let prompt = n("prompt_tokens");
            Usage {
                input_tokens: prompt.saturating_sub(cached),
                output_tokens: n("completion_tokens"),
                cache_read_tokens: cached,
                cache_write_tokens: 0,
            }
        });
        Ok(ChatReply {
            text,
            model,
            backend: Some(self.backend.clone()),
            stop_reason: finish,
            usage,
        })
    }
}

/// Pull the first choice's message text and finish reason from a completion.
pub(crate) fn extract_choice(v: &Value) -> (String, Option<String>) {
    let choice = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|c| c.first());
    let text = choice
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(String::from);
    (text, finish)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_message_and_finish_reason() {
        let v: Value = serde_json::from_str(
            r#"{"choices":[{"message":{"role":"assistant","content":" Hallo Gus. "},
                "finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
        )
        .unwrap();
        let (text, finish) = extract_choice(&v);
        assert_eq!(text, "Hallo Gus.");
        assert_eq!(finish.as_deref(), Some("stop"));
    }

    #[test]
    fn empty_when_no_choices() {
        let v: Value = serde_json::from_str(r#"{"choices":[]}"#).unwrap();
        assert_eq!(extract_choice(&v).0, "");
    }

    #[test]
    fn rejects_empty_api_key() {
        let err = OpenAiCompatProvider::new(
            "openai",
            "openai-api",
            "",
            "https://api.openai.com/v1",
            "a",
            "b",
            "c",
        );
        assert!(matches!(err, Err(LlmError::NotConfigured(_))));
    }
}
