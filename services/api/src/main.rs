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

    // Wire up the brain (DEC-001). The API key never leaves the backend.
    let llm = jarvis_llm::build_provider(jarvis_llm::ProviderConfig {
        provider: config.llm_provider.clone(),
        api_key: {
            let key = config.llm_api_key.trim();
            (!key.is_empty()).then(|| key.to_string())
        },
        anthropic_base_url: config.llm_anthropic_base_url.clone(),
        model_default: config.llm_model.clone(),
        model_hard: config.llm_model_hard.clone(),
        model_cheap: config.llm_model_cheap.clone(),
        ollama_url: config.llm_ollama_url.clone(),
        ollama_model: config.llm_ollama_model.clone(),
    });
    tracing::info!(brain = %llm.label(), "llm brain configured");

    // Server-side speech (STT + speaker verification). Stub until a real model
    // is plugged in behind the SpeechEngine trait.
    let speech = jarvis_speech::build_engine(&jarvis_speech::EngineConfig {
        provider: config.speech_provider.clone(),
    });
    tracing::info!(speech = %speech.label(), "speech engine configured");

    let state = AppState {
        db,
        environment: config.environment.clone(),
        ibkr_gateway_url: config.ibkr_gateway_url.clone(),
        llm,
        llm_max_tokens: config.llm_max_tokens,
        speech,
        speech_verify_threshold: config.speech_verify_threshold,
    };

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "jarvis-api listening");
    axum::serve(listener, build_router(state)).await?;

    Ok(())
}
