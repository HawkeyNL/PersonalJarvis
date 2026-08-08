//! Jarvis API / BFF — Axum HTTP entrypoint.
//!
//! Fase 0 skeleton (JAR-001): liveness/readiness endpoints, structured
//! logging, typed configuration and a PostgreSQL pool. No business logic and
//! no broker access yet — this is the "working empty application".

use std::time::Duration;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower_http::trace::TraceLayer;

use jarvis_config::AppConfig;

/// Shared, cheaply-cloneable application state.
#[derive(Clone)]
struct AppState {
    db: PgPool,
    environment: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` for local development if present (ignored in production).
    let _ = dotenvy::dotenv();

    let config = AppConfig::load()?;
    jarvis_observability::init(config.log_json);

    // `config`'s Debug impl redacts the database_url, so this is safe to log.
    tracing::info!(?config, "starting jarvis-api");

    // Lazy pool: the process starts even if Postgres is not up yet; readiness
    // is reported separately via `/readyz`.
    let db = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy(&config.database_url)?;

    // Apply embedded migrations at startup. A failure here is logged but not
    // fatal for the skeleton, so `/livez` keeps answering while Postgres boots.
    match sqlx::migrate!("../../migrations").run(&db).await {
        Ok(()) => tracing::info!("database migrations up to date"),
        Err(e) => tracing::warn!(error = %e, "migrations did not run (is postgres up?)"),
    }

    let state = AppState {
        db,
        environment: config.environment.clone(),
    };

    let app = Router::new()
        .route("/", get(root))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "jarvis-api listening");
    axum::serve(listener, app).await?;

    Ok(())
}

async fn root() -> Json<Value> {
    Json(json!({ "service": "jarvis-api", "status": "ok" }))
}

/// Liveness probe: the process is running. Never touches external systems.
async fn livez() -> Json<Value> {
    Json(json!({ "status": "alive" }))
}

/// Readiness probe: confirms the database is reachable.
async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match sqlx::query("SELECT 1").fetch_one(&state.db).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ready", "environment": state.environment })),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "degraded", "error": e.to_string() })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn livez_reports_alive() {
        let Json(body) = livez().await;
        assert_eq!(body["status"], "alive");
    }

    #[tokio::test]
    async fn root_reports_service_name() {
        let Json(body) = root().await;
        assert_eq!(body["service"], "jarvis-api");
    }
}
