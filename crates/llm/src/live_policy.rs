//! Shared runtime allowlist. Discovery may add disabled models, but may never
//! grant access or alter routes. Authorization changes require verified approval.
//! This is not an authorization boundary: callers must verify the root broker's
//! signed operation and protected on-disk result before activating a snapshot.

use std::sync::RwLock;

use crate::ModelAccessPolicy;

#[derive(Debug)]
pub struct LiveModelPolicy(RwLock<ModelAccessPolicy>);

impl LiveModelPolicy {
    pub fn new(policy: ModelAccessPolicy) -> Self {
        Self(RwLock::new(policy))
    }

    pub fn snapshot(&self) -> ModelAccessPolicy {
        self.0
            .read()
            .map(|policy| policy.clone())
            .unwrap_or_else(|_| ModelAccessPolicy::deny_by_default())
    }

    pub fn allows(&self, provider: &str, model: &str) -> bool {
        self.0
            .read()
            .is_ok_and(|policy| policy.version == 1 && policy.allows(provider, model))
    }

    /// A broker outcome that cannot be reconciled must never leave stale grants
    /// active. Recovery then requires a verified reload/restart, not guessing.
    pub fn suspend(&self) {
        if let Ok(mut policy) = self.0.write() {
            *policy = ModelAccessPolicy {
                version: 0,
                models: Vec::new(),
            };
        }
    }

    /// Reconcile a verified root-owned discovery snapshot without changing any
    /// authorization or existing route. New entries must be disabled and have
    /// no route override (providers retain their startup route configuration).
    /// A suspended policy can only recover through a trusted restart.
    pub fn refresh_discovery(&self, verified: ModelAccessPolicy) -> Result<(), &'static str> {
        if verified.models.len() > 10_000 {
            return Err("discovered policy too large");
        }
        verified
            .validate()
            .map_err(|_| "invalid discovered policy")?;
        let mut current = self.0.write().map_err(|_| "model policy unavailable")?;
        if current.version != 1 {
            return Err("model policy suspended");
        }
        let previous: std::collections::BTreeMap<_, _> = current
            .models
            .iter()
            .map(|entry| ((entry.provider.as_str(), entry.model.as_str()), entry))
            .collect();
        let incoming: std::collections::BTreeMap<_, _> = verified
            .models
            .iter()
            .map(|entry| ((entry.provider.as_str(), entry.model.as_str()), entry))
            .collect();
        for (key, old) in &previous {
            let new = incoming.get(key).ok_or("discovery removed a model")?;
            if old.enabled != new.enabled || old.route != new.route {
                return Err("discovery changed authorization or routing");
            }
        }
        for (key, new) in &incoming {
            if !previous.contains_key(key) && (new.enabled || new.route.is_some()) {
                return Err("new model is enabled or has a route override");
            }
        }
        *current = verified;
        Ok(())
    }

    /// Compare-and-swap exactly one approved enabled bit. The complete expected
    /// and resulting snapshots are checked, so a route, catalog or concurrent
    /// policy change cannot piggyback on approval for a different operation.
    pub fn activate_verified_toggle(
        &self,
        expected: &ModelAccessPolicy,
        verified: ModelAccessPolicy,
        provider: &str,
        model: &str,
        enabled: bool,
    ) -> Result<(), &'static str> {
        verified
            .validate()
            .map_err(|_| "invalid verified model policy")?;
        let mut desired = expected.clone();
        let entry = desired
            .models
            .iter_mut()
            .find(|entry| entry.provider == provider && entry.model == model)
            .ok_or("model is not discovered")?;
        entry.enabled = enabled;
        if desired != verified {
            return Err("verified policy differs from approved model change");
        }
        let mut current = self.0.write().map_err(|_| "model policy unavailable")?;
        if *current != *expected {
            return Err("active model policy changed");
        }
        *current = verified;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelAccessEntry;

    fn policy() -> ModelAccessPolicy {
        ModelAccessPolicy {
            version: 1,
            models: vec![ModelAccessEntry {
                provider: "huggingface".into(),
                model: "org/fixture".into(),
                enabled: false,
                source: "discovered".into(),
                route: Some("cheapest".into()),
            }],
        }
    }

    #[test]
    fn verified_enable_and_disable_are_immediately_visible_to_shared_readers() {
        let old = policy();
        let live = std::sync::Arc::new(LiveModelPolicy::new(old.clone()));
        let reader = live.clone();
        let mut enabled = old.clone();
        enabled.models[0].enabled = true;
        assert!(!reader.allows("huggingface", "org/fixture"));
        live.activate_verified_toggle(&old, enabled.clone(), "huggingface", "org/fixture", true)
            .unwrap();
        assert!(reader.allows("huggingface", "org/fixture"));
        live.activate_verified_toggle(&enabled, old, "huggingface", "org/fixture", false)
            .unwrap();
        assert!(!reader.allows("huggingface", "org/fixture"));
    }

    #[test]
    fn concurrent_snapshot_and_unsigned_route_changes_are_rejected() {
        let old = policy();
        let live = LiveModelPolicy::new(old.clone());
        let mut enabled = old.clone();
        enabled.models[0].enabled = true;
        let mut changed_route = enabled.clone();
        changed_route.models[0].route = Some("fastest".into());
        assert!(live
            .activate_verified_toggle(&old, changed_route, "huggingface", "org/fixture", true)
            .is_err());
        assert_eq!(live.snapshot(), old);
        live.activate_verified_toggle(&old, enabled, "huggingface", "org/fixture", true)
            .unwrap();
        assert!(live
            .activate_verified_toggle(&old, old.clone(), "huggingface", "org/fixture", false)
            .is_err());
        assert!(live.allows("huggingface", "org/fixture"));
    }

    #[test]
    fn discovery_refresh_preserves_grants_and_allows_a_later_exact_signed_toggle() {
        let live = LiveModelPolicy::new(policy());
        let mut discovered = policy();
        discovered.models.push(ModelAccessEntry {
            provider: "openai-api".into(),
            model: "fixture-new".into(),
            enabled: false,
            source: "provider_api".into(),
            route: None,
        });
        discovered.models.reverse();
        live.refresh_discovery(discovered.clone()).unwrap();
        assert!(!live.allows("openai-api", "fixture-new"));
        let mut enabled = discovered.clone();
        enabled.models[0].enabled = true;
        live.activate_verified_toggle(&discovered, enabled, "openai-api", "fixture-new", true)
            .unwrap();
        assert!(live.allows("openai-api", "fixture-new"));
    }

    #[test]
    fn discovery_never_grants_changes_routes_removes_or_unsuspends() {
        let live = LiveModelPolicy::new(policy());
        let mut grant = policy();
        grant.models[0].enabled = true;
        assert!(live.refresh_discovery(grant).is_err());
        let mut route = policy();
        route.models[0].route = Some("fastest".into());
        assert!(live.refresh_discovery(route).is_err());
        assert!(live
            .refresh_discovery(ModelAccessPolicy::deny_by_default())
            .is_err());
        let mut new_grant = policy();
        new_grant.models.push(ModelAccessEntry {
            provider: "openai-api".into(),
            model: "new".into(),
            enabled: true,
            source: "provider_api".into(),
            route: None,
        });
        assert!(live.refresh_discovery(new_grant).is_err());
        assert_eq!(live.snapshot(), policy());
        live.suspend();
        assert!(live.refresh_discovery(policy()).is_err());
    }
}
