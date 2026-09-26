//! LLM usage & cost tracking — the money side of the cost-aware router (ADR-027).
//!
//! Only *metered* API backends cost money; the Claude plan (`claude-cli`) and
//! local Ollama remain zero-cost. Provider-reported token counts are recorded
//! for every backend, while paid calls additionally receive an estimated cost.
//! Monthly spend feeds a hard EUR budget the router enforces before paid calls.
//!
//! Prices are best-effort estimates in USD per 1M tokens (providers bill in USD);
//! they can drift, so treat the budget as a safety cap, not an exact invoice.

use std::{collections::BTreeMap, fs, path::Path, sync::Mutex};

use serde::{Deserialize, Serialize};
pub mod surreal;

/// The metered backends — the only ones that spend money.
pub const METERED_BACKENDS: [&str; 8] = [
    "anthropic-api",
    "openai-api",
    "deepseek-api",
    "xai-api",
    "zai-api",
    "ollama-cloud",
    "huggingface",
    "jev",
];

/// Whether a backend id bills per token (vs. the free plan/local brains).
pub fn is_metered(backend: &str) -> bool {
    METERED_BACKENDS.contains(&backend)
}

/// Per-1M-token price in USD.
#[derive(Debug, Clone, Copy)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Cached input reads. Unknown discounts use the full input rate.
    pub cache_read: f64,
}

/// Root-managed, versioned pricing metadata.  Provider prices change often,
/// so routing/accounting never treats source code fragments as an irreversible
/// price authority.  A missing or malformed registry fails safely to the
/// conservative built-in baseline; an unknown remote model is never free.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingRegistry {
    pub version: u32,
    pub source: String,
    pub updated_at: String,
    #[serde(default)]
    pub models: Vec<PricingEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricingEntry {
    pub provider: String,
    pub model: String,
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    #[serde(default)]
    pub cache_read_per_million_usd: Option<f64>,
    /// Classification is explicit so provider-discovered prices are never
    /// presented as owner-reviewed exact values. Legacy registries default to
    /// `known`.
    #[serde(default = "known_price_status")]
    pub price_status: PriceStatus,
    #[serde(default)]
    pub pricing_source: Option<String>,
    #[serde(default)]
    pub pricing_updated_at: Option<String>,
    #[serde(default)]
    pub pricing_notes: Option<String>,
    #[serde(default)]
    pub long_context: Option<LongContextPrice>,
    /// Explicitly retain even a rate identical to a historical shipped default.
    #[serde(default)]
    pub owner_override: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LongContextPrice {
    /// Inclusive threshold, including cached input tokens.
    pub from_input_tokens: u32,
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    pub cache_read_per_million_usd: f64,
}

const fn known_price_status() -> PriceStatus {
    PriceStatus::Known
}

impl PricingRegistry {
    pub fn builtin() -> Self {
        // One reviewed artifact for runtime accounting, CLI and GUI; no second
        // hand-maintained Rust price table that can drift from release data.
        serde_json::from_str(include_str!(
            "../../../deploy/systemd/pricing-registry.json"
        ))
        .expect("release pricing registry must pass the catalog tests")
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let raw = fs::read_to_string(path.as_ref())
            .map_err(|error| format!("pricing registry is unavailable: {error}"))?;
        let registry: Self = serde_json::from_str(&raw)
            .map_err(|error| format!("pricing registry is malformed: {error}"))?;
        registry.validate()?;
        Ok(registry)
    }

    /// Load owner-managed prices and fill only missing exact model pairs from
    /// the release-reviewed baseline. Owner entries always win; a release can
    /// therefore add safe coverage without overwriting an explicit override.
    pub fn load_with_builtin(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::load(path)?.with_release(Self::builtin())
    }

    /// Read-time migration only. Files remain untouched, including on rollback.
    /// Only exact entries from the two historically shipped default catalogs
    /// are discarded. Custom entries and explicit overrides always survive.
    pub fn with_release(mut self, builtin: Self) -> Result<Self, String> {
        self.validate()?;
        builtin.validate()?;
        for raw in [
            include_str!("../data/legacy-pricing-2026-08-27.json"),
            include_str!("../data/legacy-pricing-2026-09-01.json"),
        ] {
            let legacy: Self = serde_json::from_str(raw).expect("tested legacy pricing snapshot");
            if self.source == legacy.source && self.updated_at == legacy.updated_at {
                self.models.retain(|entry| {
                    entry.owner_override || !legacy.models.iter().any(|default| default == entry)
                });
            }
        }
        let mut registry = self;
        registry.bind_provenance();
        let mut added = false;
        for entry in builtin.models {
            if !registry
                .models
                .iter()
                .any(|current| current.provider == entry.provider && current.model == entry.model)
            {
                registry.models.push(entry);
                added = true;
            }
        }
        if added {
            registry.source = format!("{} + {}", registry.source, builtin.source);
            if builtin.updated_at > registry.updated_at {
                registry.updated_at = builtin.updated_at;
            }
        }
        registry.validate()?;
        Ok(registry)
    }

    fn bind_provenance(&mut self) {
        for entry in &mut self.models {
            entry
                .pricing_source
                .get_or_insert_with(|| self.source.clone());
            entry
                .pricing_updated_at
                .get_or_insert_with(|| self.updated_at.clone());
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.source.trim().is_empty() || self.updated_at.trim().is_empty() {
            return Err("unsupported or incomplete pricing registry".into());
        }
        let mut seen = BTreeMap::new();
        for entry in &self.models {
            if entry.provider.trim().is_empty()
                || entry.model.trim().is_empty()
                || !entry.input_per_million_usd.is_finite()
                || !entry.output_per_million_usd.is_finite()
                || entry.input_per_million_usd < 0.0
                || entry.output_per_million_usd < 0.0
                || entry
                    .cache_read_per_million_usd
                    .is_some_and(|price| !price.is_finite() || price < 0.0)
                || entry.long_context.as_ref().is_some_and(|tier| {
                    tier.from_input_tokens == 0
                        || [
                            tier.input_per_million_usd,
                            tier.output_per_million_usd,
                            tier.cache_read_per_million_usd,
                        ]
                        .iter()
                        .any(|price| !price.is_finite() || *price < 0.0)
                        || tier.input_per_million_usd < entry.input_per_million_usd
                        || tier.output_per_million_usd < entry.output_per_million_usd
                })
                || seen
                    .insert((entry.provider.clone(), entry.model.clone()), ())
                    .is_some()
            {
                return Err("pricing registry contains an invalid or duplicate entry".into());
            }
        }
        Ok(())
    }

    pub fn price_for(&self, backend: &str, model: &str) -> (Price, PriceStatus) {
        if !is_metered(backend) {
            return (Price::new(0.0, 0.0), PriceStatus::Local);
        }
        if let Some(entry) = self
            .models
            .iter()
            .find(|entry| entry.provider == backend && entry.model == model)
        {
            return (
                Price {
                    input: entry.input_per_million_usd,
                    output: entry.output_per_million_usd,
                    cache_read: entry
                        .cache_read_per_million_usd
                        .unwrap_or(entry.input_per_million_usd),
                },
                entry.price_status,
            );
        }
        // HF routes can move across infrastructure providers. Its catalog
        // validation accepts no price above this ceiling, so missing/partial
        // HF pricing reserves against the ceiling rather than inheriting the
        // much lower generic unknown-model estimate.
        if backend == "huggingface" {
            return (
                Price {
                    input: 1_000_000.0,
                    output: 1_000_000.0,
                    cache_read: 1_000_000.0,
                },
                PriceStatus::Unknown,
            );
        }
        (Price::new(3.0, 15.0), PriceStatus::Unknown)
    }
}

#[cfg(test)]
fn entry(provider: &str, model: &str, input: f64, output: f64) -> PricingEntry {
    PricingEntry {
        provider: provider.into(),
        model: model.into(),
        input_per_million_usd: input,
        output_per_million_usd: output,
        cache_read_per_million_usd: None,
        price_status: PriceStatus::Known,
        pricing_source: None,
        pricing_updated_at: None,
        pricing_notes: None,
        long_context: None,
        owner_override: false,
    }
}

/// Price metadata is versioned in source and intentionally distinguishes an
/// unknown remote price from a free local model.  The conservative fallback is
/// used for accounting only; the registry/UI can show its `Unknown` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    Known,
    Estimated,
    Conservative,
    Unknown,
    Local,
}

pub fn price_status(backend: &str, model: &str) -> PriceStatus {
    PricingRegistry::builtin().price_for(backend, model).1
}

impl Price {
    const fn new(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: input,
        }
    }
}

/// Look up a model's price by matching known name fragments. Unknown metered
/// models fall back to a deliberately *not-cheap* estimate so we never silently
/// undercount and blow past the budget.
pub fn price_for(model: &str) -> Price {
    PricingRegistry::builtin().price_for("openai-api", model).0
}

/// Estimated cost in EUR for one call. Free backends (plan/local) return 0.0.
pub fn cost_eur(
    backend: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    eur_per_usd: f64,
) -> f64 {
    cost_eur_with_registry(
        &PricingRegistry::builtin(),
        backend,
        model,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        eur_per_usd,
    )
}

pub fn cost_eur_with_registry(
    registry: &PricingRegistry,
    backend: &str,
    model: &str,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    eur_per_usd: f64,
) -> f64 {
    if !is_metered(backend) {
        return 0.0;
    }
    let (mut p, _) = registry.price_for(backend, model);
    if let Some(tier) = registry
        .models
        .iter()
        .find(|entry| entry.provider == backend && entry.model == model)
        .and_then(|entry| entry.long_context.as_ref())
        .filter(|tier| input_tokens.saturating_add(cache_read_tokens) >= tier.from_input_tokens)
    {
        p = Price {
            input: tier.input_per_million_usd,
            output: tier.output_per_million_usd,
            cache_read: tier.cache_read_per_million_usd,
        };
    }
    let per_mtok = |tokens: u32, usd: f64| (tokens as f64 / 1_000_000.0) * usd;
    let usd = per_mtok(input_tokens, p.input)
        + per_mtok(output_tokens, p.output)
        + per_mtok(cache_read_tokens, p.cache_read);
    usd * eur_per_usd
}

/// One recorded call.
#[derive(Debug, Clone, Serialize)]
pub struct UsageEntry {
    pub request_id: String,
    pub backend: String,
    pub model: String,
    pub requested_route: Option<String>,
    pub actual_provider: Option<String>,
    pub cost_estimate_classification: String,
    pub routing_mode: String,
    pub quality_tier: String,
    pub agent_id: Option<String>,
    pub latency_ms: i64,
    pub status: String,
    pub failure_category: Option<String>,
    pub fallback_count: i32,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_tokens: i32,
    pub cache_write_tokens: i32,
    pub cost_eur: f64,
}

/// Non-content routing facts persisted alongside a model call.  It is
/// deliberately unable to carry a prompt, response, credential or signature.
#[derive(Debug, Clone)]
pub struct UsageMetadata {
    pub request_id: String,
    pub routing_mode: String,
    pub quality_tier: String,
    pub agent_id: Option<String>,
    pub latency_ms: i64,
    pub status: String,
    pub failure_category: Option<String>,
    pub fallback_count: i32,
}

impl Default for UsageMetadata {
    fn default() -> Self {
        Self {
            request_id: uuid::Uuid::now_v7().to_string(),
            routing_mode: "internal".into(),
            quality_tier: "unknown".into(),
            agent_id: None,
            latency_ms: 0,
            status: "succeeded".into(),
            failure_category: None,
            fallback_count: 0,
        }
    }
}

/// Bounded, explicitly uncertain preflight estimate for a multi-call task.
/// It is policy input, never a promise or a hidden reasoning trace.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CostEstimate {
    pub low_eur: f64,
    pub likely_eur: f64,
    pub high_eur: f64,
    pub price_status: PriceStatus,
}

pub fn estimate_task_cost(
    backend: &str,
    model: &str,
    input_tokens_per_call: u32,
    output_tokens_per_call: u32,
    calls: u32,
    eur_per_usd: f64,
) -> CostEstimate {
    estimate_task_cost_with_registry(
        &PricingRegistry::builtin(),
        backend,
        model,
        input_tokens_per_call,
        output_tokens_per_call,
        calls,
        eur_per_usd,
    )
}

pub fn estimate_task_cost_with_registry(
    registry: &PricingRegistry,
    backend: &str,
    model: &str,
    input_tokens_per_call: u32,
    output_tokens_per_call: u32,
    calls: u32,
    eur_per_usd: f64,
) -> CostEstimate {
    let likely = cost_eur_with_registry(
        registry,
        backend,
        model,
        input_tokens_per_call,
        output_tokens_per_call,
        0,
        eur_per_usd,
    ) * f64::from(calls);
    let status = registry.price_for(backend, model).1;
    let low_factor = match status {
        PriceStatus::Unknown | PriceStatus::Conservative => 1.0,
        PriceStatus::Known | PriceStatus::Estimated | PriceStatus::Local => 0.6,
    };
    let high_factor = match status {
        PriceStatus::Unknown => 2.5,
        PriceStatus::Estimated | PriceStatus::Known => 1.6,
        PriceStatus::Conservative | PriceStatus::Local => 1.0,
    };
    CostEstimate {
        low_eur: likely * low_factor,
        likely_eur: likely,
        high_eur: likely * high_factor,
        price_status: status,
    }
}

/// SurrealDB persistence functions. Failures remain best-effort at the caller,
/// so metering cannot break an assistant reply.
pub use surreal::{
    month_breakdown, month_statistics, month_total_eur, record, release_task, reserve_task,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cost_eur: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageDimension {
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyUsage {
    pub day: String,
    #[serde(flatten)]
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStatistics {
    pub totals: UsageTotals,
    pub by_backend: Vec<UsageDimension>,
    pub by_model: Vec<UsageDimension>,
    pub daily: Vec<DailyUsage>,
}

/// EUR-cent limits for the current calendar month.  Zero is a real hard stop,
/// never an implicit unlimited budget.
#[derive(Debug, Clone, Copy)]
pub struct BudgetLimits {
    pub monthly_soft_cents: u64,
    pub monthly_hard_cents: u64,
    pub per_request_hard_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    RequestCap,
    MonthlyHardCap,
    DuplicateReservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub actual_cents: u64,
    pub reserved_cents: u64,
    pub remaining_hard_cents: u64,
    pub above_soft_limit: bool,
}

#[derive(Debug, Default)]
struct BudgetState {
    actual_cents: u64,
    reservations: BTreeMap<String, u64>,
}

/// Process-local accounting gate.  It is deliberately atomic under one mutex
/// so concurrent requests cannot reserve the same remaining budget.  Durable
/// usage records remain the source for recovery/reconciliation on restart.
#[derive(Debug)]
pub struct BudgetBook {
    limits: BudgetLimits,
    state: Mutex<BudgetState>,
}

impl BudgetBook {
    pub fn new(limits: BudgetLimits, actual_cents: u64) -> Self {
        Self {
            limits,
            state: Mutex::new(BudgetState {
                actual_cents,
                reservations: BTreeMap::new(),
            }),
        }
    }

    pub fn reserve(&self, id: impl Into<String>, projected_cents: u64) -> Result<(), BudgetError> {
        let id = id.into();
        if projected_cents > self.limits.per_request_hard_cents {
            return Err(BudgetError::RequestCap);
        }
        let mut state = self.state.lock().expect("budget mutex poisoned");
        if state.reservations.contains_key(&id) {
            return Err(BudgetError::DuplicateReservation);
        }
        let reserved: u64 = state.reservations.values().sum();
        if state
            .actual_cents
            .saturating_add(reserved)
            .saturating_add(projected_cents)
            > self.limits.monthly_hard_cents
        {
            return Err(BudgetError::MonthlyHardCap);
        }
        state.reservations.insert(id, projected_cents);
        Ok(())
    }

    /// Commit actual cost and release the corresponding projection.  Actual
    /// spend can never be reduced by a failed/retried request.
    pub fn settle(&self, id: &str, actual_cents: u64) -> Result<(), BudgetError> {
        let mut state = self.state.lock().expect("budget mutex poisoned");
        let Some(projected) = state.reservations.remove(id) else {
            return Err(BudgetError::DuplicateReservation);
        };
        let others: u64 = state.reservations.values().sum();
        if state
            .actual_cents
            .saturating_add(others)
            .saturating_add(actual_cents)
            > self.limits.monthly_hard_cents
        {
            // Preserve a reservation at the larger amount so a caller cannot
            // bypass the cap by settling an unexpectedly costly task.
            state
                .reservations
                .insert(id.to_string(), projected.max(actual_cents));
            return Err(BudgetError::MonthlyHardCap);
        }
        state.actual_cents = state.actual_cents.saturating_add(actual_cents);
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> bool {
        self.state
            .lock()
            .expect("budget mutex poisoned")
            .reservations
            .remove(id)
            .is_some()
    }

    pub fn reconcile_actual(&self, actual_cents: u64) {
        self.state
            .lock()
            .expect("budget mutex poisoned")
            .actual_cents = actual_cents;
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        let state = self.state.lock().expect("budget mutex poisoned");
        let reserved_cents: u64 = state.reservations.values().sum();
        BudgetSnapshot {
            actual_cents: state.actual_cents,
            reserved_cents,
            remaining_hard_cents: self
                .limits
                .monthly_hard_cents
                .saturating_sub(state.actual_cents.saturating_add(reserved_cents)),
            above_soft_limit: state.actual_cents.saturating_add(reserved_cents)
                >= self.limits.monthly_soft_cents,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_and_local_are_free() {
        assert_eq!(
            cost_eur("claude-cli", "claude-opus-5", 1000, 1000, 0, 0.92),
            0.0
        );
        assert_eq!(cost_eur("ollama", "llama3.2", 5000, 5000, 0, 0.92), 0.0);
        assert!(!is_metered("claude-cli"));
        assert!(!is_metered("ollama"));
    }

    #[test]
    fn metered_backends_cost_money() {
        assert!(is_metered("anthropic-api"));
        assert!(is_metered("openai-api"));
        assert!(is_metered("deepseek-api"));
        assert!(is_metered("huggingface"));
        assert!(is_metered("jev"));
        // 1M in + 1M out on sonnet = (2 + 10) USD × 0.92 = 11.04 EUR.
        let c = cost_eur(
            "anthropic-api",
            "claude-sonnet-5",
            1_000_000,
            1_000_000,
            0,
            0.92,
        );
        assert!((c - 11.04).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn deepseek_is_far_cheaper_than_opus() {
        let ds = cost_eur("deepseek-api", "deepseek-flash", 500_000, 500_000, 0, 0.92);
        let opus = cost_eur("anthropic-api", "claude-opus-5", 500_000, 500_000, 0, 0.92);
        assert!(ds < opus / 10.0, "deepseek {ds} vs opus {opus}");
    }

    #[test]
    fn unknown_metered_model_is_not_free() {
        let c = cost_eur("openai-api", "some-future-model", 1000, 1000, 0, 0.92);
        assert!(c > 0.0);
        let hf = cost_eur("huggingface", "org/future-model", 1_000, 1_000, 0, 0.92);
        assert!(hf >= 1_000.0);
    }

    #[test]
    fn cache_reads_are_cheaper_than_fresh_input() {
        let fresh = cost_eur("anthropic-api", "claude-sonnet-5", 1_000_000, 0, 0, 0.92);
        let cached = cost_eur("anthropic-api", "claude-sonnet-5", 0, 0, 1_000_000, 0.92);
        assert!(cached < fresh / 5.0, "cached {cached} vs fresh {fresh}");
    }

    #[test]
    fn reservations_are_atomic_and_released_on_cancel() {
        let book = BudgetBook::new(
            BudgetLimits {
                monthly_soft_cents: 800,
                monthly_hard_cents: 1_000,
                per_request_hard_cents: 700,
            },
            200,
        );
        book.reserve("a", 600).unwrap();
        assert_eq!(book.reserve("b", 300), Err(BudgetError::MonthlyHardCap));
        assert!(book.cancel("a"));
        book.reserve("b", 300).unwrap();
        assert_eq!(book.snapshot().remaining_hard_cents, 500);
    }

    #[test]
    fn request_cap_and_unknown_price_are_not_free() {
        let book = BudgetBook::new(
            BudgetLimits {
                monthly_soft_cents: 100,
                monthly_hard_cents: 200,
                per_request_hard_cents: 100,
            },
            0,
        );
        assert_eq!(book.reserve("too-large", 101), Err(BudgetError::RequestCap));
        assert_eq!(price_status("xai-api", "future-grok"), PriceStatus::Unknown);
        assert!(cost_eur("xai-api", "future-grok", 1_000, 1_000, 0, 0.92) > 0.0);
    }

    #[test]
    fn preflight_exposes_a_range_and_is_conservative_for_unknown_prices() {
        let known = estimate_task_cost("openai-api", "gpt-4o-mini", 1_000, 500, 10, 0.92);
        assert!(known.low_eur < known.likely_eur && known.likely_eur < known.high_eur);
        let unknown = estimate_task_cost("xai-api", "future-grok", 1_000, 500, 10, 0.92);
        assert_eq!(unknown.price_status, PriceStatus::Unknown);
        assert!(unknown.high_eur > unknown.likely_eur);
    }

    #[test]
    fn registry_uses_exact_provider_model_entries_and_unknown_is_conservative() {
        let registry = PricingRegistry {
            version: 1,
            source: "test".into(),
            updated_at: "2026-08-27".into(),
            models: vec![entry("openai-api", "exact-model", 0.1, 0.2)],
        };
        assert!(registry.validate().is_ok());
        assert_eq!(
            registry.price_for("openai-api", "exact-model").1,
            PriceStatus::Known
        );
        assert_eq!(
            registry.price_for("openai-api", "exact-model-latest").1,
            PriceStatus::Unknown
        );
        assert!(
            cost_eur_with_registry(
                &registry,
                "openai-api",
                "exact-model-latest",
                1_000,
                1_000,
                0,
                1.0
            ) > 0.0
        );
    }

    #[test]
    fn registry_rejects_duplicate_or_negative_prices() {
        let mut registry = PricingRegistry::builtin();
        registry.models.push(registry.models[0].clone());
        assert!(registry.validate().is_err());
        let mut registry = PricingRegistry::builtin();
        registry.models[0].input_per_million_usd = -1.0;
        assert!(registry.validate().is_err());
    }

    #[test]
    fn reviewed_ollama_cloud_prices_use_exact_discovered_ids() {
        let registry = PricingRegistry::builtin();
        let (price, status) = registry.price_for("ollama-cloud", "gpt-oss:20b");
        assert_eq!(status, PriceStatus::Known);
        assert_eq!(price.input, 0.07);
        assert_eq!(price.cache_read, 0.035);
        assert_eq!(price.output, 0.30);
        assert_eq!(
            registry.price_for("ollama-cloud", "gpt-oss").1,
            PriceStatus::Unknown
        );
    }

    #[test]
    fn owner_pricing_wins_while_builtin_fills_missing_entries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pricing.json");
        std::fs::write(
            &path,
            r#"{"version":1,"source":"owner","updated_at":"2026-09-01","models":[{"provider":"ollama-cloud","model":"gpt-oss:20b","input_per_million_usd":9.0,"output_per_million_usd":10.0}]}"#,
        )
        .unwrap();
        let registry = PricingRegistry::load_with_builtin(path).unwrap();
        assert_eq!(
            registry.price_for("ollama-cloud", "gpt-oss:20b").0.input,
            9.0
        );
        let owner = registry
            .models
            .iter()
            .find(|entry| entry.model == "gpt-oss:20b")
            .unwrap();
        assert_eq!(owner.pricing_source.as_deref(), Some("owner"));
        assert_eq!(owner.pricing_updated_at.as_deref(), Some("2026-09-01"));
        assert_eq!(
            registry.price_for("ollama-cloud", "glm-5.3").1,
            PriceStatus::Known
        );
        assert!(registry
            .source
            .contains("release-reviewed-provider-pricing"));
        assert_eq!(registry.updated_at, "2026-09-24");
    }
}
