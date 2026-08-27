//! Typed, environment-driven configuration for Jarvis services.
//!
//! Values are loaded from an optional `jarvis.toml` and then overridden by
//! `JARVIS_`-prefixed environment variables. Secrets (e.g. database passwords) are
//! redacted from the `Debug` output so they never leak into logs.

// `figment::Error` is a large third-party error type that we surface directly
// from `load()` for ergonomics; boxing it would break `?` in anyhow callers.
#![allow(clippy::result_large_err)]

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
};

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use ipnet::IpNet;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Restricted first-device enrollment configuration. The raw secret is never
/// retained here: Core only receives a SHA-256 verifier from its restricted
/// systemd environment file.
#[derive(Clone)]
pub struct BootstrapEnrollment {
    secret_hash: [u8; 32],
    allowed_cidrs: Vec<IpNet>,
}

impl BootstrapEnrollment {
    pub fn allows(&self, ip: IpAddr) -> bool {
        self.allowed_cidrs.iter().any(|cidr| cidr.contains(&ip))
    }

    pub fn verifies(&self, supplied: &str) -> bool {
        let actual = Sha256::digest(supplied.as_bytes());
        actual.as_slice().ct_eq(&self.secret_hash).into()
    }
}

/// Top-level application configuration.
#[derive(Clone, Deserialize)]
pub struct AppConfig {
    /// `host:port` the HTTP server binds to.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    /// Private SurrealDB websocket endpoint. Never exposed publicly.
    pub surreal_endpoint: String,
    /// SurrealDB namespace and database selected by Core.
    #[serde(default = "default_surreal_namespace")]
    pub surreal_namespace: String,
    #[serde(default = "default_surreal_database")]
    pub surreal_database: String,
    /// Database-scoped Core principal. The root account is provisioning-only.
    pub surreal_username: String,
    /// Secret — redacted in `Debug`.
    pub surreal_password: String,

    /// Emit logs as JSON (recommended for production).
    #[serde(default)]
    pub log_json: bool,

    /// Deployment environment name (e.g. `development`, `production`).
    #[serde(default = "default_environment")]
    pub environment: String,

    /// Public DNS name served by the local HTTPS reverse proxy. This is
    /// deployment metadata for Caddy and must never contain a URL, path, or
    /// credential. Empty keeps a private/local deployment possible.
    #[serde(default)]
    pub public_hostname: String,

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

    /// Root-owned JSON allowlist of exact provider/model pairs.  A credential
    /// alone never enables a remote model.  Missing/empty policy therefore
    /// leaves remote providers safely unavailable.
    #[serde(default = "default_llm_model_policy_path")]
    pub llm_model_policy_path: String,

    /// Path/name of the `claude` CLI, used when `llm_provider = "claude-cli"`
    /// (runs the brain on your Claude subscription; see decisions/ADR-027).
    #[serde(default = "default_claude_cli_bin")]
    pub llm_claude_cli_bin: String,

    /// Path to Jarvis' protected canonical identity/persona doc (`/etc/jarvis/Jarvis.md`), loaded
    /// at startup as the system prompt. Missing/empty ⇒ a built-in fallback
    /// persona is used. This is the single source of truth for "what Jarvis is".
    #[serde(default = "default_llm_persona_path")]
    pub llm_persona_path: String,

    /// OpenAI API key. Secret — redacted in `Debug`. Empty ⇒ OpenAI disabled.
    #[serde(default)]
    pub llm_openai_api_key: String,
    #[serde(default = "default_openai_base_url")]
    pub llm_openai_base_url: String,
    #[serde(default = "default_openai_model")]
    pub llm_openai_model: String,
    #[serde(default = "default_openai_model")]
    pub llm_openai_model_hard: String,
    #[serde(default = "default_openai_model_cheap")]
    pub llm_openai_model_cheap: String,

    /// DeepSeek API key (OpenAI-compatible). Secret. Empty ⇒ DeepSeek disabled.
    #[serde(default)]
    pub llm_deepseek_api_key: String,
    #[serde(default = "default_deepseek_base_url")]
    pub llm_deepseek_base_url: String,
    #[serde(default = "default_deepseek_model")]
    pub llm_deepseek_model: String,
    #[serde(default = "default_deepseek_model_hard")]
    pub llm_deepseek_model_hard: String,
    #[serde(default = "default_deepseek_model")]
    pub llm_deepseek_model_cheap: String,

    /// xAI/Grok API (OpenAI-compatible). Empty key keeps it disabled.
    #[serde(default)]
    pub llm_xai_api_key: String,
    #[serde(default = "default_xai_base_url")]
    pub llm_xai_base_url: String,
    #[serde(default)]
    pub llm_xai_model: String,
    #[serde(default)]
    pub llm_xai_model_hard: String,
    #[serde(default)]
    pub llm_xai_model_cheap: String,

    /// Z.ai/GLM API (OpenAI-compatible only when the owner configures a
    /// supported endpoint/models). Empty key keeps it disabled.
    #[serde(default)]
    pub llm_zai_api_key: String,
    #[serde(default = "default_zai_base_url")]
    pub llm_zai_base_url: String,
    #[serde(default)]
    pub llm_zai_model: String,
    #[serde(default)]
    pub llm_zai_model_hard: String,
    #[serde(default)]
    pub llm_zai_model_cheap: String,

    /// Credentialed remote Ollama API.  Local Ollama remains a separate,
    /// loopback-only provider and needs no API key.
    #[serde(default)]
    pub llm_ollama_cloud_api_key: String,
    #[serde(default)]
    pub llm_ollama_cloud_base_url: String,
    #[serde(default)]
    pub llm_ollama_cloud_model: String,
    #[serde(default)]
    pub llm_ollama_cloud_model_hard: String,
    #[serde(default)]
    pub llm_ollama_cloud_model_cheap: String,

    /// Hard monthly spend cap (EUR) across all metered API backends. Once
    /// reached, the router refuses paid calls and falls back to the plan/Ollama.
    #[serde(default = "default_llm_monthly_budget_eur")]
    pub llm_monthly_budget_eur: f64,

    /// Soft monthly threshold.  Above it routing may prefer a cheaper model
    /// that still satisfies the task quality floor; it never downgrades below
    /// that floor.
    #[serde(default = "default_llm_monthly_soft_budget_eur")]
    pub llm_monthly_soft_budget_eur: f64,

    /// Hard cap for one request/task in EUR.  Zero disables paid requests
    /// rather than meaning unlimited.
    #[serde(default = "default_llm_request_hard_cap_eur")]
    pub llm_request_hard_cap_eur: f64,

    /// EUR per 1 USD, to convert provider (USD) pricing into the EUR budget.
    #[serde(default = "default_eur_per_usd")]
    pub llm_eur_per_usd: f64,

    /// Speech engine (STT + speaker verification): `stub` (default) until a real
    /// model is plugged in.
    #[serde(default = "default_speech_provider")]
    pub speech_provider: String,

    /// Cosine-similarity threshold above which a voice is accepted as the
    /// enrolled speaker. Model-dependent; tune per engine.
    #[serde(default = "default_speech_verify_threshold")]
    pub speech_verify_threshold: f32,

    /// Path to the Whisper GGML model (e.g. `models/ggml-base.bin`), used when
    /// `speech_provider = "whisper"`. Fetch via `scripts/fetch-whisper-model.sh`.
    #[serde(default)]
    pub speech_whisper_model: Option<String>,

    /// Whisper decode language: an ISO code like `nl`, or `auto` to detect.
    #[serde(default = "default_speech_whisper_language")]
    pub speech_whisper_language: String,

    /// Agentic execution kill switch (ADR-029). Default **false** — Jarvis has no
    /// hands until the owner deliberately enables it.
    #[serde(default)]
    pub agent_enabled: bool,

    /// Sandbox root for agentic actions. Empty ⇒ no workspace (actions refused).
    /// All file access is confined to this directory.
    #[serde(default)]
    pub agent_workspace_root: String,

    /// Second, deliberate opt-in for the Claude Code executor (ADR-029 fase 4c):
    /// letting Jarvis drive headless `claude` to *edit* files. Default **false** —
    /// even with the agent enabled, this stays off until switched on on purpose.
    #[serde(default)]
    pub agent_claude_code_enabled: bool,

    /// The `claude` binary used as the confined code-executor (4c).
    #[serde(default = "default_claude_code_bin")]
    pub agent_claude_code_bin: String,

    /// Model for the Claude Code executor. Empty ⇒ let `claude` pick its default.
    #[serde(default)]
    pub agent_claude_code_model: String,

    // --- Auth rate limiting (security hardening, Priority 2). Per client IP. ---
    /// Max device enrolments per minute.
    #[serde(default = "default_auth_rate_enroll")]
    pub auth_rate_enroll_per_min: u32,
    /// Max login challenges per minute.
    #[serde(default = "default_auth_rate_challenge")]
    pub auth_rate_challenge_per_min: u32,
    /// Max login attempts per minute.
    #[serde(default = "default_auth_rate_login")]
    pub auth_rate_login_per_min: u32,
    /// Failed logins that trip the lockout within the lockout window.
    #[serde(default = "default_auth_login_max_failures")]
    pub auth_login_max_failures: u32,
    /// Lockout window (seconds) for repeated failed logins.
    #[serde(default = "default_auth_login_lock_secs")]
    pub auth_login_lock_secs: u64,
    /// Max authenticated requests per device per minute.
    #[serde(default = "default_authenticated_rate")]
    pub authenticated_rate_per_min: u32,
    /// Max LLM/chat requests per authenticated device per minute.
    #[serde(default = "default_llm_rate")]
    pub llm_rate_per_min: u32,

    /// Trusted proxy hops in front of the API. 0 (default) ⇒ never trust
    /// `X-Forwarded-For`; use the socket peer address for rate-limit/audit keys.
    /// Set to N only together with `trusted_proxy_ips` when the API is reachable
    /// *only* through N trusted proxies.
    #[serde(default)]
    pub trusted_proxy_hops: u32,

    /// Comma-separated direct peer IP addresses for the proxies allowed to set
    /// forwarding headers. Empty (the default) means no proxy is trusted.
    #[serde(default)]
    pub trusted_proxy_ips: String,

    /// SHA-256 verifier for the one-time first-owner bootstrap secret. Empty
    /// disables production bootstrap; the raw secret belongs only to the local
    /// root-operated provisioning/recovery procedure.
    #[serde(default)]
    pub bootstrap_secret_sha256: String,

    /// Explicit LAN ranges allowed to perform first-owner bootstrap. Empty
    /// disables bootstrap; public clients are never allowed implicitly.
    #[serde(default)]
    pub bootstrap_allowed_cidrs: String,
}

fn default_bind_addr() -> String {
    "127.0.0.1:8080".to_string()
}

fn default_surreal_namespace() -> String {
    "jarvis".to_string()
}

fn default_surreal_database() -> String {
    "core".to_string()
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

fn default_llm_model_policy_path() -> String {
    "/etc/jarvis/model-policy.json".to_string()
}

fn default_claude_cli_bin() -> String {
    "claude".to_string()
}

fn default_llm_persona_path() -> String {
    "/etc/jarvis/Jarvis.md".to_string()
}

fn default_auth_rate_enroll() -> u32 {
    10
}

fn default_auth_rate_challenge() -> u32 {
    30
}

fn default_auth_rate_login() -> u32 {
    20
}

fn default_auth_login_max_failures() -> u32 {
    5
}

fn default_auth_login_lock_secs() -> u64 {
    300
}

fn default_authenticated_rate() -> u32 {
    300
}

fn default_llm_rate() -> u32 {
    20
}

fn default_claude_code_bin() -> String {
    "claude".to_string()
}

fn default_openai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_openai_model() -> String {
    "gpt-4o".to_string()
}

fn default_openai_model_cheap() -> String {
    "gpt-4o-mini".to_string()
}

fn default_deepseek_base_url() -> String {
    "https://api.deepseek.com/v1".to_string()
}

fn default_deepseek_model() -> String {
    "deepseek-chat".to_string()
}

fn default_deepseek_model_hard() -> String {
    "deepseek-reasoner".to_string()
}

fn default_xai_base_url() -> String {
    "https://api.x.ai/v1".to_string()
}

fn default_zai_base_url() -> String {
    "https://api.z.ai/api/paas/v4".to_string()
}

fn default_llm_monthly_budget_eur() -> f64 {
    50.0
}

fn default_llm_monthly_soft_budget_eur() -> f64 {
    40.0
}

fn default_llm_request_hard_cap_eur() -> f64 {
    5.0
}

fn default_eur_per_usd() -> f64 {
    0.92
}

/// `<none>` for an empty secret, `<redacted>` otherwise — never the value.
fn redact(secret: &str) -> &'static str {
    if secret.trim().is_empty() {
        "<none>"
    } else {
        "<redacted>"
    }
}

fn default_speech_provider() -> String {
    "stub".to_string()
}

fn default_speech_verify_threshold() -> f32 {
    0.5
}

fn default_speech_whisper_language() -> String {
    "auto".to_string()
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

    /// Parse the direct proxy peers allowed to supply forwarding headers.
    /// Configuration is deliberately strict: enabling proxy hops without an
    /// explicit peer allowlist fails startup rather than trusting arbitrary
    /// network clients.
    pub fn trusted_proxy_ips(&self) -> Result<Vec<IpAddr>, String> {
        let peers = self
            .trusted_proxy_ips
            .split(',')
            .map(str::trim)
            .filter(|peer| !peer.is_empty())
            .map(|peer| {
                peer.parse()
                    .map_err(|_| format!("invalid trusted proxy IP: {peer}"))
            })
            .collect::<Result<Vec<IpAddr>, _>>()?;
        if self.trusted_proxy_hops > 0 && peers.is_empty() {
            return Err("JARVIS_TRUSTED_PROXY_HOPS requires JARVIS_TRUSTED_PROXY_IPS".to_string());
        }
        Ok(peers)
    }

    /// Parse the opt-in LAN-only bootstrap policy. Supplying only one half is
    /// a configuration error; an absent policy intentionally means that no
    /// first-device bootstrap endpoint is available.
    pub fn bootstrap_enrollment(&self) -> Result<Option<BootstrapEnrollment>, String> {
        let secret = self.bootstrap_secret_sha256.trim();
        let ranges = self.bootstrap_allowed_cidrs.trim();
        if secret.is_empty() && ranges.is_empty() {
            return Ok(None);
        }
        if secret.is_empty() || ranges.is_empty() {
            return Err("JARVIS_BOOTSTRAP_SECRET_SHA256 and JARVIS_BOOTSTRAP_ALLOWED_CIDRS must be configured together".to_string());
        }
        let bytes = hex::decode(secret).map_err(|_| {
            "JARVIS_BOOTSTRAP_SECRET_SHA256 must be 32-byte hex SHA-256".to_string()
        })?;
        let secret_hash: [u8; 32] = bytes.try_into().map_err(|_| {
            "JARVIS_BOOTSTRAP_SECRET_SHA256 must be 32-byte hex SHA-256".to_string()
        })?;
        let allowed_cidrs = ranges
            .split(',')
            .map(str::trim)
            .filter(|cidr| !cidr.is_empty())
            .map(|cidr| {
                cidr.parse()
                    .map_err(|_| format!("invalid bootstrap CIDR: {cidr}"))
            })
            .collect::<Result<Vec<IpNet>, _>>()?;
        if allowed_cidrs.is_empty() {
            return Err("JARVIS_BOOTSTRAP_ALLOWED_CIDRS must not be empty".to_string());
        }
        Ok(Some(BootstrapEnrollment {
            secret_hash,
            allowed_cidrs,
        }))
    }

    /// Validate deployment invariants that must fail closed before the server
    /// opens a socket. Public TLS terminates at Caddy; production Core is never
    /// allowed to become a directly reachable HTTP listener.
    pub fn validate_runtime_security(&self) -> Result<(), String> {
        if self.environment.eq_ignore_ascii_case("production") {
            let bind_addr: SocketAddr = self
                .bind_addr
                .parse()
                .map_err(|_| "JARVIS_BIND_ADDR must be an IP socket address in production")?;
            if !bind_addr.ip().is_loopback() {
                return Err(
                    "production JARVIS_BIND_ADDR must use a loopback address; expose HTTPS only through Caddy"
                        .to_string(),
                );
            }
            if self.trusted_proxy_hops > 1 {
                return Err(
                    "production supports only the single directly connected Caddy proxy"
                        .to_string(),
                );
            }
        }

        if !self.public_hostname.is_empty()
            && (!self.public_hostname.is_ascii()
                || self.public_hostname.contains(['/', ':', '@', ' ']))
        {
            return Err("JARVIS_PUBLIC_HOSTNAME must be a bare DNS hostname".to_string());
        }
        Ok(())
    }
}

impl fmt::Debug for AppConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppConfig")
            .field("bind_addr", &self.bind_addr)
            .field("surreal_endpoint", &self.surreal_endpoint)
            .field("surreal_namespace", &self.surreal_namespace)
            .field("surreal_database", &self.surreal_database)
            .field("surreal_username", &self.surreal_username)
            .field("surreal_password", &redact(&self.surreal_password))
            .field("log_json", &self.log_json)
            .field("environment", &self.environment)
            .field("public_hostname", &self.public_hostname)
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
            .field("llm_model_policy_path", &self.llm_model_policy_path)
            .field("llm_claude_cli_bin", &self.llm_claude_cli_bin)
            .field("llm_persona_path", &self.llm_persona_path)
            .field("llm_openai_api_key", &redact(&self.llm_openai_api_key))
            .field("llm_openai_base_url", &self.llm_openai_base_url)
            .field("llm_openai_model", &self.llm_openai_model)
            .field("llm_deepseek_api_key", &redact(&self.llm_deepseek_api_key))
            .field("llm_deepseek_base_url", &self.llm_deepseek_base_url)
            .field("llm_deepseek_model", &self.llm_deepseek_model)
            .field("llm_xai_api_key", &redact(&self.llm_xai_api_key))
            .field("llm_xai_base_url", &self.llm_xai_base_url)
            .field("llm_xai_model", &self.llm_xai_model)
            .field("llm_zai_api_key", &redact(&self.llm_zai_api_key))
            .field("llm_zai_base_url", &self.llm_zai_base_url)
            .field("llm_zai_model", &self.llm_zai_model)
            .field(
                "llm_ollama_cloud_api_key",
                &redact(&self.llm_ollama_cloud_api_key),
            )
            .field("llm_ollama_cloud_base_url", &self.llm_ollama_cloud_base_url)
            .field("llm_ollama_cloud_model", &self.llm_ollama_cloud_model)
            .field("llm_monthly_budget_eur", &self.llm_monthly_budget_eur)
            .field(
                "llm_monthly_soft_budget_eur",
                &self.llm_monthly_soft_budget_eur,
            )
            .field("llm_request_hard_cap_eur", &self.llm_request_hard_cap_eur)
            .field("llm_eur_per_usd", &self.llm_eur_per_usd)
            .field("speech_provider", &self.speech_provider)
            .field("speech_verify_threshold", &self.speech_verify_threshold)
            .field("speech_whisper_model", &self.speech_whisper_model)
            .field("speech_whisper_language", &self.speech_whisper_language)
            .field("agent_enabled", &self.agent_enabled)
            .field("agent_workspace_root", &self.agent_workspace_root)
            .field("agent_claude_code_enabled", &self.agent_claude_code_enabled)
            .field("agent_claude_code_bin", &self.agent_claude_code_bin)
            .field("agent_claude_code_model", &self.agent_claude_code_model)
            .field(
                "authenticated_rate_per_min",
                &self.authenticated_rate_per_min,
            )
            .field("llm_rate_per_min", &self.llm_rate_per_min)
            .field("trusted_proxy_hops", &self.trusted_proxy_hops)
            .field("trusted_proxy_ips", &self.trusted_proxy_ips)
            .field(
                "bootstrap_secret_sha256",
                &redact(&self.bootstrap_secret_sha256),
            )
            .field("bootstrap_allowed_cidrs", &self.bootstrap_allowed_cidrs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_database_password() {
        let cfg = AppConfig {
            bind_addr: "127.0.0.1:8080".to_string(),
            surreal_endpoint: "127.0.0.1:8000".to_string(),
            surreal_namespace: "jarvis".to_string(),
            surreal_database: "core".to_string(),
            surreal_username: "core".to_string(),
            surreal_password: "supersecret".to_string(),
            log_json: false,
            environment: "test".to_string(),
            public_hostname: String::new(),
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
            llm_model_policy_path: "/etc/jarvis/model-policy.json".to_string(),
            llm_claude_cli_bin: "claude".to_string(),
            llm_persona_path: "/etc/jarvis/Jarvis.md".to_string(),
            llm_openai_api_key: "sk-openai-supersecret".to_string(),
            llm_openai_base_url: "https://api.openai.com/v1".to_string(),
            llm_openai_model: "gpt-4o".to_string(),
            llm_openai_model_hard: "gpt-4o".to_string(),
            llm_openai_model_cheap: "gpt-4o-mini".to_string(),
            llm_deepseek_api_key: "sk-deepseek-supersecret".to_string(),
            llm_deepseek_base_url: "https://api.deepseek.com/v1".to_string(),
            llm_deepseek_model: "deepseek-chat".to_string(),
            llm_deepseek_model_hard: "deepseek-reasoner".to_string(),
            llm_deepseek_model_cheap: "deepseek-chat".to_string(),
            llm_xai_api_key: "xai-supersecret".to_string(),
            llm_xai_base_url: "https://api.x.ai/v1".to_string(),
            llm_xai_model: String::new(),
            llm_xai_model_hard: String::new(),
            llm_xai_model_cheap: String::new(),
            llm_zai_api_key: "zai-supersecret".to_string(),
            llm_zai_base_url: "https://api.z.ai/api/paas/v4".to_string(),
            llm_zai_model: String::new(),
            llm_zai_model_hard: String::new(),
            llm_zai_model_cheap: String::new(),
            llm_ollama_cloud_api_key: "ollama-supersecret".to_string(),
            llm_ollama_cloud_base_url: String::new(),
            llm_ollama_cloud_model: String::new(),
            llm_ollama_cloud_model_hard: String::new(),
            llm_ollama_cloud_model_cheap: String::new(),
            llm_monthly_budget_eur: 50.0,
            llm_monthly_soft_budget_eur: 40.0,
            llm_request_hard_cap_eur: 5.0,
            llm_eur_per_usd: 0.92,
            speech_provider: "stub".to_string(),
            speech_verify_threshold: 0.5,
            speech_whisper_model: None,
            speech_whisper_language: "auto".to_string(),
            agent_enabled: false,
            agent_workspace_root: String::new(),
            agent_claude_code_enabled: false,
            agent_claude_code_bin: "claude".to_string(),
            agent_claude_code_model: String::new(),
            auth_rate_enroll_per_min: 10,
            auth_rate_challenge_per_min: 30,
            auth_rate_login_per_min: 20,
            auth_login_max_failures: 5,
            auth_login_lock_secs: 300,
            authenticated_rate_per_min: 300,
            llm_rate_per_min: 20,
            trusted_proxy_hops: 0,
            trusted_proxy_ips: String::new(),
            bootstrap_secret_sha256: String::new(),
            bootstrap_allowed_cidrs: String::new(),
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
            jail.set_env("JARVIS_SURREAL_ENDPOINT", "127.0.0.1:8000");
            jail.set_env("JARVIS_SURREAL_USERNAME", "core");
            jail.set_env("JARVIS_SURREAL_PASSWORD", "test-password");
            jail.set_env("JARVIS_BIND_ADDR", "127.0.0.1:9999");
            jail.set_env("JARVIS_LOG_JSON", "true");

            let cfg = AppConfig::load()?;
            assert_eq!(cfg.bind_addr, "127.0.0.1:9999");
            assert_eq!(cfg.surreal_endpoint, "127.0.0.1:8000");
            assert!(cfg.log_json);
            assert_eq!(cfg.environment, "development"); // default
            Ok(())
        });
    }

    #[test]
    fn bootstrap_policy_requires_both_inputs_and_limits_to_configured_lan() {
        let mut cfg = AppConfig {
            bind_addr: "127.0.0.1:8080".to_string(),
            surreal_endpoint: "127.0.0.1:8000".to_string(),
            surreal_namespace: "jarvis".to_string(),
            surreal_database: "core".to_string(),
            surreal_username: "core".to_string(),
            surreal_password: "x".to_string(),
            log_json: false,
            environment: "production".to_string(),
            public_hostname: String::new(),
            ibkr_gateway_url: String::new(),
            llm_provider: String::new(),
            llm_api_key: String::new(),
            llm_anthropic_base_url: String::new(),
            llm_model: String::new(),
            llm_model_hard: String::new(),
            llm_model_cheap: String::new(),
            llm_max_tokens: 1,
            llm_ollama_url: String::new(),
            llm_ollama_model: String::new(),
            llm_model_policy_path: String::new(),
            llm_claude_cli_bin: String::new(),
            llm_persona_path: String::new(),
            llm_openai_api_key: String::new(),
            llm_openai_base_url: String::new(),
            llm_openai_model: String::new(),
            llm_openai_model_hard: String::new(),
            llm_openai_model_cheap: String::new(),
            llm_deepseek_api_key: String::new(),
            llm_deepseek_base_url: String::new(),
            llm_deepseek_model: String::new(),
            llm_deepseek_model_hard: String::new(),
            llm_deepseek_model_cheap: String::new(),
            llm_xai_api_key: String::new(),
            llm_xai_base_url: String::new(),
            llm_xai_model: String::new(),
            llm_xai_model_hard: String::new(),
            llm_xai_model_cheap: String::new(),
            llm_zai_api_key: String::new(),
            llm_zai_base_url: String::new(),
            llm_zai_model: String::new(),
            llm_zai_model_hard: String::new(),
            llm_zai_model_cheap: String::new(),
            llm_ollama_cloud_api_key: String::new(),
            llm_ollama_cloud_base_url: String::new(),
            llm_ollama_cloud_model: String::new(),
            llm_ollama_cloud_model_hard: String::new(),
            llm_ollama_cloud_model_cheap: String::new(),
            llm_monthly_budget_eur: 0.0,
            llm_monthly_soft_budget_eur: 0.0,
            llm_request_hard_cap_eur: 0.0,
            llm_eur_per_usd: 0.0,
            speech_provider: String::new(),
            speech_verify_threshold: 0.0,
            speech_whisper_model: None,
            speech_whisper_language: String::new(),
            agent_enabled: false,
            agent_workspace_root: String::new(),
            agent_claude_code_enabled: false,
            agent_claude_code_bin: String::new(),
            agent_claude_code_model: String::new(),
            auth_rate_enroll_per_min: 1,
            auth_rate_challenge_per_min: 1,
            auth_rate_login_per_min: 1,
            auth_login_max_failures: 1,
            auth_login_lock_secs: 1,
            authenticated_rate_per_min: 1,
            llm_rate_per_min: 1,
            trusted_proxy_hops: 0,
            trusted_proxy_ips: String::new(),
            bootstrap_secret_sha256: "00".repeat(32),
            bootstrap_allowed_cidrs: String::new(),
        };
        assert!(cfg.bootstrap_enrollment().is_err());
        cfg.bootstrap_allowed_cidrs = "192.168.10.0/24".to_string();
        let policy = cfg.bootstrap_enrollment().unwrap().unwrap();
        assert!(policy.allows("192.168.10.8".parse().unwrap()));
        assert!(!policy.allows("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn proxy_hops_require_an_explicit_peer_allowlist() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("JARVIS_SURREAL_ENDPOINT", "127.0.0.1:8000");
            jail.set_env("JARVIS_SURREAL_USERNAME", "core");
            jail.set_env("JARVIS_SURREAL_PASSWORD", "test-password");
            jail.set_env("JARVIS_TRUSTED_PROXY_HOPS", "1");
            let cfg = AppConfig::load()?;
            assert!(cfg.trusted_proxy_ips().is_err());

            jail.set_env("JARVIS_TRUSTED_PROXY_IPS", "127.0.0.1,::1");
            let cfg = AppConfig::load()?;
            assert_eq!(cfg.trusted_proxy_ips()?.len(), 2);
            Ok(())
        });
    }

    #[test]
    fn production_requires_a_loopback_listener() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("JARVIS_SURREAL_ENDPOINT", "127.0.0.1:8000");
            jail.set_env("JARVIS_SURREAL_USERNAME", "core");
            jail.set_env("JARVIS_SURREAL_PASSWORD", "test-password");
            jail.set_env("JARVIS_ENVIRONMENT", "production");
            jail.set_env("JARVIS_BIND_ADDR", "0.0.0.0:8080");
            assert!(AppConfig::load()?.validate_runtime_security().is_err());

            jail.set_env("JARVIS_BIND_ADDR", "127.0.0.1:8080");
            assert!(AppConfig::load()?.validate_runtime_security().is_ok());

            jail.set_env("JARVIS_TRUSTED_PROXY_HOPS", "2");
            jail.set_env("JARVIS_TRUSTED_PROXY_IPS", "127.0.0.1");
            assert!(AppConfig::load()?.validate_runtime_security().is_err());
            Ok(())
        });
    }
}
