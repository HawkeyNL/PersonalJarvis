//! Local Ollama provider (`POST /api/chat`) — the offline fallback brain.
//!
//! No API key, no cloud: runs against a local `ollama serve`. Used on its own
//! (`JARVIS_LLM_PROVIDER=ollama`) or as the fallback when the cloud brain fails.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::types::{truncate, ChatReply, ChatRequest, LlmError, Role, Usage};
use crate::LlmProvider;

pub struct OllamaProvider {
    http: reqwest::Client,
    base_url: String,
    model: String,
    label: String,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Result<Self, LlmError> {
        // Local models can be slow on first token; allow a generous timeout.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let model = model.into();
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            label: format!("ollama:{model}"),
            model,
        })
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        // The router may pick a specific local model; else use the configured one.
        let model = req.model.clone().unwrap_or_else(|| self.model.clone());
        // Ollama takes the system prompt inline as a leading message.
        let mut msgs: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
        if let Some(system) = &req.system {
            msgs.push(json!({ "role": "system", "content": system }));
        }
        for m in &req.messages {
            msgs.push(json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": m.content,
            }));
        }

        let body = json!({
            "model": model,
            "messages": msgs,
            "stream": false,
            "options": { "num_predict": req.max_tokens },
        });

        let resp = self
            .http
            .post(format!("{}/api/chat", self.base_url))
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
        let text = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() {
            return Err(LlmError::Empty);
        }
        let usage = Usage {
            input_tokens: v
                .get("prompt_eval_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32,
            output_tokens: v.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as u32,
            ..Default::default()
        };
        Ok(ChatReply {
            text,
            model,
            backend: Some("ollama".into()),
            requested_route: None,
            actual_provider: None,
            stop_reason: v
                .get("done_reason")
                .and_then(Value::as_str)
                .map(String::from),
            usage: Some(usage),
        })
    }
}
