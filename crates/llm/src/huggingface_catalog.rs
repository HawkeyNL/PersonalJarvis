//! Bounded, non-secret Hugging Face discovery metadata.

use std::{collections::BTreeSet, fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_CATALOG_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_MODELS: usize = 2_000;
pub const MAX_PROVIDERS_PER_MODEL: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HuggingFaceCatalog {
    pub version: u32,
    pub discovered_at: String,
    pub models: Vec<HuggingFaceModel>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HuggingFaceModel {
    pub id: String,
    #[serde(default)]
    pub providers: Vec<HuggingFaceProviderMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HuggingFaceProviderMetadata {
    pub provider: String,
    pub status: String,
    pub context_length: Option<u64>,
    pub input_per_million_usd: Option<f64>,
    pub output_per_million_usd: Option<f64>,
    pub supports_tools: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub first_token_latency_ms: Option<f64>,
    pub throughput: Option<f64>,
}

impl HuggingFaceCatalog {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let metadata = fs::metadata(path.as_ref())
            .map_err(|error| format!("Hugging Face catalog is unavailable: {error}"))?;
        if metadata.len() > MAX_CATALOG_BYTES {
            return Err("Hugging Face catalog exceeds the size limit".into());
        }
        let raw = fs::read(path.as_ref())
            .map_err(|error| format!("Hugging Face catalog is unavailable: {error}"))?;
        let catalog: Self = serde_json::from_slice(&raw)
            .map_err(|error| format!("Hugging Face catalog is malformed: {error}"))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn from_api_response(value: &Value, discovered_at: impl Into<String>) -> Self {
        let mut models = Vec::new();
        for raw_model in value
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(MAX_MODELS)
        {
            let Some(id) = safe_string(raw_model.get("id"), 256) else {
                continue;
            };
            if models.iter().any(|model: &HuggingFaceModel| model.id == id) {
                continue;
            }
            let providers = raw_model
                .get("providers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(MAX_PROVIDERS_PER_MODEL)
                .filter_map(parse_provider)
                .collect();
            models.push(HuggingFaceModel { id, providers });
        }
        Self {
            version: 1,
            discovered_at: discovered_at.into(),
            models,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.discovered_at.is_empty()
            || self.discovered_at.len() > 64
            || self.discovered_at.chars().any(char::is_control)
            || self.models.len() > MAX_MODELS
        {
            return Err("invalid Hugging Face catalog header".into());
        }
        let mut model_ids = BTreeSet::new();
        for model in &self.models {
            if !safe_text(&model.id, 256) || model.providers.len() > MAX_PROVIDERS_PER_MODEL {
                return Err("invalid Hugging Face catalog model".into());
            }
            if !model_ids.insert(&model.id) {
                return Err("duplicate Hugging Face catalog model".into());
            }
            let mut provider_ids = BTreeSet::new();
            for provider in &model.providers {
                crate::validate_hf_route(&provider.provider)?;
                if !provider_ids.insert(&provider.provider) {
                    return Err("duplicate Hugging Face provider metadata".into());
                }
                if !safe_text(&provider.status, 32)
                    || provider
                        .context_length
                        .is_some_and(|value| value > 100_000_000)
                    || [
                        provider.input_per_million_usd,
                        provider.output_per_million_usd,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|value| !valid_price(value))
                    || [provider.first_token_latency_ms, provider.throughput]
                        .into_iter()
                        .flatten()
                        .any(|value| !valid_metric(value, 1_000_000_000.0))
                {
                    return Err("invalid Hugging Face provider metadata".into());
                }
            }
        }
        Ok(())
    }

    pub fn model(&self, id: &str) -> Option<&HuggingFaceModel> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn route_available(&self, model: &str, route: &str) -> bool {
        if matches!(route, "auto" | "fastest" | "cheapest" | "preferred") {
            return self.model(model).is_some();
        }
        self.model(model).is_some_and(|model| {
            model
                .providers
                .iter()
                .any(|provider| provider.provider == route && provider.status == "live")
        })
    }

    /// A budget-safe price. Dynamic routes use the maximum complete live
    /// provider price. Missing prices return `None`, causing the existing
    /// conservative unknown-model ceiling to remain in force.
    pub fn conservative_price(&self, model: &str, route: &str) -> Option<(f64, f64)> {
        let providers = &self.model(model)?.providers;
        if !matches!(route, "auto" | "fastest" | "cheapest" | "preferred") {
            let provider = providers
                .iter()
                .find(|provider| provider.provider == route && provider.status == "live")?;
            return Some((
                provider.input_per_million_usd?,
                provider.output_per_million_usd?,
            ));
        }
        let live: Vec<_> = providers
            .iter()
            .filter(|provider| provider.status == "live")
            .collect();
        if live.is_empty()
            || live.iter().any(|provider| {
                provider.input_per_million_usd.is_none()
                    || provider.output_per_million_usd.is_none()
            })
        {
            return None;
        }
        Some(
            live.into_iter()
                .fold((0.0_f64, 0.0_f64), |prices, provider| {
                    (
                        prices
                            .0
                            .max(provider.input_per_million_usd.unwrap_or_default()),
                        prices
                            .1
                            .max(provider.output_per_million_usd.unwrap_or_default()),
                    )
                }),
        )
    }

    /// Conservative ceiling across every tier route that may serve a model.
    /// If any route cannot be bounded, the combined result is unknown.
    pub fn conservative_price_for_routes<'a>(
        &self,
        model: &str,
        routes: impl IntoIterator<Item = &'a str>,
    ) -> Option<(f64, f64)> {
        let mut found = false;
        let mut maximum = (0.0_f64, 0.0_f64);
        for route in routes {
            let price = self.conservative_price(model, route)?;
            maximum.0 = maximum.0.max(price.0);
            maximum.1 = maximum.1.max(price.1);
            found = true;
        }
        found.then_some(maximum)
    }
}

fn parse_provider(value: &Value) -> Option<HuggingFaceProviderMetadata> {
    let provider = safe_string(value.get("provider"), 64)?;
    crate::validate_hf_route(&provider).ok()?;
    let status = safe_string(value.get("status"), 32).unwrap_or_else(|| "unknown".into());
    let pricing = value.get("pricing");
    Some(HuggingFaceProviderMetadata {
        provider,
        status,
        context_length: value
            .get("context_length")
            .and_then(Value::as_u64)
            .filter(|v| *v <= 100_000_000),
        input_per_million_usd: price_metric(pricing.and_then(|p| p.get("input"))),
        output_per_million_usd: price_metric(pricing.and_then(|p| p.get("output"))),
        supports_tools: value.get("supports_tools").and_then(Value::as_bool),
        supports_structured_output: value
            .get("supports_structured_output")
            .and_then(Value::as_bool),
        first_token_latency_ms: metric(value.get("first_token_latency_ms"), 1_000_000_000.0),
        throughput: metric(value.get("throughput"), 1_000_000_000.0),
    })
}

fn safe_string(value: Option<&Value>, limit: usize) -> Option<String> {
    let value = value?.as_str()?;
    safe_text(value, limit).then(|| value.to_owned())
}

fn safe_text(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn metric(value: Option<&Value>, maximum: f64) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| valid_metric(*value, maximum))
}

fn price_metric(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| valid_price(*value))
}

fn valid_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= 1_000_000.0
}

fn valid_metric(value: f64, maximum: f64) -> bool {
    value.is_finite() && value >= 0.0 && value <= maximum
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_multiple_providers_and_skips_malformed_records() {
        let catalog = HuggingFaceCatalog::from_api_response(
            &json!({"data":[
                {"id":"openai/gpt-oss-20b","providers":[
                    {"provider":"groq","status":"live","context_length":131072,"pricing":{"input":0.1,"output":0.2},"supports_tools":true},
                    {"provider":"deepinfra","status":"live","supports_structured_output":false},
                    {"provider":"zero-price","status":"live","pricing":{"input":0,"output":0}},
                    {"provider":"bad/route","status":"live"}
                ]},
                {"id":"bad\nmodel"}
            ]}),
            "fixture",
        );
        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].providers.len(), 3);
        assert_eq!(catalog.models[0].providers[1].input_per_million_usd, None);
        assert_eq!(catalog.models[0].providers[2].input_per_million_usd, None);
        assert!(catalog
            .conservative_price("openai/gpt-oss-20b", "fastest")
            .is_none());
        assert_eq!(
            catalog.conservative_price("openai/gpt-oss-20b", "groq"),
            Some((0.1, 0.2))
        );
        assert!(catalog.route_available("openai/gpt-oss-20b", "groq"));
        assert!(!catalog.route_available("openai/gpt-oss-20b", "novita"));
    }

    #[test]
    fn bounds_models_and_providers() {
        let provider = json!({"provider":"groq","status":"live"});
        let models: Vec<_> = (0..MAX_MODELS + 20)
            .map(|index| json!({"id":format!("org/model-{index}"),"providers":vec![provider.clone(); MAX_PROVIDERS_PER_MODEL + 10]}))
            .collect();
        let catalog = HuggingFaceCatalog::from_api_response(&json!({"data":models}), "fixture");
        assert_eq!(catalog.models.len(), MAX_MODELS);
        assert_eq!(catalog.models[0].providers.len(), MAX_PROVIDERS_PER_MODEL);
    }

    #[test]
    fn dynamic_routes_use_the_highest_complete_live_price() {
        let catalog = HuggingFaceCatalog::from_api_response(
            &json!({"data":[{"id":"org/model","providers":[
                {"provider":"groq","status":"live","pricing":{"input":0.1,"output":0.2}},
                {"provider":"novita","status":"live","pricing":{"input":0.3,"output":0.4}},
                {"provider":"offline","status":"error"}
            ]}]}),
            "fixture",
        );
        assert_eq!(
            catalog.conservative_price("org/model", "fastest"),
            Some((0.3, 0.4))
        );
        assert_eq!(
            catalog.conservative_price("org/model", "cheapest"),
            Some((0.3, 0.4))
        );
        assert_eq!(
            catalog.conservative_price_for_routes("org/model", ["groq", "novita"]),
            Some((0.3, 0.4))
        );
    }
}
