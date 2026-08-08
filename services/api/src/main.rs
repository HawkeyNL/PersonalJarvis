//! Jarvis API / BFF — process entrypoint.
//!
//! Loads config, opens the PostgreSQL pool, applies migrations, and serves the
//! router from `jarvis_api::build_router`.

use std::time::Duration;

use jarvis_api::{build_router, AppState};
use jarvis_config::AppConfig;
use sqlx::postgres::PgPoolOptions;

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

    match sqlx::migrate!("../../migrations").run(&db).await {
        Ok(()) => tracing::info!("database migrations up to date"),
        Err(e) => tracing::warn!(error = %e, "migrations did not run (is postgres up?)"),
    }

    let state = AppState {
        db,
        environment: config.environment.clone(),
        ibkr_gateway_url: config.ibkr_gateway_url.clone(),
    };

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "jarvis-api listening");
    axum::serve(listener, build_router(state)).await?;

    Ok(())
}
