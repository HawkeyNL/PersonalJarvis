//! Cost-aware router that consults live availability (ADR-027).
//!
//! Per request it orders the backends by a *sensible* policy — cheapest for
//! trivial tasks, quality-first for real work, strong-only for the hardest — and
//! tries them in order, skipping ones the registry marks unavailable (with a
//! safety net: if the registry says none are up, try them anyway). Falls through
//! on any error except a genuine refusal.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;

use crate::types::{ChatReply, ChatRequest, LlmError, ProviderFailure, Tier};
use crate::{LlmProvider, ModelAccessPolicy};

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
    model_policy: ModelAccessPolicy,
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

    pub(crate) fn with_policy(
        candidates: Vec<Candidate>,
        availability: Arc<dyn Availability>,
        catalog: Vec<CatalogModel>,
        model_policy: ModelAccessPolicy,
    ) -> Self {
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let label = format!("router[{}]", ids.join(","));
        Self {
            candidates,
            availability,
            catalog,
            model_policy,
            health: Mutex::new(BTreeMap::new()),
            label,
        }
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
        }
        None
    }

    /// Preference order of backend ids for a tier. Cheap → cheapest first (free
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

    /// The backends to try, in order: policy order ∩ existing candidates,
    /// preferring registry-available ones. A recent health cooldown is never
    /// overridden by the old "try everything" safety net: that would turn a
    /// revoked credential or 429 into an unbounded request-by-request retry.
    fn plan(&self, tier: Tier) -> Vec<&Candidate> {
        let ordered: Vec<&Candidate> = Self::policy(tier)
            .iter()
            .filter_map(|id| self.candidates.iter().find(|c| c.id == *id))
            .collect();
        let available: Vec<&Candidate> = ordered
            .iter()
            .copied()
            .filter(|c| self.availability.is_available(&c.id))
            .collect();
        let base = if available.is_empty() {
            ordered
        } else {
            available
        };
        base.into_iter()
            .filter(|candidate| self.is_healthy(&candidate.id))
            .collect()
    }

    fn requested_model_is_allowed(&self, backend: &str, model: &str) -> bool {
        self.catalog
            .iter()
            .any(|entry| entry.backend == backend && entry.id == model)
            && self.model_policy.allows(backend, model)
    }
}

#[async_trait]
impl LlmProvider for RouterProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        let plan = self.plan(req.tier);
        if plan.is_empty() {
            return Err(LlmError::NotConfigured(
                "no capable brain for this tier".into(),
            ));
        }
        let mut last = None;
        for candidate in plan {
            // Pick the cheapest sufficient model for this backend + tier; if the
            // request already names a model, respect it. Fall back to the
            // provider's own tier model when the catalog has nothing.
            let chosen = match req.model.as_deref() {
                Some(model) if self.requested_model_is_allowed(&candidate.id, model) => {
                    Some(model.to_string())
                }
                Some(_) => None,
                None => self.model_for(&candidate.id, req.tier),
            };
            // Never fall through to a provider's configured default: that would
            // turn an API key or a mutable provider alias into an implicit
            // model authorization bypass.
            let Some(chosen) = chosen else {
                continue;
            };
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

    fn ids(plan: Vec<&Candidate>) -> Vec<String> {
        plan.iter().map(|c| c.id.clone()).collect()
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
            ids(r.plan(Tier::Cheap)),
            ["ollama", "claude-cli", "anthropic-api"]
        );
    }

    #[test]
    fn default_prefers_plan_then_api_then_local() {
        let r = RouterProvider::new(all(), always_available(), vec![]);
        assert_eq!(
            ids(r.plan(Tier::Default)),
            ["claude-cli", "anthropic-api", "ollama"]
        );
    }

    #[test]
    fn hard_is_strong_only() {
        let r = RouterProvider::new(all(), always_available(), vec![]);
        assert_eq!(ids(r.plan(Tier::Hard)), ["claude-cli", "anthropic-api"]);
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
            ids(r.plan(Tier::Cheap)),
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
            ids(r.plan(Tier::Default)),
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
            ids(r.plan(Tier::Hard)),
            ["claude-cli", "anthropic-api", "openai-api"]
        );
    }

    #[test]
    fn availability_filters_but_keeps_a_safety_net() {
        let r = RouterProvider::new(all(), Arc::new(Only("anthropic-api")), vec![]);
        assert_eq!(ids(r.plan(Tier::Default)), ["anthropic-api"]);

        // If the registry claims nothing is up, still try (ordered) rather than fail.
        struct None_;
        impl Availability for None_ {
            fn is_available(&self, _: &str) -> bool {
                false
            }
        }
        let r2 = RouterProvider::new(all(), Arc::new(None_), vec![]);
        assert_eq!(
            ids(r2.plan(Tier::Default)),
            ["claude-cli", "anthropic-api", "ollama"]
        );
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
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "anthropic-api");
        // The malformed first response receives a bounded health cooldown, so
        // the next request does not repeatedly hit the same failing backend.
        assert_eq!(ids(r.plan(Tier::Default)), ["anthropic-api", "ollama"]);
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
            })
            .await;
        assert!(matches!(result, Err(LlmError::NotConfigured(_))));
    }
}
