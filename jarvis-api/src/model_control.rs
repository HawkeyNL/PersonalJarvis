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

/// What `routing.json` holds right now. The hash covers the exact bytes even
/// when they are invalid, so a signed full replacement can repair the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingFile {
    pub sha256: String,
    /// `Ok(None)`: absent, the built-in order applies.
    pub routing: Result<Option<ModelRouting>, &'static str>,
}

impl RoutingFile {
    pub fn from_bytes(raw: Option<&[u8]>) -> Self {
        Self {
            sha256: routing_sha256(raw.unwrap_or_default()),
            routing: raw
                .map(|raw| ModelRouting::parse(raw).map_err(|_| "routing_invalid"))
                .transpose(),
        }
    }
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
    // A truncated read cannot yield the state hash the broker computes.
    if raw.len() > ROUTING_MAX_BYTES {
        return Err("routing_too_large");
    }
    Ok(Some(raw))
}

/// Current `routing.json` state. `Err` only when it cannot be read safely.
pub fn load_routing(path: &Path) -> Result<RoutingFile, &'static str> {
    read_routing_bytes(path).map(|raw| RoutingFile::from_bytes(raw.as_deref()))
}

/// What the router should use at startup: missing keeps the built-in order;
/// anything unusable fails closed (built-in order without metered backends).
pub fn startup_routing(path: &Path) -> RoutingSnapshot {
    match load_routing(path).and_then(|file| file.routing) {
        Ok(routing) => {
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
/// hash is published whenever the file is readable, even when it is invalid or
/// not active, so a signed full replacement can repair it.
pub(crate) fn routing_status(
    live: &RoutingSnapshot,
    disk: Result<RoutingFile, &'static str>,
) -> (Option<&'static str>, Option<String>) {
    let reason = live.unavailable_reason.or(match &disk {
        Ok(RoutingFile {
            routing: Ok(stored),
            ..
        }) if *stored == live.routing => None,
        _ => Some("routing_reload_required"),
    });
    (reason, disk.ok().map(|file| file.sha256))
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

    pub(crate) fn read_routing(&self) -> Result<RoutingFile, &'static str> {
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
        let file = load_routing(&path).unwrap();
        assert_eq!(file.routing, Ok(None));
        assert_eq!(
            file.sha256,
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
    fn routing_hash_is_published_whenever_the_file_is_readable() {
        let off_bytes = br#"{"version":1,"paid_api":"off"}"#;
        let off = ModelRouting::parse(off_bytes).unwrap();
        let active = RoutingSnapshot {
            routing: Some(off.clone()),
            unavailable_reason: None,
        };
        let stored = RoutingFile::from_bytes(Some(off_bytes));
        assert_eq!(stored.sha256, routing_sha256(off_bytes));
        assert_eq!(
            routing_status(&active, Ok(stored.clone())),
            (None, Some(stored.sha256.clone()))
        );
        let absent = RoutingFile::from_bytes(None);
        assert_eq!(absent.sha256, routing_sha256(b""));
        assert_eq!(
            routing_status(&RoutingSnapshot::default(), Ok(absent.clone())),
            (None, Some(absent.sha256.clone()))
        );
        // Disk differs from the active routing: still hashed, never silent.
        assert_eq!(
            routing_status(&active, Ok(absent.clone())),
            (Some("routing_reload_required"), Some(absent.sha256))
        );
        // Unreadable or unsafe: no state to bind a signed change to.
        assert_eq!(
            routing_status(&active, Err("routing_unsafe")),
            (Some("routing_reload_required"), None)
        );
        let failed = RoutingSnapshot::unavailable("routing_unsafe");
        assert_eq!(
            routing_status(&failed, Ok(stored)),
            (Some("routing_unsafe"), Some(routing_sha256(off_bytes)))
        );
    }

    #[test]
    fn invalid_routing_bytes_are_hashed_so_a_signed_replacement_can_repair_them() {
        let broken = br#"{"version":1,"paid_api":"maybe"}"#;
        let file = RoutingFile::from_bytes(Some(broken));
        assert_eq!(file.routing, Err("routing_invalid"));
        assert_eq!(file.sha256, routing_sha256(broken));
        let live = RoutingSnapshot::unavailable("routing_invalid");
        assert_eq!(
            routing_status(&live, Ok(file)),
            (Some("routing_invalid"), Some(routing_sha256(broken)))
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
