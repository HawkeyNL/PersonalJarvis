use serde_json::json;
use uuid::Uuid;

use jarvis_store::Database;

use super::{
    AgentUsage, DailyUsage, FailureCount, UsageDimension, UsageEntry, UsageStatistics, UsageTotals,
};

const CURRENT_MONTH_START: &str = "time::group(time::now(), 'month')";

fn month_total_query() -> String {
    format!(
        "SELECT math::sum(cost_eur) AS total FROM llm_usage \
         WHERE ts >= {CURRENT_MONTH_START} GROUP ALL"
    )
}

fn month_breakdown_query() -> String {
    format!(
        "SELECT backend, math::sum(cost_eur) AS total FROM llm_usage \
         WHERE ts >= {CURRENT_MONTH_START} GROUP BY backend ORDER BY total DESC"
    )
}

pub async fn record(db: &Database, entry: &UsageEntry) -> Result<(), jarvis_store::StoreError> {
    db.query(
        "CREATE llm_usage SET id = $id, ts = time::now(), request_id = $request_id, backend = $backend, model = $model, \
         requested_route = $requested_route, actual_provider = $actual_provider, cost_estimate_classification = $cost_estimate_classification, \
         routing_mode = $routing_mode, quality_tier = $quality_tier, agent_id = $agent_id, latency_ms = $latency_ms, \
         status = $status, failure_category = $failure_category, fallback_count = $fallback_count, \
         input_tokens = $input_tokens, output_tokens = $output_tokens, cache_read_tokens = $cache_read_tokens, \
         cache_write_tokens = $cache_write_tokens, cost_eur = $cost_eur RETURN NONE",
    )
    .bind(json!({
        "id": Uuid::now_v7().to_string(), "request_id": entry.request_id, "backend": entry.backend, "model": entry.model,
        "requested_route": entry.requested_route, "actual_provider": entry.actual_provider,
        "cost_estimate_classification": entry.cost_estimate_classification,
        "routing_mode": entry.routing_mode, "quality_tier": entry.quality_tier, "agent_id": entry.agent_id,
        "latency_ms": entry.latency_ms, "status": entry.status, "failure_category": entry.failure_category,
        "fallback_count": entry.fallback_count,
        "input_tokens": entry.input_tokens, "output_tokens": entry.output_tokens,
        "cache_read_tokens": entry.cache_read_tokens, "cache_write_tokens": entry.cache_write_tokens,
        "cost_eur": entry.cost_eur,
    }))
    .await
    .map_err(jarvis_store::StoreError::schema)?
    .check()
    .map_err(jarvis_store::StoreError::schema)?;
    Ok(())
}

pub async fn month_total_eur(db: &Database) -> Result<f64, jarvis_store::StoreError> {
    let mut response = db
        .query(month_total_query())
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    #[derive(serde::Deserialize)]
    struct Total {
        total: Option<f64>,
    }
    let row: Option<Total> = response.take(0).map_err(jarvis_store::StoreError::schema)?;
    Ok(row.and_then(|row| row.total).unwrap_or(0.0))
}

pub async fn month_breakdown(
    db: &Database,
) -> Result<Vec<(String, f64)>, jarvis_store::StoreError> {
    #[derive(serde::Deserialize)]
    struct Row {
        backend: String,
        total: Option<f64>,
    }
    let mut response = db
        .query(month_breakdown_query())
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    let rows: Vec<Row> = response.take(0).map_err(jarvis_store::StoreError::schema)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.backend, row.total.unwrap_or(0.0)))
        .collect())
}

#[derive(Default, serde::Deserialize)]
struct AggregateRow {
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    day: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    failure_category: Option<String>,
    #[serde(default)]
    last_used: Option<String>,
    #[serde(default)]
    requests: Option<i64>,
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
    #[serde(default)]
    cache_read_tokens: Option<i64>,
    #[serde(default)]
    cache_write_tokens: Option<i64>,
    #[serde(default)]
    cost_eur: Option<f64>,
    #[serde(default)]
    failures: Option<i64>,
    #[serde(default)]
    fallbacks: Option<i64>,
    #[serde(default)]
    latency_p50_ms: Option<f64>,
    #[serde(default)]
    latency_p95_ms: Option<f64>,
}

fn totals(row: &AggregateRow) -> UsageTotals {
    let input_tokens = row.input_tokens.unwrap_or_default().max(0) as u64;
    let output_tokens = row.output_tokens.unwrap_or_default().max(0) as u64;
    let cache_read_tokens = row.cache_read_tokens.unwrap_or_default().max(0) as u64;
    let cache_write_tokens = row.cache_write_tokens.unwrap_or_default().max(0) as u64;
    UsageTotals {
        requests: row.requests.unwrap_or_default().max(0) as u64,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens: input_tokens
            .saturating_add(output_tokens)
            .saturating_add(cache_read_tokens)
            .saturating_add(cache_write_tokens),
        cost_eur: row.cost_eur.unwrap_or_default().max(0.0),
        failures: row.failures.unwrap_or_default().max(0) as u64,
        fallbacks: row.fallbacks.unwrap_or_default().max(0) as u64,
        latency_p50_ms: None,
        latency_p95_ms: None,
    }
}

fn latency_ms(value: Option<f64>) -> Option<u64> {
    value
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map(|ms| ms.round() as u64)
}

/// Latency comes from a separate filtered query: legacy rows have no latency
/// and unmeasured internal calls record zero, which would skew percentiles.
fn with_latency(mut totals: UsageTotals, latency: Option<&AggregateRow>) -> UsageTotals {
    if let Some(row) = latency {
        totals.latency_p50_ms = latency_ms(row.latency_p50_ms);
        totals.latency_p95_ms = latency_ms(row.latency_p95_ms);
    }
    totals
}

const FIELDS: &str = "count() AS requests, math::sum(input_tokens) AS input_tokens, math::sum(output_tokens) AS output_tokens, math::sum(cache_read_tokens) AS cache_read_tokens, math::sum(cache_write_tokens) AS cache_write_tokens, math::sum(cost_eur) AS cost_eur, count(failure_category != NONE) AS failures, math::sum(fallback_count ?? 0) AS fallbacks";
const LATENCY: &str = "math::percentile(latency_ms, 50) AS latency_p50_ms, math::percentile(latency_ms, 95) AS latency_p95_ms";

/// Bounded monthly aggregates only. Prompts, responses and request identifiers
/// never leave the database through this statistics boundary. Failure
/// categories are Core-assigned identifiers, never free text.
fn month_statistics_query() -> String {
    let since = format!("ts >= {CURRENT_MONTH_START}");
    let measured = format!("{since} AND latency_ms > 0");
    format!(
        "SELECT {FIELDS} FROM llm_usage WHERE {since} GROUP ALL; \
         SELECT backend, {FIELDS} FROM llm_usage WHERE {since} GROUP BY backend ORDER BY cost_eur DESC; \
         SELECT backend, model, {FIELDS} FROM llm_usage WHERE {since} GROUP BY backend, model ORDER BY cost_eur DESC; \
         SELECT time::format(ts, '%Y-%m-%d') AS day, {FIELDS} FROM llm_usage WHERE {since} GROUP BY day ORDER BY day ASC; \
         SELECT failure_category, count() AS requests FROM llm_usage WHERE {since} AND failure_category != NONE GROUP BY failure_category ORDER BY requests DESC; \
         SELECT {LATENCY} FROM llm_usage WHERE {measured} GROUP ALL; \
         SELECT backend, {LATENCY} FROM llm_usage WHERE {measured} GROUP BY backend"
    )
}

/// Per-agent monthly aggregates. SurrealDB 2.x only aggregates top-level
/// functions in a grouped SELECT, so `last_used` is formatted as RFC 3339
/// text in an outer SELECT rather than relying on datetime deserialisation.
/// Agent IDs are Core-assigned identifiers, never free text.
fn month_agent_statistics_query() -> String {
    let since = format!("ts >= {CURRENT_MONTH_START} AND agent_id != NONE");
    format!(
        "SELECT *, time::format(last_used, '%+') AS last_used FROM \
         (SELECT agent_id, {FIELDS}, time::max(ts) AS last_used FROM llm_usage WHERE {since} GROUP BY agent_id) \
         ORDER BY cost_eur DESC; \
         SELECT agent_id, {LATENCY} FROM llm_usage WHERE {since} AND latency_ms > 0 GROUP BY agent_id"
    )
}

pub async fn month_statistics(db: &Database) -> Result<UsageStatistics, jarvis_store::StoreError> {
    let mut response = db
        .query(month_statistics_query())
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    let mut take = |index: usize| -> Result<Vec<AggregateRow>, jarvis_store::StoreError> {
        response
            .take(index)
            .map_err(jarvis_store::StoreError::schema)
    };
    let total_rows = take(0)?;
    let backend_rows = take(1)?;
    let model_rows = take(2)?;
    let daily_rows = take(3)?;
    let failure_rows = take(4)?;
    let total_latency = take(5)?;
    let backend_latency = take(6)?;
    Ok(UsageStatistics {
        totals: with_latency(
            total_rows.first().map(totals).unwrap_or_default(),
            total_latency.first(),
        ),
        by_backend: backend_rows
            .into_iter()
            .filter_map(|row| {
                let backend = row.backend.clone()?;
                let latency = backend_latency
                    .iter()
                    .find(|latency| latency.backend.as_ref() == Some(&backend));
                Some(UsageDimension {
                    totals: with_latency(totals(&row), latency),
                    backend,
                    model: None,
                })
            })
            .collect(),
        by_model: model_rows
            .into_iter()
            .filter_map(|row| {
                Some(UsageDimension {
                    backend: row.backend.clone()?,
                    model: row.model.clone(),
                    totals: totals(&row),
                })
            })
            .collect(),
        daily: daily_rows
            .into_iter()
            .filter_map(|row| {
                Some(DailyUsage {
                    day: row.day.clone()?,
                    totals: totals(&row),
                })
            })
            .collect(),
        failures_by_category: failure_rows
            .into_iter()
            .filter_map(|row| {
                Some(FailureCount {
                    category: row.failure_category?,
                    requests: row.requests.unwrap_or_default().max(0) as u64,
                })
            })
            .collect(),
    })
}

pub async fn month_agent_statistics(
    db: &Database,
) -> Result<Vec<AgentUsage>, jarvis_store::StoreError> {
    let mut response = db
        .query(month_agent_statistics_query())
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    let agent_rows: Vec<AggregateRow> =
        response.take(0).map_err(jarvis_store::StoreError::schema)?;
    let agent_latency: Vec<AggregateRow> =
        response.take(1).map_err(jarvis_store::StoreError::schema)?;
    Ok(agent_rows
        .into_iter()
        .filter_map(|row| {
            let agent_id = row.agent_id.clone()?;
            let latency = agent_latency
                .iter()
                .find(|latency| latency.agent_id.as_ref() == Some(&agent_id));
            Some(AgentUsage {
                totals: with_latency(totals(&row), latency),
                last_used: row.last_used,
                agent_id,
            })
        })
        .collect())
}

/// Persist a bounded long-task projection.  The Home Node's process-local gate
/// rejects concurrent oversubscription during execution; this durable record
/// makes crash recovery and stale-reservation cleanup observable.
pub async fn reserve_task(
    db: &Database,
    task_id: &str,
    user_id: Option<&str>,
    projected_cents: u64,
    ttl_seconds: u64,
) -> Result<(), jarvis_store::StoreError> {
    db.query(
        "CREATE llm_budget_reservations SET id = $id, task_id = $task_id, user_id = $user_id, \
         projected_cents = $projected_cents, status = 'active', created_at = time::now(), \
         expires_at = time::now() + <duration>$ttl RETURN NONE",
    )
    .bind(json!({
        "id": Uuid::now_v7().to_string(), "task_id": task_id, "user_id": user_id,
        "projected_cents": projected_cents as i64, "ttl": format!("{}s", ttl_seconds),
    }))
    .await
    .map_err(jarvis_store::StoreError::schema)?
    .check()
    .map_err(jarvis_store::StoreError::schema)?;
    Ok(())
}

/// Idempotently releases a reservation; an expired/released task can never be
/// revived by this helper.
pub async fn release_task(db: &Database, task_id: &str) -> Result<(), jarvis_store::StoreError> {
    db.query(
        "UPDATE llm_budget_reservations SET status = 'released', released_at = time::now() \
         WHERE task_id = $task_id AND status = 'active' RETURN NONE",
    )
    .bind(json!({ "task_id": task_id }))
    .await
    .map_err(jarvis_store::StoreError::schema)?
    .check()
    .map_err(jarvis_store::StoreError::schema)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_query_selects_only_bounded_non_secret_dimensions() {
        let query = format!(
            "{}; {}",
            month_statistics_query(),
            month_agent_statistics_query()
        );
        // Agent IDs and failure categories are owner-approved bounded
        // dimensions; free-text and per-request fields stay out.
        for forbidden in [
            "request_id",
            "routing_mode",
            "requested_route",
            "actual_provider",
            "prompt",
            "response",
        ] {
            assert!(!query.contains(forbidden));
        }
        assert!(query.contains("GROUP BY backend, model"));
        assert!(query.contains("GROUP BY day"));
        assert!(query.contains("GROUP BY agent_id"));
        assert!(query.contains("GROUP BY failure_category"));
        // Unmeasured (zero/legacy NONE) latencies never enter a percentile.
        assert_eq!(query.matches("latency_ms > 0").count(), 3);
        assert!(query.contains("time::format(last_used, '%+')"));
        assert_eq!(query.matches("math::percentile(").count(), 6);
        assert!(query.contains("time::group(time::now(), 'month')"));
        assert!(!query.contains("1mo"));
    }

    #[test]
    fn every_month_query_uses_calendar_month_grouping() {
        let total = month_total_query();
        let statistics = month_statistics_query();
        assert!(total.ends_with("GROUP ALL"));
        assert!(statistics.contains("GROUP ALL;"));
        for query in [
            total,
            month_breakdown_query(),
            statistics,
            month_agent_statistics_query(),
        ] {
            assert!(query.contains(CURRENT_MONTH_START));
            assert!(!query.contains("1mo"));
        }
    }

    #[test]
    fn aggregates_map_failures_fallbacks_and_only_finite_latency() {
        let row = AggregateRow {
            requests: Some(4),
            failures: Some(1),
            fallbacks: Some(3),
            latency_p50_ms: Some(120.4),
            latency_p95_ms: Some(f64::NAN),
            ..Default::default()
        };
        let mapped = with_latency(totals(&row), Some(&row));
        assert_eq!(
            (mapped.requests, mapped.failures, mapped.fallbacks),
            (4, 1, 3)
        );
        assert_eq!(mapped.latency_p50_ms, Some(120));
        assert_eq!(mapped.latency_p95_ms, None);
        assert_eq!(latency_ms(Some(f64::NAN)), None);
        assert_eq!(latency_ms(Some(-1.0)), None);
        assert_eq!(with_latency(totals(&row), None).latency_p50_ms, None);
    }
}
