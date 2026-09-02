use serde_json::json;
use uuid::Uuid;

use jarvis_store::Database;

use super::{DailyUsage, UsageDimension, UsageEntry, UsageStatistics, UsageTotals};

const CURRENT_MONTH_START: &str = "time::group(time::now(), 'month')";

fn month_total_query() -> String {
    format!("SELECT math::sum(cost_eur) AS total FROM llm_usage WHERE ts >= {CURRENT_MONTH_START}")
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
         routing_mode = $routing_mode, quality_tier = $quality_tier, agent_id = $agent_id, latency_ms = $latency_ms, \
         status = $status, failure_category = $failure_category, fallback_count = $fallback_count, \
         input_tokens = $input_tokens, output_tokens = $output_tokens, cache_read_tokens = $cache_read_tokens, \
         cache_write_tokens = $cache_write_tokens, cost_eur = $cost_eur RETURN NONE",
    )
    .bind(json!({
        "id": Uuid::now_v7().to_string(), "request_id": entry.request_id, "backend": entry.backend, "model": entry.model,
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

#[derive(serde::Deserialize)]
struct AggregateRow {
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    day: Option<String>,
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
    }
}

/// Bounded monthly aggregates only. Prompts, responses and request identifiers
/// never leave the database through this statistics boundary.
fn month_statistics_query() -> String {
    const FIELDS: &str = "count() AS requests, math::sum(input_tokens) AS input_tokens, math::sum(output_tokens) AS output_tokens, math::sum(cache_read_tokens) AS cache_read_tokens, math::sum(cache_write_tokens) AS cache_write_tokens, math::sum(cost_eur) AS cost_eur";
    let since = format!("ts >= {CURRENT_MONTH_START}");
    format!(
        "SELECT {FIELDS} FROM llm_usage WHERE {since}; \
         SELECT backend, {FIELDS} FROM llm_usage WHERE {since} GROUP BY backend ORDER BY cost_eur DESC; \
         SELECT backend, model, {FIELDS} FROM llm_usage WHERE {since} GROUP BY backend, model ORDER BY cost_eur DESC; \
         SELECT time::format(ts, '%Y-%m-%d') AS day, {FIELDS} FROM llm_usage WHERE {since} GROUP BY day ORDER BY day ASC"
    )
}

pub async fn month_statistics(db: &Database) -> Result<UsageStatistics, jarvis_store::StoreError> {
    let mut response = db
        .query(month_statistics_query())
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    let total_rows: Vec<AggregateRow> =
        response.take(0).map_err(jarvis_store::StoreError::schema)?;
    let backend_rows: Vec<AggregateRow> =
        response.take(1).map_err(jarvis_store::StoreError::schema)?;
    let model_rows: Vec<AggregateRow> =
        response.take(2).map_err(jarvis_store::StoreError::schema)?;
    let daily_rows: Vec<AggregateRow> =
        response.take(3).map_err(jarvis_store::StoreError::schema)?;
    Ok(UsageStatistics {
        totals: total_rows.first().map(totals).unwrap_or_default(),
        by_backend: backend_rows
            .into_iter()
            .filter_map(|row| {
                Some(UsageDimension {
                    backend: row.backend.clone()?,
                    model: None,
                    totals: totals(&row),
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
    })
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
        let query = month_statistics_query();
        for forbidden in [
            "request_id",
            "agent_id",
            "routing_mode",
            "failure_category",
            "prompt",
            "response",
        ] {
            assert!(!query.contains(forbidden));
        }
        assert!(query.contains("GROUP BY backend, model"));
        assert!(query.contains("GROUP BY day"));
        assert!(query.contains("time::group(time::now(), 'month')"));
        assert!(!query.contains("1mo"));
    }

    #[test]
    fn every_month_query_uses_calendar_month_grouping() {
        for query in [
            month_total_query(),
            month_breakdown_query(),
            month_statistics_query(),
        ] {
            assert!(query.contains(CURRENT_MONTH_START));
            assert!(!query.contains("1mo"));
        }
    }
}
