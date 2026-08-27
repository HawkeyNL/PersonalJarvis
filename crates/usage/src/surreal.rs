use serde_json::json;
use uuid::Uuid;

use jarvis_store::Database;

use super::UsageEntry;

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
        .query("SELECT math::sum(cost_eur) AS total FROM llm_usage WHERE ts >= time::floor(time::now(), 1mo)")
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
        .query(
            "SELECT backend, math::sum(cost_eur) AS total FROM llm_usage \
         WHERE ts >= time::floor(time::now(), 1mo) GROUP BY backend ORDER BY total DESC",
        )
        .await
        .map_err(jarvis_store::StoreError::schema)?;
    let rows: Vec<Row> = response.take(0).map_err(jarvis_store::StoreError::schema)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.backend, row.total.unwrap_or(0.0)))
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
