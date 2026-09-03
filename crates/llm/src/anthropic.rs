//! Anthropic Claude provider over the Messages API (`POST /v1/messages`).
//!
//! A thin reqwest client (no heavy SDK). The API key lives only here, injected
//! from backend config/env — never logged. Model IDs per tier are configurable;
//! defaults follow DEC-001 (Claude as the brain): Sonnet balanced, Opus for hard
//! reasoning, Haiku for cheap/fast tasks.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::types::{truncate, ChatReply, ChatRequest, LlmError, Role, Tier, Usage};
use crate::LlmProvider;

/// Anthropic Messages API version (sent as `anthropic-version`).
const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model_default: String,
    model_hard: String,
    model_cheap: String,
    label: String,
}

impl AnthropicProvider {
    /// Build a provider. Fails if the API key is empty.
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(LlmError::NotConfigured(
                "JARVIS_LLM_API_KEY is empty".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let model_default = model_default.into();
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key,
            label: format!("anthropic:{model_default}"),
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
impl LlmProvider for AnthropicProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let model = req
            .model
            .clone()
            .unwrap_or_else(|| self.model_for(req.tier).to_string());
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                    "content": m.content,
                })
            })
            .collect();

        let mut body = json!({
            "model": model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if let Some(system) = &req.system {
            // Cache the (stable) system prompt so repeat calls bill it at the
            // cheap cache-read rate instead of re-processing it every turn.
            body["system"] = json!([{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" },
            }]);
        }

        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
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
        let stop_reason = v
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(String::from);
        // Safety classifiers can decline with HTTP 200 + stop_reason "refusal".
        if stop_reason.as_deref() == Some("refusal") {
            return Err(LlmError::Refused);
        }

        let text = extract_text(&v);
        if text.is_empty() {
            return Err(LlmError::Empty);
        }
        let usage = v.get("usage").map(|u| {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            Usage {
                input_tokens: n("input_tokens"),
                output_tokens: n("output_tokens"),
                cache_read_tokens: n("cache_read_input_tokens"),
                cache_write_tokens: n("cache_creation_input_tokens"),
            }
        });
        Ok(ChatReply {
            text,
            model,
            backend: Some("anthropic-api".into()),
            requested_route: None,
            actual_provider: None,
            stop_reason,
            usage,
        })
    }
}

/// Concatenate all `text`-type content blocks from a Messages API response.
pub(crate) fn extract_text(v: &Value) -> String {
    v.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_blocks_only() {
        let v: Value = serde_json::from_str(
            r#"{"content":[
                {"type":"thinking","thinking":"..."},
                {"type":"text","text":"Hallo "},
                {"type":"text","text":"Gus."}
            ],"stop_reason":"end_turn"}"#,
        )
        .unwrap();
        assert_eq!(extract_text(&v), "Hallo Gus.");
    }

    #[test]
    fn empty_when_no_text() {
        let v: Value = serde_json::from_str(r#"{"content":[],"stop_reason":"end_turn"}"#).unwrap();
        assert_eq!(extract_text(&v), "");
    }

    #[test]
    fn rejects_empty_api_key() {
        let err = AnthropicProvider::new("", "https://api.anthropic.com", "a", "b", "c");
        assert!(matches!(err, Err(LlmError::NotConfigured(_))));
    }
}
