//! LLM usage & cost tracking — the money side of the cost-aware router (ADR-027).
//!
//! Only *metered* API backends cost money; the Claude plan (`claude-cli`) and
//! local Ollama are free and never counted. Each call's cost is estimated from
//! its model's per-token price and recorded to Postgres; the monthly total feeds
//! a hard EUR budget the router enforces by refusing paid calls once reached.
//!
//! Prices are best-effort estimates in USD per 1M tokens (providers bill in USD);
//! they can drift, so treat the budget as a safety cap, not an exact invoice.

use serde::Serialize;
use sqlx::PgPool;

pub mod surreal;

/// The metered backends — the only ones that spend money.
pub const METERED_BACKENDS: [&str; 3] = ["anthropic-api", "openai-api", "deepseek-api"];

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

/// Persist a call. Failures are the caller's to log — billing must never break a chat.
pub async fn record(pool: &PgPool, e: &UsageEntry) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO llm_usage \
         (backend, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_eur) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(&e.backend)
    .bind(&e.model)
    .bind(e.input_tokens)
    .bind(e.output_tokens)
    .bind(e.cache_read_tokens)
    .bind(e.cache_write_tokens)
    .bind(e.cost_eur)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Total EUR spent so far in the current calendar month (UTC).
pub async fn month_total_eur(pool: &PgPool) -> Result<f64, sqlx::Error> {
    let total: Option<f64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(cost_eur), 0) FROM llm_usage \
         WHERE ts >= date_trunc('month', now())",
    )
    .fetch_one(pool)
    .await?;
    Ok(total.unwrap_or(0.0))
}

/// Per-backend EUR spent this month, for the UI breakdown.
pub async fn month_breakdown(pool: &PgPool) -> Result<Vec<(String, f64)>, sqlx::Error> {
    let rows: Vec<(String, Option<f64>)> = sqlx::query_as(
        "SELECT backend, SUM(cost_eur) FROM llm_usage \
         WHERE ts >= date_trunc('month', now()) \
         GROUP BY backend ORDER BY SUM(cost_eur) DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(b, c)| (b, c.unwrap_or(0.0)))
        .collect())
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
}
