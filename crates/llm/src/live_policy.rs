//! Shared runtime allowlist. Only enable/disable changes can be activated here;
//! provider routes and catalog changes still need a complete provider rebuild.
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
}
