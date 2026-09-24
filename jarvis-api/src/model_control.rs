//! Protected policy readback and serialization of signed model mutations.
//! Paths come from trusted startup configuration, never HTTP request data.

use jarvis_llm::ModelAccessPolicy;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

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

pub struct ModelControl {
    path: Option<PathBuf>,
    unavailable_hf_routes: std::collections::BTreeSet<String>,
    pub(crate) mutation: tokio::sync::Mutex<()>,
}

impl ModelControl {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            unavailable_hf_routes: Default::default(),
            mutation: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_unavailable_hf_routes(mut self, models: impl IntoIterator<Item = String>) -> Self {
        self.unavailable_hf_routes.extend(models);
        self
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
