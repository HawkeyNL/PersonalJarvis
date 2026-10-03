//! Protected policy readback and serialization of signed model mutations.
//! Paths come from trusted startup configuration, never HTTP request data.

use jarvis_llm::{ModelAccessPolicy, ModelRouting, RoutingSnapshot, ROUTING_MAX_BYTES};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{ErrorKind, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// State hash of `routing.json`: SHA-256 (hex) of its exact bytes, or of empty
/// bytes when the file is absent.
pub fn routing_sha256(raw: &[u8]) -> String {
    hex::encode(Sha256::digest(raw))
}

/// Read the root-owned `routing.json` without following links. `Ok(None)`
/// means the file is absent. Errors are stable, non-sensitive reason codes.
pub fn read_routing_bytes(path: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        // O_NOFOLLOW reports a symlink as ELOOP.
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Err("routing_unsafe"),
        Err(_) => return Err("routing_unreadable"),
    };
    let metadata = file.metadata().map_err(|_| "routing_unreadable")?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("routing_unsafe");
    }
    let mut raw = Vec::new();
    file.take(ROUTING_MAX_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "routing_unreadable")?;
    Ok(Some(raw))
}

/// Validated routing document (or `None` when absent) and its state hash.
pub fn load_routing(path: &Path) -> Result<(Option<ModelRouting>, String), &'static str> {
    match read_routing_bytes(path)? {
        None => Ok((None, routing_sha256(&[]))),
        Some(raw) => {
            let routing = ModelRouting::parse(&raw).map_err(|_| "routing_invalid")?;
            Ok((Some(routing), routing_sha256(&raw)))
        }
    }
}

/// What the router should use at startup: missing keeps the built-in order;
/// anything unusable fails closed (built-in order without metered backends).
pub fn startup_routing(path: &Path) -> RoutingSnapshot {
    match load_routing(path) {
        Ok((routing, _)) => {
            if let Some(routing) = &routing {
                tracing::info!(paid_api = ?routing.paid_api, "model routing loaded");
            }
            RoutingSnapshot {
                routing,
                unavailable_reason: None,
            }
        }
        Err(reason) => {
            tracing::warn!(
                path = %path.display(),
                reason,
                "model routing unusable; using built-in order without paid APIs"
            );
            RoutingSnapshot::unavailable(reason)
        }
    }
}

/// Presentation only: never serialize the pricing registry into the signed
/// policy, or substitute the accounting fallback for an actual quoted price.
pub(crate) fn priced_models(
    policy: &ModelAccessPolicy,
    registry: &jarvis_usage::PricingRegistry,
) -> Vec<serde_json::Value> {
    policy
        .models
        .iter()
        .map(|model| {
            let price = registry
                .models
                .iter()
                .find(|price| price.provider == model.provider && price.model == model.model);
            let mut row = serde_json::to_value(model).expect("model entry is serializable");
            row["price_status"] = serde_json::json!(price
                .map(|p| p.price_status)
                .unwrap_or(jarvis_usage::PriceStatus::Unknown));
            row["input_per_million_usd"] =
                serde_json::json!(price.map(|p| p.input_per_million_usd));
            row["output_per_million_usd"] =
                serde_json::json!(price.map(|p| p.output_per_million_usd));
            row["cache_read_per_million_usd"] =
                serde_json::json!(price.and_then(|p| p.cache_read_per_million_usd));
            row["pricing_notes"] = serde_json::json!(price.and_then(|p| p.pricing_notes.as_ref()));
            row["long_context"] = serde_json::json!(price.and_then(|p| p.long_context.as_ref()));
            row["pricing_source"] = serde_json::json!(
                price.map(|p| p.pricing_source.as_ref().unwrap_or(&registry.source))
            );
            row["pricing_updated_at"] = serde_json::json!(price.map(|p| p
                .pricing_updated_at
                .as_ref()
                .unwrap_or(&registry.updated_at)));
            row
        })
        .collect()
}

/// `(routing_unavailable_reason, routing_sha256)` for the owner view. The
/// hash is published only when the live routing is exactly what is on disk,
/// so a later signed change binds to the active state.
pub(crate) fn routing_status(
    live: &RoutingSnapshot,
    disk: Result<(Option<ModelRouting>, String), &'static str>,
) -> (Option<&'static str>, Option<String>) {
    let reason = live.unavailable_reason.or(match &disk {
        Ok((stored, _)) if *stored == live.routing => None,
        _ => Some("routing_reload_required"),
    });
    let hash = disk.ok().filter(|_| reason.is_none()).map(|(_, hash)| hash);
    (reason, hash)
}

pub struct ModelControl {
    path: Option<PathBuf>,
    routing_path: Option<PathBuf>,
    unavailable_hf_routes: std::collections::BTreeSet<String>,
    pub(crate) mutation: tokio::sync::Mutex<()>,
}

impl ModelControl {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            routing_path: None,
            unavailable_hf_routes: Default::default(),
            mutation: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_unavailable_hf_routes(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.unavailable_hf_routes.extend(models);
        self
    }

    pub fn with_routing_path(mut self, path: PathBuf) -> Self {
        self.routing_path = Some(path);
        self
    }

    pub(crate) fn read_routing(&self) -> Result<(Option<ModelRouting>, String), &'static str> {
        load_routing(self.routing_path.as_deref().ok_or("routing_unavailable")?)
    }

    pub(crate) fn can_enable(&self, provider: &str, model: &str) -> bool {
        provider != "huggingface" || !self.unavailable_hf_routes.contains(model)
    }

    pub(crate) fn read(&self) -> Result<(ModelAccessPolicy, String), &'static str> {
        let path = self.path.as_ref().ok_or("model control unavailable")?;
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "model policy unavailable")?;
        let metadata = file.metadata().map_err(|_| "model policy unavailable")?;
        const LIMIT: u64 = 8 * 1024 * 1024;
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() > LIMIT
        {
            return Err("unsafe model policy");
        }
        let mut raw = Vec::new();
        file.take(LIMIT + 1)
            .read_to_end(&mut raw)
            .map_err(|_| "model policy unavailable")?;
        if raw.len() as u64 > LIMIT {
            return Err("model policy too large");
        }
        let policy: ModelAccessPolicy =
            serde_json::from_slice(&raw).map_err(|_| "invalid model policy")?;
        policy.validate().map_err(|_| "invalid model policy")?;
        Ok((policy, hex::encode(Sha256::digest(raw))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_control_has_no_caller_selected_file() {
        assert!(ModelControl::new(None).read().is_err());
        assert!(ModelControl::new(None).read_routing().is_err());
    }

    #[test]
    fn missing_routing_keeps_builtin_order_and_hashes_empty_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("routing.json");
        let (routing, hash) = load_routing(&path).unwrap();
        assert!(routing.is_none());
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let snapshot = startup_routing(&path);
        assert_eq!(snapshot, RoutingSnapshot::default());
        assert!(!snapshot.paid_api_off());
    }

    #[test]
    fn unsafe_routing_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let writable = dir.path().join("writable.json");
        fs::write(&writable, br#"{"version":1}"#).unwrap();
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o664)).unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&writable, &link).unwrap();
        let owned = dir.path().join("owned.json");
        fs::write(&owned, br#"{"version":1}"#).unwrap();
        let mut cases = vec![writable.as_path(), link.as_path(), dir.path()];
        // Rootless fixture: a file owned by the test user is not root-owned.
        if fs::metadata(&owned).unwrap().uid() != 0 {
            cases.push(owned.as_path());
        }
        for path in cases {
            let reason = "routing_unsafe";
            assert_eq!(load_routing(path), Err(reason));
            let snapshot = startup_routing(path);
            assert_eq!(snapshot.unavailable_reason, Some(reason));
            assert!(snapshot.routing.is_none());
            assert!(snapshot.refuses_provider("anthropic-api"));
            assert!(!snapshot.refuses_provider("claude-cli"));
        }
    }

    #[test]
    fn routing_hash_is_published_only_for_the_active_routing() {
        let off = ModelRouting::parse(br#"{"version":1,"paid_api":"off"}"#).unwrap();
        let active = RoutingSnapshot {
            routing: Some(off.clone()),
            unavailable_reason: None,
        };
        let hash = routing_sha256(b"fixture");
        assert_eq!(
            routing_status(&active, Ok((Some(off.clone()), hash.clone()))),
            (None, Some(hash.clone()))
        );
        assert_eq!(
            routing_status(&RoutingSnapshot::default(), Ok((None, hash.clone()))),
            (None, Some(hash.clone()))
        );
        for disk in [Ok((None, hash.clone())), Err("routing_invalid")] {
            assert_eq!(
                routing_status(&active, disk),
                (Some("routing_reload_required"), None)
            );
        }
        let failed = RoutingSnapshot::unavailable("routing_unsafe");
        assert_eq!(
            routing_status(&failed, Ok((Some(off), hash))),
            (Some("routing_unsafe"), None)
        );
    }

    #[test]
    fn routing_hash_binds_exact_bytes() {
        assert_ne!(
            routing_sha256(br#"{"version":1}"#),
            routing_sha256(br#"{"version":1} "#)
        );
        assert_eq!(routing_sha256(b"").len(), 64);
    }

    #[test]
    fn routing_providers_match_the_signed_broker_operation() {
        let operation = |provider: &str| jarvis_privileged::Operation::ModelSetEnabled {
            provider: provider.into(),
            model: "fixture".into(),
            enabled: true,
            expected_policy_sha256: "00".repeat(32),
        };
        for provider in jarvis_llm::ROUTING_PROVIDERS {
            assert!(operation(provider).validate().is_ok(), "{provider}");
        }
        for provider in ["jev", "codex", "unknown", "Claude-CLI"] {
            assert!(operation(provider).validate().is_err(), "{provider}");
        }
    }

    #[test]
    fn prices_are_exact_provider_model_matches_and_never_unknown_zeroes() {
        let registry = jarvis_usage::PricingRegistry::builtin();
        let known = &registry.models[0];
        let mut policy = ModelAccessPolicy {
            version: 1,
            models: vec![jarvis_llm::ModelAccessEntry {
                provider: known.provider.clone(),
                model: known.model.clone(),
                enabled: false,
                source: "provider_api".into(),
                route: None,
            }],
        };
        let original = policy.clone();
        let rows = priced_models(&policy, &registry);
        assert_eq!(
            rows[0]["input_per_million_usd"],
            known.input_per_million_usd
        );
        assert_eq!(policy, original);
        policy.models[0].model.push_str("-not-an-alias");
        let rows = priced_models(&policy, &registry);
        assert_eq!(rows[0]["price_status"], "unknown");
        assert!(rows[0]["input_per_million_usd"].is_null());
        assert!(rows[0]["output_per_million_usd"].is_null());
    }
}
