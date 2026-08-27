//! LLM usage & cost tracking — the money side of the cost-aware router (ADR-027).
//!
//! Only *metered* API backends cost money; the Claude plan (`claude-cli`) and
//! local Ollama are free and never counted. Each call's cost is estimated from
//! its model's per-token price and recorded to Postgres; the monthly total feeds
//! a hard EUR budget the router enforces by refusing paid calls once reached.
//!
//! Prices are best-effort estimates in USD per 1M tokens (providers bill in USD);
//! they can drift, so treat the budget as a safety cap, not an exact invoice.

use std::{collections::BTreeMap, sync::Mutex};

use serde::Serialize;
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
    if !is_metered(backend) {
        PriceStatus::Local
    } else if known_price(model) {
        PriceStatus::Known
    } else {
        PriceStatus::Unknown
    }
}

fn known_price(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    [
        "opus", "sonnet", "haiku", "gpt-", "o1", "o3", "o4", "deepseek",
    ]
    .iter()
    .any(|fragment| model.contains(fragment))
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
    let m = model.to_ascii_lowercase();
    // Order matters: check the more specific fragment first.
    if m.contains("opus") {
        Price::new(5.0, 25.0)
    } else if m.contains("sonnet") {
        Price::new(3.0, 15.0)
    } else if m.contains("haiku") {
        Price::new(1.0, 5.0)
    } else if m.contains("gpt-4o-mini") || m.contains("gpt-4.1-mini") || m.contains("o4-mini") {
        Price::new(0.15, 0.60)
    } else if m.contains("gpt-4o") || m.contains("gpt-4.1") {
        Price::new(2.5, 10.0)
    } else if m.contains("deepseek-reasoner") {
        Price::new(0.55, 2.19)
    } else if m.contains("deepseek") {
        Price::new(0.27, 1.10)
    } else {
        // Unknown metered model: assume a mid/expensive tier, not free.
        Price::new(3.0, 15.0)
    }
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
    if !is_metered(backend) {
        return 0.0;
    }
    let p = price_for(model);
    let per_mtok = |tokens: u32, usd: f64| (tokens as f64 / 1_000_000.0) * usd;
    let usd = per_mtok(input_tokens, p.input)
        + per_mtok(output_tokens, p.output)
        + per_mtok(cache_read_tokens, p.cache_read);
    usd * eur_per_usd
}

/// One recorded call.
#[derive(Debug, Clone, Serialize)]
pub struct UsageEntry {
    pub backend: String,
    pub model: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_tokens: i32,
    pub cache_write_tokens: i32,
    pub cost_eur: f64,
}

/// SurrealDB persistence functions. Failures remain best-effort at the caller,
/// so metering cannot break an assistant reply.
pub use surreal::{month_breakdown, month_total_eur, record, release_task, reserve_task};

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
}
