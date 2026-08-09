//! Typed, environment-driven configuration for Jarvis services.
//!
//! Values are loaded from an optional `jarvis.toml` and then overridden by
//! `JARVIS_`-prefixed environment variables. Secrets (e.g. `database_url`) are
//! redacted from the `Debug` output so they never leak into logs.

// `figment::Error` is a large third-party error type that we surface directly
// from `load()` for ergonomics; boxing it would break `?` in anyhow callers.
#![allow(clippy::result_large_err)]

use std::fmt;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::Deserialize;

/// Top-level application configuration.
#[derive(Clone, Deserialize)]
pub struct AppConfig {
    /// `host:port` the HTTP server binds to.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// PostgreSQL connection string. Secret — redacted in `Debug`.
    pub database_url: String,

    /// Emit logs as JSON (recommended for production).
    #[serde(default)]
    pub log_json: bool,

    /// Deployment environment name (e.g. `development`, `production`).
    #[serde(default = "default_environment")]
    pub environment: String,

    /// Base URL of the IBKR Client Portal Gateway (read-only proxy target).
    #[serde(default = "default_ibkr_gateway_url")]
    pub ibkr_gateway_url: String,

    /// LLM provider for the brain: `anthropic` (default) or `ollama`.
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,

    /// Anthropic API key. Secret — redacted in `Debug`; never sent to the
    /// client. Empty ⇒ Anthropic disabled and the local Ollama brain is used.
    #[serde(default)]
    pub llm_api_key: String,

    /// Anthropic API base URL (override for a proxy or tests).
    #[serde(default = "default_anthropic_base_url")]
    pub llm_anthropic_base_url: String,

    /// Default "brain" model (balanced).
    #[serde(default = "default_llm_model")]
    pub llm_model: String,

    /// Model for hard reasoning.
    #[serde(default = "default_llm_model_hard")]
    pub llm_model_hard: String,

    /// Model for cheap/fast tasks.
    #[serde(default = "default_llm_model_cheap")]
    pub llm_model_cheap: String,

    /// Max output tokens per reply.
    #[serde(default = "default_llm_max_tokens")]
    pub llm_max_tokens: u32,

    /// Base URL of the local Ollama server (fallback brain).
    #[serde(default = "default_ollama_url")]
    pub llm_ollama_url: String,

    /// Ollama model name (fallback brain).
    #[serde(default = "default_ollama_model")]
    pub llm_ollama_model: String,

    /// Speech engine (STT + speaker verification): `stub` (default) until a real
    /// model is plugged in.
    #[serde(default = "default_speech_provider")]
    pub speech_provider: String,

    /// Cosine-similarity threshold above which a voice is accepted as the
    /// enrolled speaker. Model-dependent; tune per engine.
    #[serde(default = "default_speech_verify_threshold")]
    pub speech_verify_threshold: f32,
}

fn default_bind_addr() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_environment() -> String {
    "development".to_string()
}

fn default_ibkr_gateway_url() -> String {
    "https://localhost:5000/v1/api".to_string()
}

fn default_llm_provider() -> String {
    "anthropic".to_string()
}

fn default_anthropic_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

// DEC-001: Claude as the brain. Sonnet balanced, Opus for hard reasoning,
// Haiku for cheap/fast tasks.
fn default_llm_model() -> String {
    "claude-sonnet-5".to_string()
}

fn default_llm_model_hard() -> String {
    "claude-opus-5".to_string()
}

fn default_llm_model_cheap() -> String {
    "claude-haiku-4-5".to_string()
}

fn default_llm_max_tokens() -> u32 {
    1024
}

fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_ollama_model() -> String {
    "llama3.2".to_string()
}

fn default_speech_provider() -> String {
    "stub".to_string()
}

fn default_speech_verify_threshold() -> f32 {
    0.5
}

impl AppConfig {
    /// Load configuration from `jarvis.toml` (optional) and `JARVIS_` env vars.
    ///
    /// Environment variables take precedence over the file.
    pub fn load() -> Result<Self, figment::Error> {
        Figment::new()
            .merge(Toml::file("jarvis.toml"))
            .merge(Env::prefixed("JARVIS_"))
            .extract()
    }
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("log_json", &self.log_json)
            .field("environment", &self.environment)
            .field("ibkr_gateway_url", &self.ibkr_gateway_url)
            .field("llm_provider", &self.llm_provider)
            .field(
                "llm_api_key",
                &if self.llm_api_key.is_empty() {
                    "<none>"
                } else {
                    "<redacted>"
                },
            )
            .field("llm_anthropic_base_url", &self.llm_anthropic_base_url)
            .field("llm_model", &self.llm_model)
            .field("llm_model_hard", &self.llm_model_hard)
            .field("llm_model_cheap", &self.llm_model_cheap)
            .field("llm_max_tokens", &self.llm_max_tokens)
            .field("llm_ollama_url", &self.llm_ollama_url)
            .field("llm_ollama_model", &self.llm_ollama_model)
            .field("speech_provider", &self.speech_provider)
            .field("speech_verify_threshold", &self.speech_verify_threshold)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_database_url() {
        let cfg = AppConfig {
            bind_addr: "0.0.0.0:8080".to_string(),
            database_url: "postgres://user:supersecret@localhost/jarvis".to_string(),
            log_json: false,
            environment: "test".to_string(),
            ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
            llm_provider: "anthropic".to_string(),
            llm_api_key: "sk-ant-supersecretkey".to_string(),
            llm_anthropic_base_url: "https://api.anthropic.com".to_string(),
            llm_model: "claude-sonnet-5".to_string(),
            llm_model_hard: "claude-opus-5".to_string(),
            llm_model_cheap: "claude-haiku-4-5".to_string(),
            llm_max_tokens: 1024,
            llm_ollama_url: "http://localhost:11434".to_string(),
            llm_ollama_model: "llama3.2".to_string(),
            speech_provider: "stub".to_string(),
            speech_verify_threshold: 0.5,
        };

        let rendered = format!("{cfg:?}");

        assert!(
            !rendered.contains("supersecret"),
            "secret leaked into Debug output: {rendered}"
        );
        assert!(
            !rendered.contains("sk-ant-supersecretkey"),
            "llm_api_key secret leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn env_overrides_are_applied() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("JARVIS_DATABASE_URL", "postgres://localhost/x");
            jail.set_env("JARVIS_BIND_ADDR", "127.0.0.1:9999");
            jail.set_env("JARVIS_LOG_JSON", "true");

            let cfg = AppConfig::load()?;
            assert_eq!(cfg.bind_addr, "127.0.0.1:9999");
            assert_eq!(cfg.database_url, "postgres://localhost/x");
            assert!(cfg.log_json);
            assert_eq!(cfg.environment, "development"); // default
            Ok(())
        });
    }
}
