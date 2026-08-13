//! Cost-aware router that consults live availability (ADR-027).
//!
//! Per request it orders the backends by a *sensible* policy — cheapest for
//! trivial tasks, quality-first for real work, strong-only for the hardest — and
//! tries them in order, skipping ones the registry marks unavailable (with a
//! safety net: if the registry says none are up, try them anyway). Falls through
//! on any error except a genuine refusal.

use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{ChatReply, ChatRequest, LlmError, Tier};
use crate::LlmProvider;

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

/// A backend the router can route to.
pub(crate) struct Candidate {
    pub(crate) id: String,
    pub(crate) provider: Arc<dyn LlmProvider>,
}

pub struct RouterProvider {
    candidates: Vec<Candidate>,
    availability: Arc<dyn Availability>,
    label: String,
}

impl RouterProvider {
    pub(crate) fn new(candidates: Vec<Candidate>, availability: Arc<dyn Availability>) -> Self {
        let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let label = format!("router[{}]", ids.join(","));
        Self {
            candidates,
            availability,
            label,
        }
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
                "openai-api",
                "anthropic-api",
            ],
            Tier::Default => &[
                "claude-cli",
                "anthropic-api",
                "openai-api",
                "deepseek-api",
                "ollama",
            ],
            Tier::Hard => &["claude-cli", "anthropic-api", "openai-api"],
        }
    }

    /// The backends to try, in order: policy order ∩ existing candidates,
    /// preferring available ones — but if none look available, try them all.
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
        if available.is_empty() {
            ordered
        } else {
            available
        }
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
            match candidate.provider.chat(req).await {
                Ok(reply) => return Ok(reply),
                Err(LlmError::Refused) => return Err(LlmError::Refused),
                Err(e) => {
                    tracing::warn!(backend = %candidate.id, error = %e, "brain failed; routing to next");
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

    #[test]
    fn cheap_prefers_local_then_plan_then_api() {
        let r = RouterProvider::new(all(), always_available());
        assert_eq!(ids(r.plan(Tier::Cheap)), ["ollama", "claude-cli", "anthropic-api"]);
    }

    #[test]
    fn default_prefers_plan_then_api_then_local() {
        let r = RouterProvider::new(all(), always_available());
        assert_eq!(ids(r.plan(Tier::Default)), ["claude-cli", "anthropic-api", "ollama"]);
    }

    #[test]
    fn hard_is_strong_only() {
        let r = RouterProvider::new(all(), always_available());
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
        let r = RouterProvider::new(fleet, always_available());
        // Cheap: free first, then the cheapest metered.
        assert_eq!(
            ids(r.plan(Tier::Cheap)),
            ["ollama", "claude-cli", "deepseek-api", "openai-api", "anthropic-api"]
        );
        // Default: plan first, strong APIs, then cheap/local last.
        assert_eq!(
            ids(r.plan(Tier::Default)),
            ["claude-cli", "anthropic-api", "openai-api", "deepseek-api", "ollama"]
        );
        // Hard: strong brains only (no deepseek/ollama).
        assert_eq!(
            ids(r.plan(Tier::Hard)),
            ["claude-cli", "anthropic-api", "openai-api"]
        );
    }

    #[test]
    fn availability_filters_but_keeps_a_safety_net() {
        let r = RouterProvider::new(all(), Arc::new(Only("anthropic-api")));
        assert_eq!(ids(r.plan(Tier::Default)), ["anthropic-api"]);

        // If the registry claims nothing is up, still try (ordered) rather than fail.
        struct None_;
        impl Availability for None_ {
            fn is_available(&self, _: &str) -> bool {
                false
            }
        }
        let r2 = RouterProvider::new(all(), Arc::new(None_));
        assert_eq!(ids(r2.plan(Tier::Default)), ["claude-cli", "anthropic-api", "ollama"]);
    }

    #[tokio::test]
    async fn falls_through_to_a_working_backend() {
        // Default order is claude-cli → anthropic-api → ollama; fail the first.
        let cands = vec![
            cand("ollama", true),
            cand("claude-cli", false),
            cand("anthropic-api", true),
        ];
        let r = RouterProvider::new(cands, always_available());
        let reply = r
            .chat(&ChatRequest {
                system: None,
                messages: vec![ChatMessage::user("hi")],
                tier: Tier::Default,
                max_tokens: 16,
            })
            .await
            .unwrap();
        assert_eq!(reply.text, "anthropic-api");
    }
}
