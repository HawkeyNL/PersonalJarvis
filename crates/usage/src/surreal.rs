use serde_json::json;
use uuid::Uuid;

use jarvis_store::Database;

use super::UsageEntry;

pub async fn record(db: &Database, entry: &UsageEntry) -> Result<(), jarvis_store::StoreError> {
    db.query(
        "CREATE llm_usage SET id = $id, ts = time::now(), backend = $backend, model = $model, \
         input_tokens = $input_tokens, output_tokens = $output_tokens, cache_read_tokens = $cache_read_tokens, \
         cache_write_tokens = $cache_write_tokens, cost_eur = $cost_eur RETURN NONE",
    )
    .bind(json!({
        "id": Uuid::now_v7().to_string(), "backend": entry.backend, "model": entry.model,
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
