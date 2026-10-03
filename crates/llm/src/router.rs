//! Cost-aware router that consults live availability (ADR-027).
//!
//! Per request it orders the backends by a *sensible* policy — cheapest for
//! trivial tasks, quality-first for real work, strong-only for the hardest — and
//! tries them in order, skipping ones the registry marks unavailable (with a
//! safety net: if the registry says none are up, try the unmetered ones anyway).
//! Falls through on any error except a genuine refusal.

use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;

use crate::types::{ChatReply, ChatRequest, LlmError, ProviderFailure, Tier};
#[cfg(test)]
use crate::ModelAccessPolicy;
use crate::{LiveModelPolicy, LiveRouting, LlmProvider};

/// Live availability of a backend, by id — implemented by the api over the
/// resource registry (`jarvis-registry`), so the router routes on what's up now.
pub trait Availability: Send + Sync {
    fn is_available(&self, backend_id: &str) -> bool;
}

struct AlwaysAvailable;
impl Availability for AlwaysAvailable {
    fn is_available(&self, _id: &str) -> bool {
        true
    }
}

/// An availability source that considers every backend up (no registry).
pub fn always_available() -> Arc<dyn Availability> {
    Arc::new(AlwaysAvailable)
}

/// Whether the configured Ollama endpoint is off this host. Set once when the
/// Ollama brain is built; a remote Ollama may bill and is treated as metered.
static OLLAMA_REMOTE: AtomicBool = AtomicBool::new(false);

/// Record the configured Ollama URL. Only a loopback host (127.0.0.0/8, ::1,
/// `localhost`) keeps `ollama` unmetered; anything else, including an
/// unparsable URL, fails closed to metered.
pub(crate) fn record_ollama_url(url: &str) {
    OLLAMA_REMOTE.store(!is_loopback_url(url), Ordering::Relaxed);
}

fn is_loopback_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Static classification by provider id, independent of runtime config, so
/// the broker, the CLI and Core validate `routing.json` identically.
pub(crate) fn is_metered_provider_id(backend_id: &str) -> bool {
    !matches!(backend_id, "ollama" | "claude-cli")
}

/// Whether a backend bills per call. Fails closed: only the known local and
/// subscription backends are unmetered, and Ollama only on loopback; anything
/// else is treated as paid.
pub fn is_metered_backend(backend_id: &str) -> bool {
    is_metered_provider_id(backend_id)
        || (backend_id == "ollama" && OLLAMA_REMOTE.load(Ordering::Relaxed))
}

/// What a model is good for — mirrors `jarvis_registry::ModelClass` so the router
/// can pick per task without depending on the registry crate (ADR-028 fase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelClass {
    Light,
    Mid,
    Heavy,
    Reasoning,
}

/// One model the router may pick, mapped from the registry catalog (available only).
#[derive(Debug, Clone)]
pub struct CatalogModel {
    pub backend: String,
    pub id: String,
    pub class: ModelClass,
}

/// A backend the router can route to.
pub(crate) struct Candidate {
    pub(crate) id: String,
    pub(crate) provider: Arc<dyn LlmProvider>,
}

/// Short, bounded retry suppression for a failing backend. This is deliberately
/// process-local: the resource registry remains the durable availability view,
/// while the router avoids paying/retrying the same known-bad provider on every
/// request. It stores only a backend id, safe category and a monotonic deadline.
#[derive(Debug, Clone, Copy)]
struct HealthCooldown {
    until: Instant,
}

pub struct RouterProvider {
    candidates: Vec<Candidate>,
    availability: Arc<dyn Availability>,
    /// Available models per backend, cheapest-first within a class (ADR-028).
    catalog: Vec<CatalogModel>,
    /// Root-owned, explicit allowlist.  Provider credentials do not imply
    /// permission to route to a model.
    model_policy: Arc<LiveModelPolicy>,
    /// Owner-defined per-tier order and `paid_api` switch (`routing.json`).
    routing: Arc<LiveRouting>,
    health: Mutex<BTreeMap<String, HealthCooldown>>,
    label: String,
}

impl RouterProvider {
    #[cfg(test)]
    pub(crate) fn new(
        candidates: Vec<Candidate>,
        availability: Arc<dyn Availability>,
        catalog: Vec<CatalogModel>,
    ) -> Self {
        Self::with_policy(
            candidates,
            availability,
            catalog,
            ModelAccessPolicy::deny_by_default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_policy(
        candidates: Vec<Candidate>,
        availability: Arc<dyn Availability>,
        catalog: Vec<CatalogModel>,
        model_policy: ModelAccessPolicy,
    ) -> Self {
        Self::with_live_policy(
            candidates,
            availability,
            catalog,
            Arc::new(LiveModelPolicy::new(model_policy)),
        )
    }

    pub(crate) fn with_live_policy(
        candidates: Vec<Candidate>,
        availability: Arc<dyn Availability>,
        catalog: Vec<CatalogModel>,
        model_policy: Arc<LiveModelPolicy>,
    ) -> Self {
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let label = format!("router[{}]", ids.join(","));
        Self {
            candidates,
            availability,
            catalog,
            model_policy,
            routing: Arc::default(),
            health: Mutex::new(BTreeMap::new()),
            label,
        }
    }

    pub(crate) fn with_routing(mut self, routing: Arc<LiveRouting>) -> Self {
        self.routing = routing;
        self
    }

    /// Model classes acceptable for a tier, best-preferred first — "low models
    /// almost always" (ADR-028): everyday work stays light, only Hard reaches for
    /// the strong brains, with a sensible escalation fallback.
    fn target_classes(tier: Tier) -> &'static [ModelClass] {
        match tier {
            Tier::Cheap => &[ModelClass::Light],
            Tier::Default => &[ModelClass::Light, ModelClass::Mid],
            Tier::Hard => &[ModelClass::Heavy, ModelClass::Reasoning, ModelClass::Mid],
        }
    }

    /// Pick the best model for `backend` at `tier` from the catalog: the first
    /// catalog entry (catalog is cheapest-first within a class) whose class is
    /// the most-preferred available for this tier. `None` ⇒ let the provider use
    /// its own tier model.
    fn model_for(&self, backend: &str, tier: Tier) -> Option<String> {
        for want in Self::target_classes(tier) {
            if let Some(m) = self.catalog.iter().find(|m| {
                m.backend == backend
                    && m.class == *want
                    && self.model_policy.allows(&m.backend, &m.id)
            }) {
                return Some(m.id.clone());
            }
            // Root-verified discovery can add disabled cloud models after
            // startup. Once explicitly enabled, treat new entries as Mid, just
            // like registry discovery. Never infer a cheap/free tier or replace
            // a configured model's classification. Candidates and availability
            // still enforce credentialed, configured backends and budgets.
            if *want == ModelClass::Mid && Self::dynamic_cloud_backend(backend) {
                if let Some(entry) = self.model_policy.snapshot().models.iter().find(|entry| {
                    entry.provider == backend
                        && entry.enabled
                        && !self
                            .catalog
                            .iter()
                            .any(|m| m.backend == backend && m.id == entry.model)
                }) {
                    return Some(entry.model.clone());
                }
            }
        }
        None
    }

    /// Built-in preference order of backend ids for a tier, used when the
    /// owner's routing has no chain for it. Cheap → cheapest first (free
    /// local/plan, then the cheapest metered); Default → the plan first for
    /// quality, strong APIs as the vangnet, cheap/local last; Hard → strong
    /// brains only. Ids not built (no key) are simply skipped.
    fn policy(tier: Tier) -> &'static [&'static str] {
        match tier {
            Tier::Cheap => &[
                "ollama",
                "claude-cli",
                "deepseek-api",
                "zai-api",
                "openai-api",
                "xai-api",
                "anthropic-api",
                "ollama-cloud",
                "huggingface",
            ],
            Tier::Default => &[
                "claude-cli",
                "anthropic-api",
                "openai-api",
                "deepseek-api",
                "xai-api",
                "zai-api",
                "ollama-cloud",
                "huggingface",
                "ollama",
            ],
            Tier::Hard => &[
                "claude-cli",
                "anthropic-api",
                "openai-api",
                "xai-api",
                "zai-api",
                "ollama-cloud",
                "huggingface",
            ],
        }
    }

    fn cooldown_for(failure: ProviderFailure) -> Duration {
        match failure {
            // Repeated auth failures are both noisy and futile until an owner
            // rotates the credential. Keep the cooldown bounded so a healthy
            // credential reload naturally recovers without a process restart.
            ProviderFailure::Authentication => Duration::from_secs(15 * 60),
            ProviderFailure::RateLimited => Duration::from_secs(60),
            ProviderFailure::Unavailable | ProviderFailure::Transport => Duration::from_secs(30),
            ProviderFailure::ContextOverflow | ProviderFailure::MalformedResponse => {
                Duration::from_secs(15)
            }
            ProviderFailure::NotConfigured => Duration::from_secs(5 * 60),
            // A policy refusal is not a provider-health failure and returns
            // directly from `chat`, so this value is never used in practice.
            ProviderFailure::Refused => Duration::ZERO,
        }
    }

    fn is_healthy(&self, backend: &str) -> bool {
        let now = Instant::now();
        let mut health = self.health.lock().expect("router health mutex poisoned");
        match health.get(backend).copied() {
            Some(cooldown) if cooldown.until > now => false,
            Some(_) => {
                health.remove(backend);
                true
            }
            None => true,
        }
    }

    fn record_failure(&self, backend: &str, failure: ProviderFailure) {
        let cooldown = Self::cooldown_for(failure);
        if cooldown.is_zero() {
            return;
        }
        self.health
            .lock()
            .expect("router health mutex poisoned")
            .insert(
                backend.to_string(),
                HealthCooldown {
                    until: Instant::now() + cooldown,
                },
            );
    }

    fn record_success(&self, backend: &str) {
        self.health
            .lock()
            .expect("router health mutex poisoned")
            .remove(backend);
    }

    /// The backends to try, in order, each with the model the owner pinned for
    /// it (routed chains only): the owner's routed chain for the tier, else the
    /// built-in order, ∩ existing candidates, without metered backends when
    /// paid APIs are off, preferring registry-available ones. An explicitly
    /// requested model (`routed == false`) is more specific than a tier chain
    /// and uses the built-in order. A recent health cooldown is never
    /// overridden by the old "try everything" safety net: that would turn a
    /// revoked credential or 429 into an unbounded request-by-request retry.
    /// The safety net never includes metered backends: the monthly cap is
    /// enforced through availability, so "nothing available" must not become
    /// a paid call. A pinned provider (`only`) is the whole plan: a pin never
    /// falls back to another provider.
    fn plan(
        &self,
        tier: Tier,
        routed: bool,
        only: Option<&str>,
    ) -> Vec<(&Candidate, Option<String>)> {
        let routing = self.routing.snapshot();
        let find = |id: &str| self.candidates.iter().find(|c| c.id == id);
        let ordered: Vec<(&Candidate, Option<String>)> =
            match (only, routing.tier(tier).filter(|_| routed)) {
                (Some(provider), _) => find(provider).map(|c| (c, None)).into_iter().collect(),
                (None, Some(route)) => route
                    .chain
                    .iter()
                    .filter_map(|entry| {
                        find(&entry.provider).map(|c| (c, Some(entry.model.clone())))
                    })
                    .collect(),
                (None, None) => Self::policy(tier)
                    .iter()
                    .filter_map(|id| find(id).map(|c| (c, None)))
                    .collect(),
            };
        let ordered: Vec<_> = ordered
            .into_iter()
            .filter(|(c, _)| !routing.refuses_provider(&c.id))
            .collect();
        let available: Vec<_> = ordered
            .iter()
            .filter(|(c, _)| self.availability.is_available(&c.id))
            .cloned()
            .collect();
        let base = if available.is_empty() {
            ordered
                .into_iter()
                .filter(|(candidate, _)| !is_metered_backend(&candidate.id))
                .collect()
        } else {
            available
        };
        base.into_iter()
            .filter(|(candidate, _)| self.is_healthy(&candidate.id))
            .collect()
    }

    /// The model to send to `backend`, or `None` to skip it. Every choice is
    /// checked against the live owner allowlist at attempt time.
    fn choose_model(
        &self,
        backend: &str,
        pinned: Option<&str>,
        requested: Option<&str>,
        tier: Tier,
    ) -> Option<String> {
        match (requested, pinned) {
            (Some(model), _) => self
                .requested_model_is_allowed(backend, model)
                .then(|| model.to_string()),
            // A routed entry names its exact model: no catalog class needed.
            (None, Some(model)) => self
                .model_policy
                .allows(backend, model)
                .then(|| model.to_string()),
            (None, None) => self.model_for(backend, tier),
        }
    }

    /// The plan is a snapshot; the owner may switch paid APIs off while a
    /// request is in flight. Re-check right before every attempt.
    fn paid_api_now_refuses(&self, backend: &str) -> bool {
        self.routing.snapshot().refuses_provider(backend)
    }

    fn requested_model_is_allowed(&self, backend: &str, model: &str) -> bool {
        (Self::dynamic_cloud_backend(backend)
            || self
                .catalog
                .iter()
                .any(|entry| entry.backend == backend && entry.id == model))
            && self.model_policy.allows(backend, model)
    }

    fn dynamic_cloud_backend(backend: &str) -> bool {
        matches!(
            backend,
            "openai-api"
                | "anthropic-api"
                | "deepseek-api"
                | "xai-api"
                | "zai-api"
                | "ollama-cloud"
                | "huggingface"
        )
    }
}

#[async_trait]
impl LlmProvider for RouterProvider {
    fn label(&self) -> &str {
        &self.label
    }

    fn can_serve_tier(&self, tier: Tier) -> bool {
        self.plan(tier, true, None)
            .iter()
            .any(|(candidate, pinned)| {
                self.choose_model(&candidate.id, pinned.as_deref(), None, tier)
                    .is_some()
            })
    }

    async fn chat_stream(
        &self,
        req: &ChatRequest,
        sink: crate::TextDeltaSink,
    ) -> Result<ChatReply, LlmError> {
        for (candidate, pinned) in self.plan(req.tier, req.model.is_none(), req.provider.as_deref())
        {
            if self.paid_api_now_refuses(&candidate.id) {
                continue;
            }
            let chosen = self.choose_model(
                &candidate.id,
                pinned.as_deref(),
                req.model.as_deref(),
                req.tier,
            );
            let Some(model) = chosen else { continue };
            let attempt = ChatRequest {
                model: Some(model),
                ..req.clone()
            };
            // A realtime run makes exactly one attempt. On ambiguous transport
            // failure, neither replay a paid call nor mix another model's text
            // with already displayed/spoken deltas.
            let result = candidate.provider.chat_stream(&attempt, sink).await;
            match &result {
                Ok(_) => self.record_success(&candidate.id),
                Err(error) => self.record_failure(&candidate.id, error.failure_category()),
            }
            return result;
        }
        Err(LlmError::NotConfigured(
            "no owner-enabled capable brain".into(),
        ))
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let plan = self.plan(req.tier, req.model.is_none(), req.provider.as_deref());
        if plan.is_empty() {
            return Err(LlmError::NotConfigured(
                "no capable brain for this tier".into(),
            ));
        }
        let mut last = None;
        for (candidate, pinned) in plan {
            // A requested model wins, then the owner's routed model, else the
            // cheapest sufficient catalog model for this backend + tier.
            let chosen = self.choose_model(
                &candidate.id,
                pinned.as_deref(),
                req.model.as_deref(),
                req.tier,
            );
            // Never fall through to a provider's configured default: that would
            // turn an API key or a mutable provider alias into an implicit
            // model authorization bypass.
            let Some(chosen) = chosen else {
                continue;
            };
            if self.paid_api_now_refuses(&candidate.id) {
                continue;
            }
            let attempt = ChatRequest {
                model: Some(chosen),
                ..req.clone()
            };
            match candidate.provider.chat(&attempt).await {
                Ok(reply) => {
                    self.record_success(&candidate.id);
                    return Ok(reply);
                }
                Err(LlmError::Refused) => return Err(LlmError::Refused),
                Err(e) => {
                    let failure = e.failure_category();
                    self.record_failure(&candidate.id, failure);
                    tracing::warn!(
                        backend = %candidate.id,
                        failure = ?failure,
                        "brain failed; routing to next"
                    );
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| LlmError::NotConfigured("no brain answered".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;
    use crate::RoutingSnapshot;

    struct Fixed {
        label: String,
        ok: bool,
    }
    #[async_trait]
    impl LlmProvider for Fixed {
        fn label(&self) -> &str {
            &self.label
        }
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatReply, LlmError> {
            if self.ok {
                Ok(ChatReply {
                    text: self.label.clone(),
                    model: self.label.clone(),
                    backend: Some(self.label.clone()),
                    requested_route: None,
                    actual_provider: None,
                    stop_reason: None,
                    usage: None,
                })
            } else {
                Err(LlmError::Empty)
            }
        }
    }

    fn cand(id: &str, ok: bool) -> Candidate {
        Candidate {
            id: id.to_string(),
            provider: Arc::new(Fixed {
                label: id.to_string(),
                ok,
            }),
        }
    }

    struct Only(&'static str);
    impl Availability for Only {
        fn is_available(&self, id: &str) -> bool {
            id == self.0
        }
    }

    fn all() -> Vec<Candidate> {
        vec![
            cand("ollama", true),
            cand("claude-cli", true),
            cand("anthropic-api", true),
        ]
    }

    fn ids(plan: Vec<(&Candidate, Option<String>)>) -> Vec<String> {
        plan.iter().map(|(c, _)| c.id.clone()).collect()
    }

    fn catalog() -> Vec<CatalogModel> {
        let m = |backend: &str, id: &str, class| CatalogModel {
            backend: backend.into(),
            id: id.into(),
            class,
        };
        vec![
            m("ollama", "llama3.2", ModelClass::Light),
            m("claude-cli", "claude-haiku-4-5", ModelClass::Light),
            m("claude-cli", "claude-opus-5", ModelClass::Heavy),
            m("anthropic-api", "claude-sonnet-5", ModelClass::Mid),
        ]
    }

    fn allow_catalog(catalog: &[CatalogModel]) -> ModelAccessPolicy {
        ModelAccessPolicy {
            version: 1,
            models: catalog
                .iter()
                .map(|model| crate::ModelAccessEntry {
                    provider: model.backend.clone(),
                    model: model.id.clone(),
                    enabled: true,
                    source: "test".into(),
                    route: None,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn live_disable_stops_calls_without_rebuilding_router() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counted(Arc<AtomicUsize>);
        #[async_trait]
        impl LlmProvider for Counted {
            fn label(&self) -> &str {
                "fixture"
            }
            async fn chat(&self, request: &ChatRequest) -> Result<ChatReply, LlmError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Fixed {
                    label: "ollama-cloud".into(),
                    ok: true,
                }
                .chat(request)
                .await
            }
        }
        let catalog = vec![CatalogModel {
            backend: "ollama-cloud".into(),
            id: "fixture".into(),
            class: ModelClass::Light,
        }];
        let enabled = allow_catalog(&catalog);
        let mut disabled = enabled.clone();
        disabled.models[0].enabled = false;
        let live = Arc::new(LiveModelPolicy::new(disabled.clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let router = RouterProvider::with_live_policy(
            vec![Candidate {
                id: "ollama-cloud".into(),
                provider: Arc::new(Counted(calls.clone())),
            }],
            always_available(),
            catalog,
            live.clone(),
        );
        let request = ChatRequest {
            system: None,
            messages: vec![ChatMessage::user("fixture")],
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: Some("fixture".into()),
            provider: None,
        };
        assert!(router.chat(&request).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        live.activate_verified_toggle(&disabled, enabled.clone(), "ollama-cloud", "fixture", true)
            .unwrap();
        assert!(router.chat(&request).await.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        live.activate_verified_toggle(&enabled, disabled, "ollama-cloud", "fixture", false)
            .unwrap();
        assert!(router.chat(&request).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        live.suspend();
        assert!(!live.allows("ollama", "not-listed"));
    }

    #[test]
    fn picks_low_models_by_default_and_strong_for_hard() {
        let c = catalog();
        let r =
            RouterProvider::with_policy(all(), always_available(), c.clone(), allow_catalog(&c));
        // Default → light on the plan (cheap sufficient, free).
        assert_eq!(
            r.model_for("claude-cli", Tier::Default).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            r.model_for("claude-cli", Tier::Cheap).as_deref(),
            Some("claude-haiku-4-5")
        );
        // Hard → the heavy model.
        assert_eq!(
            r.model_for("claude-cli", Tier::Hard).as_deref(),
            Some("claude-opus-5")
        );
        // No light for anthropic-api → Default escalates to its Mid model.
        assert_eq!(
            r.model_for("anthropic-api", Tier::Default).as_deref(),
            Some("claude-sonnet-5")
        );
        // Ollama has only a light model → nothing for a Hard task (uses default).
        assert_eq!(r.model_for("ollama", Tier::Hard), None);
    }

    #[test]
    fn advisory_tier_check_respects_live_owner_allowlist() {
        let c = vec![CatalogModel {
            backend: "openai-api".into(),
            id: "owner-enabled-mid".into(),
            class: ModelClass::Mid,
        }];
        let enabled = allow_catalog(&c);
        let live = Arc::new(LiveModelPolicy::new(enabled));
        let r = RouterProvider::with_live_policy(
            vec![cand("openai-api", true)],
            always_available(),
            c,
            live.clone(),
        );
        assert!(!r.can_serve_tier(Tier::Cheap));
        assert!(r.can_serve_tier(Tier::Default));
        assert!(r.can_serve_tier(Tier::Hard));
        live.suspend();
        assert!(!r.can_serve_tier(Tier::Default));
        assert!(!r.can_serve_tier(Tier::Hard));
    }

    #[tokio::test]
    async fn router_sends_the_chosen_model_to_the_backend() {
        // A provider that echoes back whichever model it was handed.
        struct EchoModel;
        #[async_trait]
        impl LlmProvider for EchoModel {
            fn label(&self) -> &str {
                "claude-cli"
            }
            async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
                Ok(ChatReply {
                    text: String::new(),
                    model: req.model.clone().unwrap_or_else(|| "DEFAULT".into()),
                    backend: Some("claude-cli".into()),
                    requested_route: None,
                    actual_provider: None,
                    stop_reason: None,
                    usage: None,
                })
            }
        }
        let cands = vec![Candidate {
            id: "claude-cli".into(),
            provider: Arc::new(EchoModel),
        }];
        let c = catalog();
        let r =
            RouterProvider::with_policy(cands, always_available(), c.clone(), allow_catalog(&c));
        let ask = |tier| ChatRequest {
            system: None,
            messages: vec![ChatMessage::user("hi")],
            tier,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        assert_eq!(
            r.chat(&ask(Tier::Default)).await.unwrap().model,
            "claude-haiku-4-5"
        );
        assert_eq!(
            r.chat(&ask(Tier::Hard)).await.unwrap().model,
            "claude-opus-5"
        );
    }

    #[test]
    fn cheap_prefers_local_then_plan_then_api() {
        let r = RouterProvider::new(all(), always_available(), vec![]);
        assert_eq!(
            ids(r.plan(Tier::Cheap, true, None)),
            ["ollama", "claude-cli", "anthropic-api"]
        );
    }

    #[test]
    fn default_prefers_plan_then_api_then_local() {
        let r = RouterProvider::new(all(), always_available(), vec![]);
        assert_eq!(
            ids(r.plan(Tier::Default, true, None)),
            ["claude-cli", "anthropic-api", "ollama"]
        );
    }

    #[test]
    fn hard_is_strong_only() {
        let r = RouterProvider::new(all(), always_available(), vec![]);
        assert_eq!(
            ids(r.plan(Tier::Hard, true, None)),
            ["claude-cli", "anthropic-api"]
        );
    }

    #[test]
    fn full_fleet_orders_by_cost_and_quality_per_tier() {
        let fleet = vec![
            cand("ollama", true),
            cand("claude-cli", true),
            cand("deepseek-api", true),
            cand("openai-api", true),
            cand("anthropic-api", true),
        ];
        let r = RouterProvider::new(fleet, always_available(), vec![]);
        // Cheap: free first, then the cheapest metered.
        assert_eq!(
            ids(r.plan(Tier::Cheap, true, None)),
            [
                "ollama",
                "claude-cli",
                "deepseek-api",
                "openai-api",
                "anthropic-api"
            ]
        );
        // Default: plan first, strong APIs, then cheap/local last.
        assert_eq!(
            ids(r.plan(Tier::Default, true, None)),
            [
                "claude-cli",
                "anthropic-api",
                "openai-api",
                "deepseek-api",
                "ollama"
            ]
        );
        // Hard: strong brains only (no deepseek/ollama).
        assert_eq!(
            ids(r.plan(Tier::Hard, true, None)),
            ["claude-cli", "anthropic-api", "openai-api"]
        );
    }

    #[test]
    fn availability_filters_but_keeps_a_safety_net() {
        let r = RouterProvider::new(all(), Arc::new(Only("anthropic-api")), vec![]);
        assert_eq!(ids(r.plan(Tier::Default, true, None)), ["anthropic-api"]);

        // If the registry claims nothing is up, still try the unmetered
        // backends (ordered) rather than fail, but never a paid one.
        let r2 = RouterProvider::new(all(), Arc::new(Nothing), vec![]);
        assert_eq!(
            ids(r2.plan(Tier::Default, true, None)),
            ["claude-cli", "ollama"]
        );
    }

    struct Nothing;
    impl Availability for Nothing {
        fn is_available(&self, _: &str) -> bool {
            false
        }
    }

    /// Counts calls; always fails so the router would fall through.
    struct Counting(Arc<std::sync::atomic::AtomicUsize>);
    #[async_trait]
    impl LlmProvider for Counting {
        fn label(&self) -> &str {
            "counting"
        }
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatReply, LlmError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(LlmError::Empty)
        }
    }

    fn counted(id: &str) -> (Candidate, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            Candidate {
                id: id.into(),
                provider: Arc::new(Counting(calls.clone())),
            },
            calls,
        )
    }

    fn ask(tier: Tier, model: Option<&str>) -> ChatRequest {
        ChatRequest {
            system: None,
            messages: vec![ChatMessage::user("fixture")],
            tier,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: model.map(str::to_string),
            provider: None,
        }
    }

    #[tokio::test]
    async fn cap_reached_and_nothing_available_never_attempts_a_metered_backend() {
        use std::sync::atomic::Ordering;
        let (paid, paid_calls) = counted("anthropic-api");
        let (plan, plan_calls) = counted("claude-cli");
        let c = vec![
            CatalogModel {
                backend: "anthropic-api".into(),
                id: "paid".into(),
                class: ModelClass::Light,
            },
            CatalogModel {
                backend: "claude-cli".into(),
                id: "plan".into(),
                class: ModelClass::Light,
            },
        ];
        let r = RouterProvider::with_policy(
            vec![paid, plan],
            Arc::new(Nothing),
            c.clone(),
            allow_catalog(&c),
        );
        for tier in [Tier::Cheap, Tier::Default, Tier::Hard] {
            assert!(r.chat(&ask(tier, None)).await.is_err());
            assert!(r.chat(&ask(tier, Some("paid"))).await.is_err());
        }
        assert_eq!(paid_calls.load(Ordering::SeqCst), 0);
        assert!(plan_calls.load(Ordering::SeqCst) > 0);

        // Only metered backends configured: nothing is attempted at all.
        let (paid, paid_calls) = counted("openai-api");
        let c = vec![CatalogModel {
            backend: "openai-api".into(),
            id: "gpt".into(),
            class: ModelClass::Mid,
        }];
        let r = RouterProvider::with_policy(
            vec![paid],
            Arc::new(Nothing),
            c.clone(),
            allow_catalog(&c),
        );
        assert!(r.plan(Tier::Default, true, None).is_empty());
        assert!(r.chat(&ask(Tier::Default, Some("gpt"))).await.is_err());
        assert_eq!(paid_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unknown_backends_are_treated_as_metered() {
        assert!(!is_metered_backend("ollama"));
        assert!(!is_metered_backend("claude-cli"));
        for id in [
            "anthropic-api",
            "openai-api",
            "zai-api",
            "huggingface",
            "new",
        ] {
            assert!(is_metered_backend(id));
        }
    }

    #[tokio::test]
    async fn falls_through_to_a_working_backend() {
        // Default order is claude-cli → anthropic-api → ollama; fail the first.
        let cands = vec![
            cand("ollama", true),
            cand("claude-cli", false),
            cand("anthropic-api", true),
        ];
        let c = vec![
            CatalogModel {
                backend: "claude-cli".into(),
                id: "plan".into(),
                class: ModelClass::Mid,
            },
            CatalogModel {
                backend: "anthropic-api".into(),
                id: "sonnet".into(),
                class: ModelClass::Mid,
            },
            CatalogModel {
                backend: "ollama".into(),
                id: "local".into(),
                class: ModelClass::Mid,
            },
        ];
        let r =
            RouterProvider::with_policy(cands, always_available(), c.clone(), allow_catalog(&c));
        let reply = r
            .chat(&ChatRequest {
                system: None,
                messages: vec![ChatMessage::user("hi")],
                tier: Tier::Default,
                mode: crate::RoutingMode::Auto,
                max_tokens: 16,
                model: None,
                provider: None,
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "anthropic-api");
        // The malformed first response receives a bounded health cooldown, so
        // the next request does not repeatedly hit the same failing backend.
        assert_eq!(
            ids(r.plan(Tier::Default, true, None)),
            ["anthropic-api", "ollama"]
        );
    }

    #[test]
    fn auth_cooldown_is_longer_than_transient_failures() {
        assert!(
            RouterProvider::cooldown_for(ProviderFailure::Authentication)
                > RouterProvider::cooldown_for(ProviderFailure::RateLimited)
        );
        assert!(RouterProvider::cooldown_for(ProviderFailure::Refused).is_zero());
    }

    #[tokio::test]
    async fn post_startup_discovery_needs_approval_then_routes_without_restart() {
        let live = Arc::new(LiveModelPolicy::new(ModelAccessPolicy::deny_by_default()));
        let router = RouterProvider::with_live_policy(
            vec![cand("openai-api", true)],
            always_available(),
            vec![],
            live.clone(),
        );
        let discovered = ModelAccessPolicy {
            version: 1,
            models: vec![crate::ModelAccessEntry {
                provider: "openai-api".into(),
                model: "fixture-discovered".into(),
                enabled: false,
                source: "provider_api".into(),
                route: None,
            }],
        };
        live.refresh_discovery(discovered.clone()).unwrap();
        let mut request = ChatRequest {
            system: None,
            messages: vec![ChatMessage::user("fixture")],
            tier: Tier::Default,
            mode: crate::RoutingMode::Auto,
            max_tokens: 16,
            model: None,
            provider: None,
        };
        assert!(router.chat(&request).await.is_err());
        assert!(!router.requested_model_is_allowed("openai-api", "fixture-discovered"));
        let mut enabled = discovered.clone();
        enabled.models[0].enabled = true;
        live.activate_verified_toggle(
            &discovered,
            enabled,
            "openai-api",
            "fixture-discovered",
            true,
        )
        .unwrap();
        assert_eq!(router.chat(&request).await.unwrap().text, "openai-api");
        request.model = Some("fixture-discovered".into());
        assert!(router.chat(&request).await.is_ok());
        request.model = Some("not-enabled".into());
        assert!(router.chat(&request).await.is_err());
        assert_eq!(router.model_for("openai-api", Tier::Cheap), None);
        live.suspend();
        assert!(router.chat(&request).await.is_err());
    }

    #[tokio::test]
    async fn disabled_model_cannot_be_reached_by_fallback_or_override() {
        let cands = vec![cand("anthropic-api", true)];
        let catalog = vec![CatalogModel {
            backend: "anthropic-api".into(),
            id: "claude-test".into(),
            class: ModelClass::Mid,
        }];
        let policy = ModelAccessPolicy {
            version: 1,
            models: vec![crate::ModelAccessEntry {
                provider: "anthropic-api".into(),
                model: "claude-test".into(),
                enabled: false,
                source: "test".into(),
                route: None,
            }],
        };
        let router = RouterProvider::with_policy(cands, always_available(), catalog, policy);
        let result = router
            .chat(&ChatRequest {
                system: None,
                messages: vec![ChatMessage::user("hi")],
                tier: Tier::Default,
                mode: crate::RoutingMode::Auto,
                max_tokens: 16,
                model: Some("claude-test".into()),
                provider: None,
            })
            .await;
        assert!(matches!(result, Err(LlmError::NotConfigured(_))));
    }

    /// Echoes the backend id and the model it was handed; counts calls.
    struct EchoAs(&'static str, Arc<std::sync::atomic::AtomicUsize>, bool);
    #[async_trait]
    impl LlmProvider for EchoAs {
        fn label(&self) -> &str {
            self.0
        }
        async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !self.2 {
                return Err(LlmError::Empty);
            }
            Ok(ChatReply {
                text: self.0.into(),
                model: req.model.clone().unwrap_or_default(),
                backend: Some(self.0.into()),
                requested_route: None,
                actual_provider: None,
                stop_reason: None,
                usage: None,
            })
        }
    }

    struct Fleet {
        router: RouterProvider,
        calls: BTreeMap<&'static str, Arc<std::sync::atomic::AtomicUsize>>,
    }

    impl Fleet {
        fn calls(&self, id: &str) -> usize {
            self.calls[id].load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// ollama, claude-cli, zai-api and anthropic-api; `failing` backends error.
    fn fleet(
        routing: RoutingSnapshot,
        enabled: &[(&str, &str)],
        availability: Arc<dyn Availability>,
        failing: &[&str],
    ) -> Fleet {
        let mut calls = BTreeMap::new();
        let candidates = ["ollama", "claude-cli", "zai-api", "anthropic-api"]
            .into_iter()
            .map(|id| {
                let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                calls.insert(id, counter.clone());
                Candidate {
                    id: id.into(),
                    provider: Arc::new(EchoAs(id, counter, !failing.contains(&id))),
                }
            })
            .collect();
        let policy = ModelAccessPolicy {
            version: 1,
            models: enabled
                .iter()
                .map(|(provider, model)| crate::ModelAccessEntry {
                    provider: provider.to_string(),
                    model: model.to_string(),
                    enabled: true,
                    source: "test".into(),
                    route: None,
                })
                .collect(),
        };
        let c = catalog();
        let router = RouterProvider::with_policy(candidates, availability, c, policy)
            .with_routing(Arc::new(LiveRouting::new(routing)));
        Fleet { router, calls }
    }

    fn routing(raw: &str) -> RoutingSnapshot {
        RoutingSnapshot {
            routing: Some(crate::ModelRouting::parse(raw.as_bytes()).unwrap()),
            unavailable_reason: None,
        }
    }

    const CHEAP_ROUTE: &str = r#"{"version":1,"tiers":{"cheap":{"chain":[
        {"provider":"zai-api","model":"glm-5.3-flash"},
        {"provider":"claude-cli","model":"claude-haiku-4-5"}]}}}"#;

    #[tokio::test]
    async fn routed_chain_overrides_builtin_order_with_pinned_models() {
        let f = fleet(
            routing(CHEAP_ROUTE),
            &[
                ("zai-api", "glm-5.3-flash"),
                ("claude-cli", "claude-haiku-4-5"),
            ],
            always_available(),
            &[],
        );
        assert_eq!(
            ids(f.router.plan(Tier::Cheap, true, None)),
            ["zai-api", "claude-cli"]
        );
        // A tier without a chain keeps the built-in order.
        assert_eq!(
            ids(f.router.plan(Tier::Default, true, None)),
            ["claude-cli", "anthropic-api", "zai-api", "ollama"]
        );
        // The pinned model is used even though the catalog has no zai entry.
        let reply = f.router.chat(&ask(Tier::Cheap, None)).await.unwrap();
        assert_eq!(
            (reply.text.as_str(), reply.model.as_str()),
            ("zai-api", "glm-5.3-flash")
        );
        assert!(f.router.can_serve_tier(Tier::Cheap));
    }

    #[tokio::test]
    async fn routed_entries_still_pass_allowlist_availability_and_health() {
        // Allowlist: the zai pin is not enabled, so it is never attempted.
        let f = fleet(
            routing(CHEAP_ROUTE),
            &[("claude-cli", "claude-haiku-4-5")],
            always_available(),
            &[],
        );
        let reply = f.router.chat(&ask(Tier::Cheap, None)).await.unwrap();
        assert_eq!(reply.model, "claude-haiku-4-5");
        assert_eq!(f.calls("zai-api"), 0);

        // Availability (the monthly cap): an unavailable metered entry is skipped.
        let f = fleet(
            routing(CHEAP_ROUTE),
            &[
                ("zai-api", "glm-5.3-flash"),
                ("claude-cli", "claude-haiku-4-5"),
            ],
            Arc::new(Only("claude-cli")),
            &[],
        );
        assert_eq!(ids(f.router.plan(Tier::Cheap, true, None)), ["claude-cli"]);
        f.router.chat(&ask(Tier::Cheap, None)).await.unwrap();
        assert_eq!(f.calls("zai-api"), 0);

        // Health: a failing entry falls through, then cools down.
        let f = fleet(
            routing(CHEAP_ROUTE),
            &[
                ("zai-api", "glm-5.3-flash"),
                ("claude-cli", "claude-haiku-4-5"),
            ],
            always_available(),
            &["zai-api"],
        );
        let reply = f.router.chat(&ask(Tier::Cheap, None)).await.unwrap();
        assert_eq!(reply.text, "claude-cli");
        assert_eq!(ids(f.router.plan(Tier::Cheap, true, None)), ["claude-cli"]);
        assert_eq!(f.calls("zai-api"), 1);

        // Nothing routable: the chain does not silently widen to other backends.
        let f = fleet(routing(CHEAP_ROUTE), &[], always_available(), &[]);
        assert!(f.router.chat(&ask(Tier::Cheap, None)).await.is_err());
        assert!(!f.router.can_serve_tier(Tier::Cheap));
        assert_eq!(f.calls("ollama") + f.calls("anthropic-api"), 0);
    }

    #[tokio::test]
    async fn paid_api_off_removes_metered_from_routed_and_builtin_orders() {
        let f = fleet(
            routing(
                r#"{"version":1,"paid_api":"off","tiers":{"cheap":{"chain":[
                {"provider":"zai-api","model":"glm-5.3-flash"},
                {"provider":"claude-cli","model":"claude-haiku-4-5"}]}}}"#,
            ),
            &[
                ("zai-api", "glm-5.3-flash"),
                ("claude-cli", "claude-haiku-4-5"),
                ("anthropic-api", "claude-sonnet-5"),
            ],
            always_available(),
            &[],
        );
        assert_eq!(ids(f.router.plan(Tier::Cheap, true, None)), ["claude-cli"]);
        for tier in [Tier::Default, Tier::Hard] {
            assert!(f
                .router
                .plan(tier, true, None)
                .iter()
                .all(|(c, _)| !is_metered_backend(&c.id)));
        }
        assert_eq!(
            ids(f.router.plan(Tier::Default, false, None)),
            ["claude-cli", "ollama"]
        );
        // An explicitly requested metered model is not reached either.
        let result = f
            .router
            .chat(&ask(Tier::Default, Some("claude-sonnet-5")))
            .await;
        assert!(result.is_err());
        f.router.chat(&ask(Tier::Cheap, None)).await.unwrap();
        assert_eq!(f.calls("zai-api") + f.calls("anthropic-api"), 0);
    }

    #[test]
    fn unusable_routing_file_falls_back_to_builtin_order_without_metered() {
        let f = fleet(
            RoutingSnapshot::unavailable("routing_invalid"),
            &[],
            always_available(),
            &[],
        );
        assert_eq!(
            ids(f.router.plan(Tier::Default, true, None)),
            ["claude-cli", "ollama"]
        );
        assert_eq!(
            ids(f.router.plan(Tier::Cheap, true, None)),
            ["ollama", "claude-cli"]
        );
        assert_eq!(ids(f.router.plan(Tier::Hard, true, None)), ["claude-cli"]);
    }

    #[tokio::test]
    async fn requested_model_uses_builtin_order_and_live_swap_needs_no_rebuild() {
        let live = Arc::new(LiveRouting::default());
        let f = fleet(
            RoutingSnapshot::default(),
            &[
                ("claude-cli", "claude-haiku-4-5"),
                ("anthropic-api", "claude-sonnet-5"),
            ],
            always_available(),
            &[],
        );
        let router = f.router.with_routing(live.clone());
        live.swap(
            &RoutingSnapshot::default(),
            routing(
                r#"{"version":1,"tiers":{"default":{"chain":[
                {"provider":"claude-cli","model":"claude-haiku-4-5"}]}}}"#,
            ),
        )
        .unwrap();
        assert_eq!(ids(router.plan(Tier::Default, true, None)), ["claude-cli"]);
        // An explicit model is more specific than the tier chain.
        let reply = router
            .chat(&ask(Tier::Default, Some("claude-sonnet-5")))
            .await
            .unwrap();
        assert_eq!(reply.text, "anthropic-api");
    }

    #[tokio::test]
    async fn pinned_provider_never_falls_back_to_another_provider() {
        let enabled = [
            ("claude-cli", "claude-opus-5"),
            ("anthropic-api", "claude-opus-5"),
        ];
        let pin = |provider: &str| ChatRequest {
            provider: Some(provider.into()),
            ..ask(Tier::Hard, Some("claude-opus-5"))
        };
        let f = fleet(
            RoutingSnapshot::default(),
            &enabled,
            always_available(),
            &["claude-cli"],
        );
        assert!(f.router.chat(&pin("claude-cli")).await.is_err());
        assert_eq!(f.calls("claude-cli"), 1);
        assert_eq!(f.calls("anthropic-api"), 0);

        let f = fleet(
            RoutingSnapshot::default(),
            &enabled,
            always_available(),
            &[],
        );
        let reply = f.router.chat(&pin("claude-cli")).await.unwrap();
        assert_eq!(
            (reply.text.as_str(), reply.model.as_str()),
            ("claude-cli", "claude-opus-5")
        );

        // A metered pin is refused while paid APIs are off, never rerouted.
        let f = fleet(
            routing(r#"{"version":1,"paid_api":"off"}"#),
            &enabled,
            always_available(),
            &[],
        );
        assert!(f.router.chat(&pin("anthropic-api")).await.is_err());
        assert_eq!(f.calls("anthropic-api") + f.calls("claude-cli"), 0);
    }

    #[tokio::test]
    async fn paid_api_switched_off_mid_request_stops_metered_attempts() {
        struct SwitchOffThenFail(Arc<LiveRouting>);
        #[async_trait]
        impl LlmProvider for SwitchOffThenFail {
            fn label(&self) -> &str {
                "claude-cli"
            }
            async fn chat(&self, _req: &ChatRequest) -> Result<ChatReply, LlmError> {
                let current = self.0.snapshot();
                let mut next = current.clone();
                next.routing.as_mut().unwrap().paid_api = crate::PaidApi::Off;
                self.0.swap(&current, next).unwrap();
                Err(LlmError::Empty)
            }
        }
        let live = Arc::new(LiveRouting::new(routing(
            r#"{"version":1,"tiers":{"default":{"chain":[
            {"provider":"claude-cli","model":"claude-haiku-4-5"},
            {"provider":"zai-api","model":"glm-5.3-flash"}],
            "metered_after_subscription":true}}}"#,
        )));
        let (zai, zai_calls) = counted("zai-api");
        let candidates = vec![
            Candidate {
                id: "claude-cli".into(),
                provider: Arc::new(SwitchOffThenFail(live.clone())),
            },
            zai,
        ];
        let c = catalog();
        let mut policy = allow_catalog(&c);
        policy.models.push(crate::ModelAccessEntry {
            provider: "zai-api".into(),
            model: "glm-5.3-flash".into(),
            enabled: true,
            source: "test".into(),
            route: None,
        });
        let router = RouterProvider::with_policy(candidates, always_available(), c, policy)
            .with_routing(live);
        assert!(router.chat(&ask(Tier::Default, None)).await.is_err());
        assert_eq!(zai_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn only_a_loopback_ollama_url_counts_as_local() {
        for url in [
            "http://127.0.0.1:11434",
            "http://127.8.9.10:11434/",
            "http://localhost:11434",
            "http://LOCALHOST",
            "http://[::1]:11434",
        ] {
            assert!(is_loopback_url(url), "{url}");
        }
        for url in [
            "http://192.168.1.20:11434",
            "https://ollama.example.com",
            "http://localhost.example.com",
            "http://127.0.0.1.nip.io",
            "http://[::ffff:c0a8:114]",
            "not a url",
            "",
        ] {
            assert!(!is_loopback_url(url), "{url}");
        }
    }
}
