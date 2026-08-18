//! The shared application state. `AppState` is the dependency bundle every
//! handler receives via `State<AppState>` — the DB pool, the brain, the speech
//! engine, the live registry, the budget counters, the agent sandbox, and the
//! auth rate limiter. It is cheaply cloneable (everything shared is behind `Arc`)
//! so Axum can hand a clone to each request.

use std::net::IpAddr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use jarvis_agent as agent;
use jarvis_llm as llm;
use jarvis_registry as registry;
use jarvis_speech as speech;

use crate::rate_limit;

/// Shared, cheaply-cloneable application state.
#[derive(Clone)]
pub struct AppState {
    pub db: jarvis_store::Database,
    pub environment: String,
    pub ibkr_gateway_url: String,
    /// The brain (DEC-001) — provider-abstracted, swappable at runtime.
    pub llm: Arc<dyn llm::LlmProvider>,
    /// Max output tokens per assistant reply.
    pub llm_max_tokens: u32,
    /// Jarvis' identity/persona (from `core/Jarvis.md`), prepended as the system
    /// prompt on every chat. The single source of truth for "what Jarvis is".
    pub jarvis_system: Arc<str>,
    /// Server-side speech engine (STT + speaker verification).
    pub speech: Arc<dyn speech::SpeechEngine>,
    /// Cosine threshold to accept a voice as the enrolled speaker.
    pub speech_verify_threshold: f32,
    /// Resource/agent registry — Jarvis' "instant memory" (ADR-027 stage 3).
    pub registry: Arc<RwLock<registry::Registry>>,
    /// Inputs to re-collect the registry on refresh.
    pub registry_input: Arc<registry::CollectInput>,
    /// Hard monthly spend cap in EUR-cents across metered API backends (ADR-027).
    pub budget_cents: u64,
    /// Metered spend so far this month, in EUR-cents. Mirrors the DB (refreshed
    /// after each call) so the router's sync budget gate can read it cheaply.
    pub spent_cents: Arc<AtomicU64>,
    /// EUR per 1 USD, to price provider (USD) usage into the EUR budget.
    pub eur_per_usd: f64,
    /// Agentic execution kill switch (ADR-029) — Jarvis has no hands unless true.
    pub agent_enabled: bool,
    /// The sandbox Jarvis' read-only actions are confined to. `None` ⇒ no
    /// workspace configured (actions refused even when enabled).
    pub agent_sandbox: Option<Arc<agent::Sandbox>>,
    /// Per-IP rate limiter for auth-sensitive endpoints (enroll/challenge/login).
    pub rate_limiter: Arc<rate_limit::RateLimiter>,
    /// Tunable thresholds for the auth rate limiter (from `JARVIS_AUTH_*`).
    pub auth_limits: rate_limit::AuthLimits,
    /// Number of trusted proxy hops in front of the API. Forwarding headers are
    /// only considered when the direct peer is in `trusted_proxy_ips`.
    pub trusted_proxy_hops: u32,
    /// Direct peer IPs of the proxies allowed to supply forwarding headers.
    pub trusted_proxy_ips: Arc<Vec<IpAddr>>,
}
