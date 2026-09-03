//! Read-only pricing and usage projections for CLI and graphical administration.
//!
//! This module deliberately reads only fixed, root-controlled paths and emits
//! bounded aggregate telemetry. It never reads prompts, replies, credentials,
//! request identifiers, or a caller-selected file.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{ModelPolicy, CURRENT_RELEASE, RELEASES_ROOT};

const OWNER_PRICING_REGISTRY: &str = "/etc/jarvis/pricing-registry.json";
const RELEASE_PRICING_REGISTRY: &str = "pricing-registry.json";
const USAGE_SUMMARY: &str = "/var/lib/jarvis/usage-summary.json";
const HUGGINGFACE_CATALOG: &str = "/etc/jarvis/huggingface-catalog.json";
const MAX_DOCUMENT_SIZE: u64 = 512 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct PricingRegistry {
    version: u32,
    source: String,
    updated_at: String,
    models: Vec<PricingEntry>,
}

#[derive(Clone, Debug, Deserialize)]
struct PricingEntry {
    provider: String,
    model: String,
    input_per_million_usd: f64,
    output_per_million_usd: f64,
    #[serde(default)]
    cache_read_per_million_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PricedModelRecord {
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) enabled: bool,
    pub(super) source: String,
    pub(super) route: Option<String>,
    pub(super) price_status: &'static str,
    pub(super) input_per_million_usd: Option<f64>,
    pub(super) cache_read_per_million_usd: Option<f64>,
    pub(super) output_per_million_usd: Option<f64>,
    pub(super) pricing_source: String,
    pub(super) pricing_updated_at: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PricedModelPolicy {
    version: u8,
    pub(super) models: Vec<PricedModelRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct UsageReport {
    pub(super) period: String,
    pub(super) generated_at_unix: u64,
    pub(super) budget_eur: f64,
    pub(super) spent_eur: f64,
    pub(super) remaining_eur: f64,
    pub(super) over_budget: bool,
    #[serde(default)]
    pub(super) reserved_eur: f64,
    #[serde(default)]
    pub(super) remaining_hard_eur: f64,
    #[serde(default)]
    pub(super) above_soft_budget: bool,
    pub(super) requests: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read_tokens: u64,
    pub(super) cache_write_tokens: u64,
    pub(super) total_tokens: u64,
    #[serde(default)]
    pub(super) by_backend: Vec<UsageRow>,
    #[serde(default)]
    pub(super) by_model: Vec<UsageRow>,
    #[serde(default)]
    pub(super) daily: Vec<DailyUsageRow>,
    pub(super) pricing: PricingSummary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct UsageRow {
    pub(super) backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) model: Option<String>,
    pub(super) spent_eur: f64,
    pub(super) requests: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read_tokens: u64,
    pub(super) cache_write_tokens: u64,
    pub(super) total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct DailyUsageRow {
    pub(super) day: String,
    pub(super) spent_eur: f64,
    pub(super) requests: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read_tokens: u64,
    pub(super) cache_write_tokens: u64,
    pub(super) total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct PricingSummary {
    pub(super) source: String,
    pub(super) updated_at: String,
}

fn validate_pricing_registry(registry: &PricingRegistry) -> Result<()> {
    if registry.version != 1
        || registry.source.trim().is_empty()
        || registry.updated_at.trim().is_empty()
        || registry.models.len() > 2_000
    {
        bail!("pricing registry is unsupported or incomplete");
    }
    let mut seen = BTreeSet::new();
    for entry in &registry.models {
        if entry.provider.is_empty()
            || entry.provider.len() > 64
            || entry.model.is_empty()
            || entry.model.len() > 256
            || !entry.input_per_million_usd.is_finite()
            || !entry.output_per_million_usd.is_finite()
            || entry.input_per_million_usd < 0.0
            || entry.output_per_million_usd < 0.0
            || entry
                .cache_read_per_million_usd
                .is_some_and(|price| !price.is_finite() || price < 0.0)
            || !seen.insert((entry.provider.clone(), entry.model.clone()))
        {
            bail!("pricing registry contains an invalid or duplicate entry");
        }
    }
    Ok(())
}

fn parse_pricing_registry(path: &Path) -> Result<PricingRegistry> {
    let metadata = fs::symlink_metadata(path).context("inspect pricing registry")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_DOCUMENT_SIZE
    {
        bail!("pricing registry file is unsafe");
    }
    let registry: PricingRegistry =
        serde_json::from_slice(&fs::read(path).context("read pricing registry")?)
            .context("parse pricing registry")?;
    validate_pricing_registry(&registry)?;
    Ok(registry)
}

fn valid_release_tag(tag: &str) -> bool {
    let mut parts = tag.strip_prefix('v').unwrap_or_default().split('.');
    parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Returns `None` only when a correctly rooted active release predates the
/// pricing file. An unsafe link, present symlink, or unsafe file fails closed.
fn resolve_optional_release_data_file(name: &'static str) -> Result<Option<PathBuf>> {
    let releases = Path::new(RELEASES_ROOT);
    let canonical_releases = fs::canonicalize(releases).context("resolve managed release root")?;
    if canonical_releases != releases {
        bail!("managed release root must not traverse links");
    }
    let current = Path::new(CURRENT_RELEASE);
    let current_metadata = fs::symlink_metadata(current).context("inspect active release link")?;
    if !current_metadata.file_type().is_symlink()
        || current_metadata.uid() != 0
        || current_metadata.gid() != 0
    {
        bail!("active release link is unsafe");
    }
    let active = fs::canonicalize(current).context("resolve active release")?;
    let tag = active
        .strip_prefix(&canonical_releases)
        .ok()
        .and_then(Path::to_str)
        .filter(|value| !value.contains('/'))
        .context("active release is outside the managed release root")?;
    if !valid_release_tag(tag) || active.parent() != Some(canonical_releases.as_path()) {
        bail!("active release path is not a stable managed release");
    }

    let path = active.join(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect release data file"),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        bail!("release data file ownership or permissions are unsafe");
    }
    let canonical = fs::canonicalize(&path).context("resolve release data file")?;
    if canonical.parent() != Some(active.as_path())
        || canonical.file_name() != Some(OsStr::new(name))
    {
        bail!("release data file escapes the active release");
    }
    Ok(Some(path))
}

fn read_layered_pricing_registry() -> Result<PricingRegistry> {
    let path = Path::new(OWNER_PRICING_REGISTRY);
    let metadata = fs::symlink_metadata(path).context("inspect owner pricing registry")?;
    let config = fs::symlink_metadata("/etc/jarvis").context("inspect config directory")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != config.gid()
        || metadata.permissions().mode() & 0o777 != 0o640
    {
        bail!("owner pricing registry permissions are unsafe");
    }
    let mut owner = parse_pricing_registry(path)?;
    if let Some(release_path) = resolve_optional_release_data_file(RELEASE_PRICING_REGISTRY)? {
        let release = parse_pricing_registry(&release_path)?;
        let mut added = false;
        for entry in release.models {
            if !owner
                .models
                .iter()
                .any(|current| current.provider == entry.provider && current.model == entry.model)
            {
                owner.models.push(entry);
                added = true;
            }
        }
        if added {
            owner.source = format!("{} + {}", owner.source, release.source);
            if release.updated_at > owner.updated_at {
                owner.updated_at = release.updated_at;
            }
        }
    }
    validate_pricing_registry(&owner)?;
    Ok(owner)
}

pub(super) fn priced_model_policy(policy: ModelPolicy) -> Result<PricedModelPolicy> {
    let pricing = read_layered_pricing_registry()?;
    let hf_catalog = read_huggingface_catalog().ok();
    Ok(priced_model_policy_with_registry_and_hf(
        policy,
        &pricing,
        hf_catalog.as_ref(),
    ))
}

fn read_huggingface_catalog() -> Result<jarvis_llm::HuggingFaceCatalog> {
    let path = Path::new(HUGGINGFACE_CATALOG);
    let metadata = fs::symlink_metadata(path).context("inspect Hugging Face catalog")?;
    let config = fs::symlink_metadata("/etc/jarvis").context("inspect config directory")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != config.gid()
        || metadata.permissions().mode() & 0o777 != 0o640
    {
        bail!("Hugging Face catalog permissions are unsafe");
    }
    jarvis_llm::HuggingFaceCatalog::load(path).map_err(anyhow::Error::msg)
}

pub(super) fn huggingface_routes_for_model(model: &str) -> Result<Vec<String>> {
    let catalog = read_huggingface_catalog()?;
    Ok(huggingface_routes(&catalog, model))
}

fn huggingface_routes(catalog: &jarvis_llm::HuggingFaceCatalog, model: &str) -> Vec<String> {
    let mut routes = vec![
        "auto".to_owned(),
        "fastest".to_owned(),
        "cheapest".to_owned(),
        "preferred".to_owned(),
    ];
    if let Some(model) = catalog.model(model) {
        routes.extend(
            model
                .providers
                .iter()
                .filter(|provider| provider.status == "live")
                .map(|provider| provider.provider.clone()),
        );
    }
    routes.sort();
    routes.dedup();
    routes
}

#[cfg(test)]
fn priced_model_policy_with_registry(
    policy: ModelPolicy,
    pricing: &PricingRegistry,
) -> PricedModelPolicy {
    priced_model_policy_with_registry_and_hf(policy, pricing, None)
}

fn priced_model_policy_with_registry_and_hf(
    policy: ModelPolicy,
    pricing: &PricingRegistry,
    hf_catalog: Option<&jarvis_llm::HuggingFaceCatalog>,
) -> PricedModelPolicy {
    let models = policy
        .models
        .into_iter()
        .map(|model| {
            let local = !matches!(
                model.provider.as_str(),
                "anthropic-api"
                    | "openai-api"
                    | "deepseek-api"
                    | "xai-api"
                    | "zai-api"
                    | "ollama-cloud"
                    | "huggingface"
            );
            let entry = pricing
                .models
                .iter()
                .find(|entry| entry.provider == model.provider && entry.model == model.model);
            let hf_price = (model.provider == "huggingface")
                .then(|| hf_catalog?.conservative_price(&model.model, model.route.as_deref()?))
                .flatten();
            PricedModelRecord {
                provider: model.provider,
                model: model.model,
                enabled: model.enabled,
                source: model.source,
                route: model.route.clone(),
                price_status: if local {
                    "local"
                } else if entry.is_some() {
                    "known"
                } else if hf_price.is_some() {
                    if model.route.as_deref().is_some_and(|route| {
                        !matches!(route, "auto" | "fastest" | "cheapest" | "preferred")
                    }) {
                        "estimated"
                    } else {
                        "conservative"
                    }
                } else {
                    "unknown"
                },
                input_per_million_usd: entry
                    .map(|value| value.input_per_million_usd)
                    .or(hf_price.map(|price| price.0)),
                cache_read_per_million_usd: entry
                    .and_then(|value| {
                        value
                            .cache_read_per_million_usd
                            .or(Some(value.input_per_million_usd * 0.1))
                    })
                    .or(hf_price.map(|price| price.0)),
                output_per_million_usd: entry
                    .map(|value| value.output_per_million_usd)
                    .or(hf_price.map(|price| price.1)),
                pricing_source: pricing.source.clone(),
                pricing_updated_at: pricing.updated_at.clone(),
            }
        })
        .collect();
    PricedModelPolicy {
        version: policy.version,
        models,
    }
}

pub(super) fn display_model_price(value: Option<f64>, status: &str) -> String {
    match (value, status) {
        (_, "local") => "included".to_owned(),
        (Some(price), _) => format!("${price:.4}"),
        (None, _) => "unknown".to_owned(),
    }
}

pub(super) fn read_usage_report() -> Result<UsageReport> {
    let path = Path::new(USAGE_SUMMARY);
    let directory = fs::symlink_metadata("/var/lib/jarvis").context("inspect Jarvis state")?;
    let metadata = fs::symlink_metadata(path)
        .context("usage statistics are unavailable; restart Core or complete one model request")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != directory.uid()
        || metadata.gid() != directory.gid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > MAX_DOCUMENT_SIZE
    {
        bail!("usage summary ownership, permissions, or size are unsafe");
    }
    let report: UsageReport =
        serde_json::from_slice(&fs::read(path).context("read usage summary")?)
            .context("parse usage summary")?;
    if report.period != "current_calendar_month"
        || report.by_backend.len() > 32
        || report.by_model.len() > 250
        || report.daily.len() > 32
        || !report.spent_eur.is_finite()
        || !report.budget_eur.is_finite()
    {
        bail!("usage summary is unsupported or outside safe bounds");
    }
    Ok(report)
}

pub(super) fn usage(json: bool) -> Result<()> {
    let report = read_usage_report()?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    println!("Jarvis usage · current calendar month");
    println!("Requests:      {}", report.requests);
    println!("Total tokens:  {}", report.total_tokens);
    println!("Input tokens:  {}", report.input_tokens);
    println!("Output tokens: {}", report.output_tokens);
    println!("Cached input:  {}", report.cache_read_tokens);
    println!(
        "Spend:         €{:.2} / €{:.2}",
        report.spent_eur, report.budget_eur
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rejects_duplicate_exact_pairs() {
        let registry: PricingRegistry = serde_json::from_str(
            r#"{"version":1,"source":"fixture","updated_at":"2026-09-01","models":[{"provider":"ollama-cloud","model":"exact","input_per_million_usd":1.0,"output_per_million_usd":2.0},{"provider":"ollama-cloud","model":"exact","input_per_million_usd":1.0,"output_per_million_usd":2.0}]}"#,
        )
        .unwrap();
        assert!(validate_pricing_registry(&registry).is_err());
    }

    #[test]
    fn every_presentation_receives_the_same_exact_price_projection() {
        let registry: PricingRegistry = serde_json::from_str(
            r#"{"version":1,"source":"fixture","updated_at":"2026-09-01","models":[{"provider":"ollama-cloud","model":"exact","input_per_million_usd":0.07,"cache_read_per_million_usd":0.035,"output_per_million_usd":0.3}]}"#,
        )
        .unwrap();
        let policy: ModelPolicy = serde_json::from_str(
            r#"{"version":1,"models":[{"provider":"ollama-cloud","model":"exact","enabled":false,"source":"discovered"}]}"#,
        )
        .unwrap();
        let projected = priced_model_policy_with_registry(policy, &registry);
        assert_eq!(projected.models.len(), 1);
        let model = &projected.models[0];
        assert_eq!(model.price_status, "known");
        assert_eq!(model.input_per_million_usd, Some(0.07));
        assert_eq!(model.cache_read_per_million_usd, Some(0.035));
        assert_eq!(model.output_per_million_usd, Some(0.3));
        assert!(!model.enabled);
    }

    #[test]
    fn huggingface_dynamic_route_is_explicitly_conservative() {
        let registry: PricingRegistry = serde_json::from_str(
            r#"{"version":1,"source":"fixture","updated_at":"2026-09-01","models":[]}"#,
        )
        .unwrap();
        let policy: ModelPolicy = serde_json::from_str(
            r#"{"version":1,"models":[{"provider":"huggingface","model":"org/model","enabled":true,"source":"provider_api","route":"fastest"}]}"#,
        )
        .unwrap();
        let catalog = jarvis_llm::HuggingFaceCatalog {
            version: 1,
            discovered_at: "fixture".into(),
            models: vec![jarvis_llm::HuggingFaceModel {
                id: "org/model".into(),
                providers: vec![jarvis_llm::HuggingFaceProviderMetadata {
                    provider: "groq".into(),
                    status: "live".into(),
                    context_length: None,
                    input_per_million_usd: Some(0.1),
                    output_per_million_usd: Some(0.3),
                    supports_tools: None,
                    supports_structured_output: None,
                    first_token_latency_ms: None,
                    throughput: None,
                }],
            }],
        };
        let projected = priced_model_policy_with_registry_and_hf(policy, &registry, Some(&catalog));
        assert_eq!(projected.models[0].price_status, "conservative");
        assert_eq!(projected.models[0].output_per_million_usd, Some(0.3));
        assert!(huggingface_routes(&catalog, "org/model").contains(&"groq".to_owned()));
    }

    #[test]
    fn usage_report_rejects_secret_or_content_fields_by_schema() {
        let report: UsageReport = serde_json::from_str(
            r#"{"period":"current_calendar_month","generated_at_unix":1,"budget_eur":20.0,"spent_eur":1.0,"remaining_eur":19.0,"over_budget":false,"requests":1,"input_tokens":2,"output_tokens":3,"cache_read_tokens":0,"cache_write_tokens":0,"total_tokens":5,"by_backend":[],"by_model":[],"daily":[],"pricing":{"source":"fixture","updated_at":"2026-09-01"},"prompt":"must-not-be-retained","api_key":"must-not-be-retained"}"#,
        )
        .unwrap();
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("prompt"));
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("must-not-be-retained"));
    }
}
