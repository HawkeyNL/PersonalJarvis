//! Owner-controlled model access policy.
//!
//! Credentials prove that Core *can* authenticate to a provider.  They are not
//! an authorization grant for every model a provider happens to expose.  This
//! small, file-backed policy is deliberately separate from credentials so it
//! can be managed by a root-operated tool and read by the unprivileged Core.

use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAccessEntry {
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    /// `local`, `configured`, or `discovered`.  Informational only: routing
    /// always uses the explicit `enabled` bit.
    #[serde(default = "default_source")]
    pub source: String,
    /// Hugging Face execution route. This is deliberately separate from the
    /// base model identity used by the owner allowlist. Legacy policies omit
    /// the field and remain valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

fn default_source() -> String {
    "configured".to_string()
}

/// Stable on-disk format owned by the administrator, never by Core.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAccessPolicy {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub models: Vec<ModelAccessEntry>,
}

fn default_version() -> u32 {
    1
}

impl ModelAccessPolicy {
    /// No remote model is implicitly allowed.  Local and subscription-backed
    /// models are available only when explicitly represented as well; this
    /// makes an absent policy safe on a public Home Node.
    pub fn deny_by_default() -> Self {
        Self {
            version: 1,
            models: Vec::new(),
        }
    }

    /// Parse a root-managed JSON policy.  The caller chooses whether a missing
    /// file is fatal; malformed files must never become a permissive policy.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let raw = fs::read_to_string(path.as_ref())
            .map_err(|e| format!("model access policy is unavailable: {e}"))?;
        let policy: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("model access policy is malformed: {e}"))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported model access policy version".into());
        }
        let mut seen = BTreeMap::new();
        for entry in &self.models {
            if !valid_component(&entry.provider) || entry.model.trim().is_empty() {
                return Err("model access policy contains an invalid provider or model".into());
            }
            if entry.route.is_some() && !entry.provider.eq_ignore_ascii_case("huggingface") {
                return Err("model routes are supported only for huggingface".into());
            }
            if let Some(route) = &entry.route {
                validate_hf_route(route)?;
            }
            let key = (entry.provider.to_ascii_lowercase(), entry.model.clone());
            if seen.insert(key, ()).is_some() {
                return Err("model access policy contains duplicate provider/model entries".into());
            }
        }
        Ok(())
    }

    /// Exact provider/model match only.  Provider aliases and similarly named
    /// replacement models cannot inherit access accidentally.
    pub fn allows(&self, provider: &str, model: &str) -> bool {
        match self.state(provider, model) {
            Some(enabled) => enabled,
            // Local Ollama has no credential and remains a useful offline
            // safe default.  An explicit disabled entry still wins.
            None => provider.eq_ignore_ascii_case("ollama"),
        }
    }

    pub fn state(&self, provider: &str, model: &str) -> Option<bool> {
        self.models
            .iter()
            .find(|entry| entry.provider.eq_ignore_ascii_case(provider) && entry.model == model)
            .map(|entry| entry.enabled)
    }

    pub fn route(&self, provider: &str, model: &str) -> Option<&str> {
        self.models
            .iter()
            .find(|entry| entry.provider.eq_ignore_ascii_case(provider) && entry.model == model)
            .and_then(|entry| entry.route.as_deref())
    }
}

/// Reserved policies plus discovered infrastructure provider ids. Provider
/// ids are data, never paths, URLs, shell fragments, or model identifiers.
pub fn validate_hf_route(value: &str) -> Result<(), String> {
    if matches!(value, "auto" | "fastest" | "cheapest" | "preferred") {
        return Ok(());
    }
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
    {
        return Err("invalid Hugging Face inference provider route".into());
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_and_similar_models_never_inherit_access() {
        let policy = ModelAccessPolicy {
            version: 1,
            models: vec![ModelAccessEntry {
                provider: "openai-api".into(),
                model: "gpt-4.1".into(),
                enabled: true,
                source: "configured".into(),
                route: None,
            }],
        };
        assert!(policy.allows("openai-api", "gpt-4.1"));
        assert!(!policy.allows("openai-api", "gpt-4.1-latest"));
        assert!(!policy.allows("xai-api", "gpt-4.1"));
    }

    #[test]
    fn duplicate_entries_fail_closed() {
        let mut policy = ModelAccessPolicy::deny_by_default();
        policy.models = vec![
            ModelAccessEntry {
                provider: "ollama".into(),
                model: "qwen".into(),
                enabled: true,
                source: "local".into(),
                route: None,
            },
            ModelAccessEntry {
                provider: "ollama".into(),
                model: "qwen".into(),
                enabled: false,
                source: "local".into(),
                route: None,
            },
        ];
        assert!(policy.validate().is_err());
    }

    #[test]
    fn route_is_separate_and_strictly_validated() {
        assert!(validate_hf_route("fastest").is_ok());
        assert!(validate_hf_route("deepinfra").is_ok());
        for unsafe_value in ["Groq", "a/b", "https://route", "a b", "a\n"] {
            assert!(validate_hf_route(unsafe_value).is_err());
        }
    }

    #[test]
    fn legacy_policy_without_route_remains_readable() {
        let policy: ModelAccessPolicy = serde_json::from_str(
            r#"{"version":1,"models":[{"provider":"openai-api","model":"gpt","enabled":true,"source":"configured"}]}"#,
        )
        .unwrap();
        assert!(policy.validate().is_ok());
        assert_eq!(policy.models[0].route, None);
    }
}
