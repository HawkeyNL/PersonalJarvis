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
pub const METERED_BACKENDS: [&str; 6] = [
    "anthropic-api",
    "openai-api",
    "deepseek-api",
    "xai-api",
    "zai-api",
    "ollama-cloud",
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
    /// Cached input reads (cheaper). Defaults to 10% of input when unknown.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    pub provider: String,
    pub model: String,
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    #[serde(default)]
    pub cache_read_per_million_usd: Option<f64>,
}

impl PricingRegistry {
    pub fn builtin() -> Self {
        Self {
            version: 1,
            source: "owner-reviewed-baseline-2026-09-01".into(),
            updated_at: "2026-09-01".into(),
            models: vec![
                entry("anthropic-api", "claude-opus-5", 5.0, 25.0),
                entry("anthropic-api", "claude-sonnet-5", 3.0, 15.0),
                entry("anthropic-api", "claude-haiku-4-5", 1.0, 5.0),
                entry("openai-api", "gpt-4o", 2.5, 10.0),
                entry("openai-api", "gpt-4o-mini", 0.15, 0.60),
                entry("openai-api", "gpt-4.1", 2.5, 10.0),
                entry("openai-api", "gpt-4.1-mini", 0.15, 0.60),
                entry("deepseek-api", "deepseek-chat", 0.27, 1.10),
                entry("deepseek-api", "deepseek-reasoner", 0.55, 2.19),
                entry_cached("ollama-cloud", "deepseek-v4-flash:0731", 0.44, 0.014, 1.32),
                entry_cached("ollama-cloud", "deepseek-v4-pro:0813", 1.32, 0.044, 3.96),
                entry_cached("ollama-cloud", "gemma4:31b", 0.14, 0.05, 0.40),
                entry_cached("ollama-cloud", "glm-5.3", 1.40, 0.26, 4.40),
                entry_cached("ollama-cloud", "glm-5.3-flash", 0.15, 0.03, 0.50),
                entry_cached("ollama-cloud", "glm-5.2", 1.40, 0.26, 4.40),
                entry_cached("ollama-cloud", "glm-5.1", 1.00, 0.20, 3.20),
                entry_cached("ollama-cloud", "gpt-oss:120b", 0.15, 0.014, 0.60),
                entry_cached("ollama-cloud", "gpt-oss:20b", 0.07, 0.035, 0.30),
                entry_cached("ollama-cloud", "kimi-k3", 3.00, 0.30, 15.00),
                entry_cached("ollama-cloud", "kimi-k2.7-code", 0.95, 0.19, 4.00),
                entry_cached("ollama-cloud", "kimi-k2.6", 0.95, 0.16, 4.00),
                entry_cached("ollama-cloud", "minimax-m3", 0.60, 0.12, 2.40),
                entry_cached("ollama-cloud", "minimax-m2.7", 0.30, 0.06, 1.20),
                entry_cached("ollama-cloud", "mistral-large-3:675b", 0.50, 0.50, 1.50),
                entry_cached("ollama-cloud", "nemotron-3-nano:30b", 0.06, 0.06, 0.24),
                entry_cached("ollama-cloud", "nemotron-3-super", 0.015, 0.015, 0.60),
                entry_cached("ollama-cloud", "nemotron-3-ultra", 0.10, 0.10, 3.00),
                entry_cached("ollama-cloud", "qwen3.5:397b", 0.60, 0.60, 3.60),
            ],
        }
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
        let mut registry = Self::load(path)?;
        let builtin = Self::builtin();
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
                        .unwrap_or(entry.input_per_million_usd * 0.1),
                },
                PriceStatus::Known,
            );
        }
        (Price::new(3.0, 15.0), PriceStatus::Unknown)
    }
}

fn entry(provider: &str, model: &str, input: f64, output: f64) -> PricingEntry {
    PricingEntry {
        provider: provider.into(),
        model: model.into(),
        input_per_million_usd: input,
        output_per_million_usd: output,
        cache_read_per_million_usd: None,
    }
}

fn entry_cached(
    provider: &str,
    model: &str,
    input: f64,
    cache_read: f64,
    output: f64,
) -> PricingEntry {
    PricingEntry {
        provider: provider.into(),
        model: model.into(),
        input_per_million_usd: input,
        output_per_million_usd: output,
        cache_read_per_million_usd: Some(cache_read),
    }
}

/// Price metadata is versioned in source and intentionally distinguishes an
/// unknown remote price from a free local model.  The conservative fallback is
/// used for accounting only; the registry/UI can show its `Unknown` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceStatus {
    Known,
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
            cache_read: input * 0.1,
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
    let (p, _) = registry.price_for(backend, model);
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
        input_tokens_per_call.saturating_mul(calls),
        output_tokens_per_call.saturating_mul(calls),
        0,
        eur_per_usd,
    );
    let status = registry.price_for(backend, model).1;
    let low_factor = if status == PriceStatus::Unknown {
        1.0
    } else {
        0.6
    };
    let high_factor = if status == PriceStatus::Unknown {
        2.5
    } else {
        1.6
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
        // 1M in + 1M out on sonnet = (3 + 15) USD × 0.92 ≈ 16.56 EUR.
        let c = cost_eur(
            "anthropic-api",
            "claude-sonnet-5",
            1_000_000,
            1_000_000,
            0,
            0.92,
        );
        assert!((c - 16.56).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn deepseek_is_far_cheaper_than_opus() {
        let ds = cost_eur("deepseek-api", "deepseek-chat", 500_000, 500_000, 0, 0.92);
        let opus = cost_eur("anthropic-api", "claude-opus-5", 500_000, 500_000, 0, 0.92);
        assert!(ds < opus / 10.0, "deepseek {ds} vs opus {opus}");
    }

    #[test]
    fn unknown_metered_model_is_not_free() {
        let c = cost_eur("openai-api", "some-future-model", 1000, 1000, 0, 0.92);
        assert!(c > 0.0);
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
        assert_eq!(
            registry.price_for("ollama-cloud", "glm-5.3").1,
            PriceStatus::Known
        );
        assert!(registry.source.contains("owner-reviewed-baseline"));
        assert_eq!(registry.updated_at, "2026-09-01");
    }
}
