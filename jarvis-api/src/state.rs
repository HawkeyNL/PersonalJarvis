//! The shared application state. `AppState` is the dependency bundle every
//! handler receives via `State<AppState>` — the DB pool, the brain, the speech
//! engine, the live registry, the budget counters, the agent sandbox, and the
//! auth rate limiter. It is cheaply cloneable (everything shared is behind `Arc`)
//! so Axum can hand a clone to each request.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use jarvis_agent as agent;
use jarvis_llm as llm;
use jarvis_registry as registry;
use jarvis_speech as speech;

use crate::rate_limit;

/// Runtime-only location and public origin for the authenticated application
/// update mirror. Neither value is compiled into a client or release manifest.
#[derive(Clone, Debug)]
pub struct AppUpdateMirror {
    root: Arc<PathBuf>,
    mobile_root: Option<Arc<PathBuf>>,
    public_base_url: Arc<str>,
}

impl AppUpdateMirror {
    pub fn new(root: impl Into<PathBuf>, public_base_url: &str) -> Result<Self, &'static str> {
        let root = root.into();
        if !root.is_absolute()
            || root == Path::new("/")
            || root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("application update mirror root must be a bounded absolute path");
        }
        let parsed = url::Url::parse(public_base_url)
            .map_err(|_| "application update public base URL is invalid")?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("application update public base URL must be credential-free HTTPS");
        }
        Ok(Self {
            root: Arc::new(root),
            mobile_root: None,
            public_base_url: Arc::from(public_base_url.trim_end_matches('/')),
        })
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// Keep an existing private mobile generation independent of desktop sync.
    /// Without this opt-in, legacy mixed-platform mirrors retain their behavior.
    pub fn with_mobile_root(mut self, root: impl Into<PathBuf>) -> Result<Self, &'static str> {
        let root = root.into();
        if !root.is_absolute()
            || root == Path::new("/")
            || root
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || root.starts_with(self.root())
            || self.root().starts_with(&root)
        {
            return Err("mobile mirror must be a separate bounded absolute path");
        }
        self.mobile_root = Some(Arc::new(root));
        Ok(self)
    }

    pub fn for_mobile(&self) -> Self {
        Self {
            root: self
                .mobile_root
                .clone()
                .unwrap_or_else(|| self.root.clone()),
            mobile_root: None,
            public_base_url: self.public_base_url.clone(),
        }
    }

    pub fn public_base_url(&self) -> &str {
        &self.public_base_url
    }
}

/// Tests for runtime-only independent mirror configuration.
#[cfg(test)]
mod update_mirror_tests {
    use super::AppUpdateMirror;
    use std::path::Path;

    #[test]
    fn production_update_origin_and_roots_are_bounded() {
        for origin in [
            "http://jarvis.example.com",
            "https://user:pass@jarvis.example.com",
            "https://jarvis.example.com/path",
            "https://jarvis.example.com?query=1",
            "https://jarvis.example.com/#fragment",
        ] {
            assert!(AppUpdateMirror::new("/var/lib/jarvis/app-updates", origin).is_err());
        }
        for root in ["relative", "/", "/var/lib/../outside"] {
            assert!(AppUpdateMirror::new(root, "https://jarvis.example.com").is_err());
        }
    }

    #[test]
    fn independent_mobile_root_preserves_origin_and_legacy_fallback() {
        let legacy =
            AppUpdateMirror::new("/var/lib/jarvis/app-updates", "https://jarvis.example.com")
                .unwrap();
        assert_eq!(legacy.for_mobile().root(), legacy.root());
        let separate = legacy
            .clone()
            .with_mobile_root("/var/lib/jarvis/mobile-updates")
            .unwrap();
        assert_eq!(separate.root(), legacy.root());
        assert_eq!(
            separate.for_mobile().root(),
            Path::new("/var/lib/jarvis/mobile-updates")
        );
        assert_eq!(
            separate.for_mobile().public_base_url(),
            legacy.public_base_url()
        );
        for unsafe_root in [
            "/",
            "relative",
            "/var/lib/jarvis/app-updates",
            "/var/lib/jarvis/app-updates/nested",
            "/var/lib/jarvis",
            "/var/lib/../elsewhere",
        ] {
            assert!(legacy.clone().with_mobile_root(unsafe_root).is_err());
        }
    }
}

/// Shared, cheaply-cloneable application state.
#[derive(Clone)]
pub struct AppState {
    pub db: jarvis_store::Database,
    pub environment: String,
    /// Production requests carrying bearer credentials must have arrived over
    /// HTTPS through an explicitly trusted local reverse proxy.
    pub require_https: bool,
    pub ibkr_gateway_url: String,
    /// The brain (DEC-001) — provider-abstracted, swappable at runtime.
    pub llm: Arc<dyn llm::LlmProvider>,
    /// Max output tokens per assistant reply.
    pub llm_max_tokens: u32,
    /// Jarvis' protected identity/persona (from `/etc/jarvis/Jarvis.md`), prepended as the system
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
    /// Immutable snapshot loaded from the root-owned allowlist at startup.
    /// Changes are activated by the root-operated command/restart flow.
    pub model_policy: Arc<llm::ModelAccessPolicy>,
    /// Root-owned/versioned provider pricing. Missing or malformed deployment
    /// input is replaced with conservative built-in pricing at startup.
    pub pricing_registry: Arc<jarvis_usage::PricingRegistry>,
    /// Optional non-secret aggregate snapshot consumed by the local root-only
    /// administration boundary. It never contains prompts, responses or IDs.
    pub usage_snapshot_path: Option<Arc<PathBuf>>,
    /// Optional local root-broker socket. This is not a credential and a
    /// request is still independently signature-verified by the broker.
    pub privileged_broker_socket: Option<Arc<str>>,
    /// Optional local Codex broker. A missing socket disables coding runs;
    /// it never enables direct host execution.
    pub codex_broker_socket: Option<Arc<str>>,
    /// Hard monthly spend cap in EUR-cents across metered API backends (ADR-027).
    pub budget_cents: u64,
    /// Metered spend so far this month, in EUR-cents. Mirrors the DB (refreshed
    /// after each call) so the router's sync budget gate can read it cheaply.
    pub spent_cents: Arc<AtomicU64>,
    /// Atomic request/task reservations.  Reservations are separate from
    /// durable usage rows so concurrent work cannot oversubscribe the monthly
    /// hard cap before post-call metering catches up.
    pub budget_book: Arc<jarvis_usage::BudgetBook>,
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
    /// LAN-only, one-time first-owner bootstrap verifier. `None` means no
    /// bootstrap route is usable; it is intentionally not a general secret bag.
    pub bootstrap_enrollment: Option<jarvis_config::BootstrapEnrollment>,
    /// Disabled unless the root-operated mirror and its private HTTPS origin
    /// are explicitly configured together.
    pub app_update_mirror: Option<AppUpdateMirror>,
}
