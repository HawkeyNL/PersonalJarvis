//! Owner-defined model routing (`routing.json`, root-owned next to the model
//! policy). It only *orders* candidates and pins models per tier; it never
//! grants access. The allowlist, the monthly cap and health still decide at
//! attempt time. `paid_api: "off"` removes every metered backend.

use std::{collections::BTreeSet, sync::RwLock};

use serde::{Deserialize, Serialize};

use crate::{
    router::{is_metered_backend, is_metered_provider_id},
    Tier,
};

/// Upper bound for the raw document, checked before parsing.
pub const ROUTING_MAX_BYTES: usize = 64 * 1024;

/// Providers a routed chain may name. Keep in sync with the provider list of
/// `jarvis_privileged::Operation::validate` (jarvis-api tests both).
pub const ROUTING_PROVIDERS: [&str; 9] = [
    "anthropic-api",
    "openai-api",
    "deepseek-api",
    "xai-api",
    "zai-api",
    "ollama",
    "ollama-cloud",
    "huggingface",
    "claude-cli",
];

const MAX_CHAIN: usize = 9;
const MAX_MODEL_CHARS: usize = 256;

/// The owner's "turn the paid API route off" switch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaidApi {
    #[default]
    Allowed,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteEntry {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRoute {
    pub chain: Vec<RouteEntry>,
    /// A metered entry after a subscription entry must be explicitly allowed:
    /// a full plan must not silently become a paid call.
    #[serde(default)]
    pub metered_after_subscription: bool,
}

/// A missing tier keeps the built-in order for that tier.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRoutes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cheap: Option<TierRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<TierRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard: Option<TierRoute>,
}

/// Stable on-disk format of `routing.json`, version 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRouting {
    pub version: u32,
    #[serde(default)]
    pub paid_api: PaidApi,
    #[serde(default)]
    pub tiers: TierRoutes,
}

fn is_subscription_backend(provider: &str) -> bool {
    provider == "claude-cli"
}

impl ModelRouting {
    /// Validate raw `routing.json` bytes. The error is a fixed string that
    /// never echoes file content. It does not check the enabled bit: the
    /// runtime allowlist decides per attempt.
    pub fn parse(raw: &[u8]) -> Result<Self, &'static str> {
        if raw.len() > ROUTING_MAX_BYTES {
            return Err("model routing too large");
        }
        let routing: Self = serde_json::from_slice(raw).map_err(|_| "invalid model routing")?;
        routing.validate()?;
        Ok(routing)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("unsupported model routing version");
        }
        for route in [&self.tiers.cheap, &self.tiers.default, &self.tiers.hard]
            .into_iter()
            .flatten()
        {
            if route.chain.is_empty() || route.chain.len() > MAX_CHAIN {
                return Err("model routing chain must have 1 to 9 entries");
            }
            let mut seen = BTreeSet::new();
            let mut after_subscription = false;
            for entry in &route.chain {
                if !ROUTING_PROVIDERS.contains(&entry.provider.as_str()) {
                    return Err("model routing names an unknown provider");
                }
                if entry.model.is_empty()
                    || entry.model.chars().count() > MAX_MODEL_CHARS
                    || entry.model.chars().any(char::is_control)
                {
                    return Err("model routing names an invalid model");
                }
                if !seen.insert((entry.provider.as_str(), entry.model.as_str())) {
                    return Err("model routing chain has a duplicate entry");
                }
                if after_subscription
                    && is_metered_provider_id(&entry.provider)
                    && !route.metered_after_subscription
                {
                    return Err("metered entry after a subscription needs explicit approval");
                }
                after_subscription |= is_subscription_backend(&entry.provider);
            }
        }
        Ok(())
    }

    /// Every routed pair is already discovered (exact match) in the policy.
    /// Routing never grants access; this only keeps unknown ids out.
    pub fn all_discovered(&self, policy: &crate::ModelAccessPolicy) -> bool {
        [&self.tiers.cheap, &self.tiers.default, &self.tiers.hard]
            .into_iter()
            .flatten()
            .flat_map(|route| &route.chain)
            .all(|entry| {
                policy
                    .models
                    .iter()
                    .any(|known| known.provider == entry.provider && known.model == entry.model)
            })
    }

    pub fn tier(&self, tier: Tier) -> Option<&TierRoute> {
        match tier {
            Tier::Cheap => self.tiers.cheap.as_ref(),
            Tier::Default => self.tiers.default.as_ref(),
            Tier::Hard => self.tiers.hard.as_ref(),
        }
    }
}

/// What the router currently routes on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingSnapshot {
    /// `None`: no usable routing file, so the built-in order applies.
    pub routing: Option<ModelRouting>,
    /// Set when `routing.json` exists but could not be used. Routing then fails
    /// closed: built-in order without metered backends.
    pub unavailable_reason: Option<&'static str>,
}

impl RoutingSnapshot {
    pub fn unavailable(reason: &'static str) -> Self {
        Self {
            routing: None,
            unavailable_reason: Some(reason),
        }
    }

    pub fn paid_api_off(&self) -> bool {
        self.unavailable_reason.is_some()
            || self
                .routing
                .as_ref()
                .is_some_and(|routing| routing.paid_api == PaidApi::Off)
    }

    /// A brain pin or explicit provider choice must be refused, not silently
    /// rerouted, when it selects a metered backend while paid APIs are off.
    pub fn refuses_provider(&self, provider: &str) -> bool {
        self.paid_api_off() && is_metered_backend(provider)
    }

    pub fn tier(&self, tier: Tier) -> Option<&TierRoute> {
        self.routing.as_ref().and_then(|routing| routing.tier(tier))
    }
}

/// Shared runtime routing, swapped atomically. Like [`crate::LiveModelPolicy`]
/// this is not an authorization boundary: callers activate only a verified,
/// root-owned document.
#[derive(Debug, Default)]
pub struct LiveRouting(RwLock<RoutingSnapshot>);

impl LiveRouting {
    pub fn new(snapshot: RoutingSnapshot) -> Self {
        Self(RwLock::new(snapshot))
    }

    pub fn snapshot(&self) -> RoutingSnapshot {
        self.0
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| RoutingSnapshot::unavailable("routing_unavailable"))
    }

    /// Compare-and-swap: activate `next` only if nothing changed since
    /// `expected` was read.
    pub fn swap(
        &self,
        expected: &RoutingSnapshot,
        next: RoutingSnapshot,
    ) -> Result<(), &'static str> {
        if let Some(routing) = &next.routing {
            routing.validate()?;
        }
        let mut current = self.0.write().map_err(|_| "model routing unavailable")?;
        if *current != *expected {
            return Err("active model routing changed");
        }
        *current = next;
        Ok(())
    }

    /// An outcome that cannot be verified must never keep a paid route live.
    pub fn fail_closed(&self, reason: &'static str) {
        // A poisoned lock already reads as unavailable.
        if let Ok(mut current) = self.0.write() {
            *current = RoutingSnapshot::unavailable(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{"version":1,"paid_api":"allowed","tiers":{
        "cheap":{"chain":[{"provider":"zai-api","model":"glm-5.3-flash"},
                          {"provider":"claude-cli","model":"claude-haiku-4-5"}],
                 "metered_after_subscription":false},
        "default":{"chain":[{"provider":"claude-cli","model":"claude-sonnet-5"}]},
        "hard":{"chain":[{"provider":"claude-cli","model":"claude-opus-5"}],
                "metered_after_subscription":false}}}"#;

    fn chain(entries: &[(&str, &str)], metered_after_subscription: bool) -> String {
        let chain: Vec<_> = entries
            .iter()
            .map(|(provider, model)| serde_json::json!({"provider": provider, "model": model}))
            .collect();
        serde_json::json!({"version": 1, "tiers": {"default": {
            "chain": chain, "metered_after_subscription": metered_after_subscription}}})
        .to_string()
    }

    fn models(count: usize) -> Vec<(&'static str, &'static str)> {
        ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7", "m8", "m9"][..count]
            .iter()
            .map(|model| ("ollama", *model))
            .collect()
    }

    #[test]
    fn validator_accepts_the_documented_shapes() {
        let routing = ModelRouting::parse(VALID.as_bytes()).unwrap();
        assert_eq!(routing.paid_api, PaidApi::Allowed);
        assert_eq!(routing.tier(Tier::Cheap).unwrap().chain.len(), 2);
        let cases = [
            r#"{"version":1}"#.to_string(),
            r#"{"version":1,"paid_api":"off"}"#.to_string(),
            r#"{"version":1,"tiers":{}}"#.to_string(),
            chain(&[("claude-cli", "a"), ("anthropic-api", "a")], true),
            chain(&[("anthropic-api", "a"), ("claude-cli", "a")], false),
            chain(&[("claude-cli", "a"), ("ollama", "llama3.2")], false),
            chain(&[("claude-cli", "a"), ("claude-cli", "b")], false),
            chain(&[("ollama", &"m".repeat(256))], false),
            chain(&models(9), false),
        ];
        for case in cases {
            assert!(ModelRouting::parse(case.as_bytes()).is_ok(), "{case}");
        }
        let off = ModelRouting::parse(br#"{"version":1,"paid_api":"off"}"#).unwrap();
        assert_eq!(off.paid_api, PaidApi::Off);
        assert!(off.tier(Tier::Default).is_none());
    }

    #[test]
    fn validator_rejects_every_unsafe_or_ambiguous_shape() {
        let cases: Vec<(String, &str)> = vec![
            ("".into(), "empty"),
            ("[]".into(), "not an object"),
            (r#"{"tiers":{}}"#.into(), "missing version"),
            (r#"{"version":2}"#.into(), "unknown version"),
            (r#"{"version":1,"extra":true}"#.into(), "unknown field"),
            (r#"{"version":1,"paid_api":"maybe"}"#.into(), "unknown switch"),
            (r#"{"version":1,"paid_api":"OFF"}"#.into(), "case"),
            (r#"{"version":1,"version":1}"#.into(), "duplicate key"),
            (
                r#"{"version":1,"tiers":{"turbo":{"chain":[]}}}"#.into(),
                "unknown tier",
            ),
            (
                r#"{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama","model":"a","x":1}]}}}"#
                    .into(),
                "unknown entry field",
            ),
            (
                r#"{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama"}]}}}"#.into(),
                "missing model",
            ),
            (
                r#"{"version":1,"tiers":{"default":{"chain":[],"y":1}}}"#.into(),
                "unknown tier field",
            ),
            (chain(&[], false), "empty chain"),
            (chain(&models(10), false), "ten entries"),
            (chain(&[("jev", "a")], false), "provider not routable"),
            (chain(&[("Claude-CLI", "a")], false), "provider case"),
            (chain(&[("ollama", "")], false), "empty model"),
            (chain(&[("ollama", &"m".repeat(257))], false), "long model"),
            (chain(&[("ollama", "a\nb")], false), "control char"),
            (chain(&[("ollama", "a"), ("ollama", "a")], false), "duplicate"),
            (
                chain(&[("claude-cli", "a"), ("anthropic-api", "a")], false),
                "metered after subscription",
            ),
            (
                chain(&[("claude-cli", "a"), ("ollama", "b"), ("zai-api", "c")], false),
                "metered after subscription via local",
            ),
            (
                format!(r#"{{"version":1,"pad":"{}"}}"#, " ".repeat(ROUTING_MAX_BYTES)),
                "too large",
            ),
        ];
        for (raw, why) in cases {
            assert!(ModelRouting::parse(raw.as_bytes()).is_err(), "{why}");
        }
        assert_eq!(
            ModelRouting::parse(" ".repeat(ROUTING_MAX_BYTES + 1).as_bytes()),
            Err("model routing too large")
        );
    }

    #[test]
    fn routed_pairs_must_be_discovered_exactly() {
        let routing = ModelRouting::parse(VALID.as_bytes()).unwrap();
        let entry = |provider: &str, model: &str| crate::ModelAccessEntry {
            provider: provider.into(),
            model: model.into(),
            enabled: false,
            source: "fixture".into(),
            route: None,
        };
        let mut policy = crate::ModelAccessPolicy {
            version: 1,
            models: vec![
                entry("zai-api", "glm-5.3-flash"),
                entry("claude-cli", "claude-haiku-4-5"),
                entry("claude-cli", "claude-sonnet-5"),
                entry("claude-cli", "claude-opus-5"),
            ],
        };
        assert!(routing.all_discovered(&policy));
        policy.models[3].model = "Claude-Opus-5".into();
        assert!(!routing.all_discovered(&policy));
        policy.models.pop();
        assert!(!routing.all_discovered(&policy));
        let empty = ModelRouting::parse(br#"{"version":1}"#).unwrap();
        assert!(empty.all_discovered(&policy));
    }

    #[test]
    fn unavailable_routing_turns_paid_api_off() {
        assert!(!RoutingSnapshot::default().paid_api_off());
        let failed = RoutingSnapshot::unavailable("routing_invalid");
        assert!(failed.paid_api_off());
        assert!(failed.refuses_provider("openai-api"));
        assert!(!failed.refuses_provider("claude-cli"));
        assert!(!failed.refuses_provider("ollama"));
        assert!(failed.tier(Tier::Default).is_none());
    }

    #[test]
    fn swap_is_compare_and_set_and_validates() {
        let live = LiveRouting::default();
        let old = live.snapshot();
        let next = RoutingSnapshot {
            routing: Some(ModelRouting::parse(VALID.as_bytes()).unwrap()),
            unavailable_reason: None,
        };
        let mut invalid = next.clone();
        invalid.routing.as_mut().unwrap().version = 2;
        assert!(live.swap(&old, invalid).is_err());
        live.swap(&old, next.clone()).unwrap();
        assert_eq!(live.snapshot(), next);
        assert!(live.swap(&old, old.clone()).is_err());
        assert_eq!(live.snapshot(), next);
        live.fail_closed("routing_activation_unverified");
        assert!(live.snapshot().paid_api_off());
        assert!(live.snapshot().routing.is_none());
    }
}
