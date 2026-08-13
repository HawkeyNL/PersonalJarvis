//! Jarvis API / BFF — process entrypoint.
//!
//! Loads config, opens the PostgreSQL pool, applies migrations, and serves the
//! router from `jarvis_api::build_router`.

use std::time::Duration;

use std::sync::{Arc, RwLock};

use jarvis_api::{build_router, AppState, RegistryAvailability};
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

    // Server-side speech (STT + speaker verification). `stub` by default; set
    // provider to `whisper` (with --features speech-whisper) for real STT.
    let speech = jarvis_speech::build_engine(&jarvis_speech::EngineConfig {
        provider: config.speech_provider.clone(),
        whisper_model: config.speech_whisper_model.clone(),
        whisper_language: config.speech_whisper_language.clone(),
    });
    tracing::info!(speech = %speech.label(), "speech engine configured");

    // Resource/agent registry — Jarvis' "instant memory" of brains + host
    // (ADR-027). Collected first so the router can consult live availability
    // through it; `active_brain` is filled in once the brain is wired.
    let mut registry_input = jarvis_registry::CollectInput {
        llm_provider: config.llm_provider.clone(),
        claude_cli_bin: config.llm_claude_cli_bin.clone(),
        has_api_key: !config.llm_api_key.trim().is_empty(),
        anthropic_model: config.llm_model.clone(),
        ollama_model: config.llm_ollama_model.clone(),
        speech_provider: config.speech_provider.clone(),
        whisper_model: config.speech_whisper_model.clone(),
        active_brain: String::new(),
    };
    let registry = jarvis_registry::collect(&registry_input).await;
    tracing::info!(
        cpu_cores = registry.host.cpu_cores,
        brains = registry.brains.len(),
        "resource registry collected"
    );
    let registry = Arc::new(RwLock::new(registry));

    // Wire up the brain (DEC-001). The API key never leaves the backend. In
    // `router`/`auto` mode the router routes per task, consulting the registry
    // for live availability (ADR-027) via `RegistryAvailability`.
    let provider_cfg = jarvis_llm::ProviderConfig {
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
        claude_cli_bin: config.llm_claude_cli_bin.clone(),
    };
    let llm = match config.llm_provider.to_ascii_lowercase().as_str() {
        "router" | "auto" => {
            let availability = Arc::new(RegistryAvailability(registry.clone()));
            jarvis_llm::build_router(provider_cfg, availability)
        }
        _ => jarvis_llm::build_provider(provider_cfg),
    };
    tracing::info!(brain = %llm.label(), "llm brain configured");

    // Record the resolved brain for display (Status "AI-RESOURCES") and refresh.
    let active_brain = llm.label().to_string();
    registry_input.active_brain = active_brain.clone();
    if let Ok(mut reg) = registry.write() {
        reg.active_brain = active_brain;
    }

    let state = AppState {
        db,
        environment: config.environment.clone(),
        ibkr_gateway_url: config.ibkr_gateway_url.clone(),
        llm,
        llm_max_tokens: config.llm_max_tokens,
        speech,
        speech_verify_threshold: config.speech_verify_threshold,
        registry,
        registry_input: Arc::new(registry_input),
    };

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "jarvis-api listening");
    axum::serve(listener, build_router(state)).await?;

    Ok(())
}
