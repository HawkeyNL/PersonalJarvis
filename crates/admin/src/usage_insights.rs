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

use super::{ModelPolicy, ModelRecord, CURRENT_RELEASE, RELEASES_ROOT};

const OWNER_PRICING_REGISTRY: &str = "/etc/jarvis/pricing-registry.json";
const RELEASE_PRICING_REGISTRY: &str = "pricing-registry.json";
const USAGE_SUMMARY: &str = "/var/lib/jarvis/usage-summary.json";
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

#[derive(Debug, Serialize)]
struct PricedModelRecord {
    #[serde(flatten)]
    model: ModelRecord,
    price_status: &'static str,
    input_per_million_usd: Option<f64>,
    cache_read_per_million_usd: Option<f64>,
    output_per_million_usd: Option<f64>,
    pricing_source: String,
    pricing_updated_at: String,
}

#[derive(Debug, Serialize)]
struct PricedModelPolicy {
    version: u8,
    models: Vec<PricedModelRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct UsageReport {
    period: String,
    generated_at_unix: u64,
    budget_eur: f64,
    spent_eur: f64,
    remaining_eur: f64,
    over_budget: bool,
    #[serde(default)]
    reserved_eur: f64,
    #[serde(default)]
    remaining_hard_eur: f64,
    #[serde(default)]
    above_soft_budget: bool,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
    #[serde(default)]
    by_backend: Vec<UsageRow>,
    #[serde(default)]
    by_model: Vec<UsageRow>,
    #[serde(default)]
    daily: Vec<DailyUsageRow>,
    pricing: PricingSummary,
}

#[derive(Debug, Deserialize, Serialize)]
struct UsageRow {
    backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    spent_eur: f64,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct DailyUsageRow {
    day: String,
    spent_eur: f64,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    total_tokens: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct PricingSummary {
    source: String,
    updated_at: String,
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

pub(super) fn priced_model_policy_json(policy: ModelPolicy) -> Result<String> {
    let pricing = read_layered_pricing_registry()?;
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
            );
            let entry = pricing
                .models
                .iter()
                .find(|entry| entry.provider == model.provider && entry.model == model.model);
            PricedModelRecord {
                model,
                price_status: if local {
                    "local"
                } else if entry.is_some() {
                    "known"
                } else {
                    "unknown"
                },
                input_per_million_usd: entry.map(|value| value.input_per_million_usd),
                cache_read_per_million_usd: entry.and_then(|value| {
                    value
                        .cache_read_per_million_usd
                        .or(Some(value.input_per_million_usd * 0.1))
                }),
                output_per_million_usd: entry.map(|value| value.output_per_million_usd),
                pricing_source: pricing.source.clone(),
                pricing_updated_at: pricing.updated_at.clone(),
            }
        })
        .collect();
    Ok(serde_json::to_string(&PricedModelPolicy {
        version: policy.version,
        models,
    })?)
}

fn read_usage_report() -> Result<UsageReport> {
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
