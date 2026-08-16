//! Jarvis API / BFF — process entrypoint.
//!
//! Loads config, opens the PostgreSQL pool, applies migrations, and serves the
//! router from `jarvis_api::build_router`.

use std::time::Duration;

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use jarvis_api::{build_router, AppState, BrainAvailability};
use jarvis_config::AppConfig;
use sqlx::postgres::PgPoolOptions;

/// `Some(trimmed)` for a non-empty secret, `None` otherwise — so an unset key
/// disables its backend instead of building a provider that always 401s.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
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

    if let Err(error) = sqlx::migrate!("../../migrations").run(&db).await {
        if config.environment == "production" {
            tracing::error!(%error, "database migrations failed; refusing to start in production");
            return Err(error.into());
        }
        tracing::warn!(%error, "database migrations did not run; continuing outside production");
    } else {
        tracing::info!("database migrations up to date");
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
        anthropic_model_hard: config.llm_model_hard.clone(),
        anthropic_model_cheap: config.llm_model_cheap.clone(),
        ollama_model: config.llm_ollama_model.clone(),
        has_openai_key: !config.llm_openai_api_key.trim().is_empty(),
        openai_model: config.llm_openai_model.clone(),
        openai_model_hard: config.llm_openai_model_hard.clone(),
        openai_model_cheap: config.llm_openai_model_cheap.clone(),
        has_deepseek_key: !config.llm_deepseek_api_key.trim().is_empty(),
        deepseek_model: config.llm_deepseek_model.clone(),
        deepseek_model_hard: config.llm_deepseek_model_hard.clone(),
        deepseek_model_cheap: config.llm_deepseek_model_cheap.clone(),
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
        openai: jarvis_llm::OpenAiBackend {
            api_key: non_empty(&config.llm_openai_api_key),
            base_url: config.llm_openai_base_url.clone(),
            model_default: config.llm_openai_model.clone(),
            model_hard: config.llm_openai_model_hard.clone(),
            model_cheap: config.llm_openai_model_cheap.clone(),
        },
        deepseek: jarvis_llm::OpenAiBackend {
            api_key: non_empty(&config.llm_deepseek_api_key),
            base_url: config.llm_deepseek_base_url.clone(),
            model_default: config.llm_deepseek_model.clone(),
            model_hard: config.llm_deepseek_model_hard.clone(),
            model_cheap: config.llm_deepseek_model_cheap.clone(),
        },
    };
    // Cost guardrail (ADR-027): a hard monthly EUR cap on metered API backends.
    // Seed the in-memory spend counter from this month's DB total so the gate is
    // correct across restarts; the router refuses paid calls once it's reached.
    let budget_cents = (config.llm_monthly_budget_eur * 100.0).round().max(0.0) as u64;
    let spent_eur = jarvis_usage::month_total_eur(&db).await.unwrap_or(0.0);
    let spent_cents = Arc::new(AtomicU64::new((spent_eur * 100.0).round().max(0.0) as u64));
    tracing::info!(
        budget_eur = config.llm_monthly_budget_eur,
        spent_eur,
        "llm monthly budget"
    );

    let llm = match config.llm_provider.to_ascii_lowercase().as_str() {
        "router" | "auto" => {
            let availability = Arc::new(BrainAvailability {
                registry: registry.clone(),
                spent_cents: spent_cents.clone(),
                budget_cents,
            });
            // The router picks the cheapest sufficient model from the catalog
            // of *available* models (ADR-028 fase 2).
            let catalog = jarvis_api::router_catalog(&registry);
            jarvis_llm::build_router(provider_cfg, availability, catalog)
        }
        _ => jarvis_llm::build_provider(provider_cfg),
    };
    tracing::info!(brain = %llm.label(), "llm brain configured");

    // Load Jarvis' identity (core/Jarvis.md) as the system prompt — the single
    // source of truth for "what Jarvis is". Falls back to a built-in persona if
    // the file is absent, so the brain always has an identity.
    let (jarvis_system, persona_loaded) = jarvis_api::load_persona(&config.llm_persona_path);
    if persona_loaded {
        tracing::info!(path = %config.llm_persona_path, chars = jarvis_system.len(), "Jarvis persona loaded");
    } else {
        tracing::warn!(path = %config.llm_persona_path, "no persona file; using built-in fallback persona");
    }

    // Record the resolved brain for display (Status "AI-RESOURCES") and refresh.
    let active_brain = llm.label().to_string();
    registry_input.active_brain = active_brain.clone();
    if let Ok(mut reg) = registry.write() {
        reg.active_brain = active_brain;
    }

    // Agentic execution (ADR-029 4a) — off by default. Build the sandbox only
    // when a workspace root is configured and valid; otherwise actions are refused.
    let agent_sandbox = {
        let root = config.agent_workspace_root.trim();
        if root.is_empty() {
            None
        } else {
            match jarvis_agent::Sandbox::new(root) {
                Ok(mut sb) => {
                    // Second, deliberate opt-in: the Claude Code executor (4c).
                    if config.agent_claude_code_enabled {
                        sb = sb.with_claude_code(jarvis_agent::ClaudeCodeCfg {
                            bin: config.agent_claude_code_bin.clone(),
                            model: config.agent_claude_code_model.clone(),
                        });
                    }
                    tracing::info!(
                        root = %sb.root().display(),
                        enabled = config.agent_enabled,
                        claude_code = config.agent_claude_code_enabled,
                        "agent sandbox ready"
                    );
                    Some(Arc::new(sb))
                }
                Err(e) => {
                    tracing::warn!(root, error = %e, "invalid agent workspace root; agent disabled");
                    None
                }
            }
        }
    };
    if config.agent_enabled && agent_sandbox.is_some() {
        tracing::warn!(
            "AGENTIC EXECUTION IS ENABLED (mutaties achter getekende goedkeuring, ADR-029 4a/4b)"
        );
        if config.agent_claude_code_enabled {
            tracing::warn!("CLAUDE CODE EXECUTOR IS ENABLED (4c) — Jarvis mag bestanden bewerken via headless claude, achter goedkeuring");
        }
    }

    let trusted_proxy_ips = config.trusted_proxy_ips().map_err(anyhow::Error::msg)?;

    let state = AppState {
        db,
        environment: config.environment.clone(),
        ibkr_gateway_url: config.ibkr_gateway_url.clone(),
        llm,
        llm_max_tokens: config.llm_max_tokens,
        jarvis_system,
        speech,
        speech_verify_threshold: config.speech_verify_threshold,
        registry,
        registry_input: Arc::new(registry_input),
        budget_cents,
        spent_cents,
        eur_per_usd: config.llm_eur_per_usd,
        agent_enabled: config.agent_enabled,
        agent_sandbox,
        rate_limiter: std::sync::Arc::new(jarvis_api::RateLimiter::new()),
        auth_limits: jarvis_api::AuthLimits {
            enroll_per_min: config.auth_rate_enroll_per_min,
            challenge_per_min: config.auth_rate_challenge_per_min,
            login_per_min: config.auth_rate_login_per_min,
            login_max_failures: config.auth_login_max_failures,
            login_lock_secs: config.auth_login_lock_secs,
        },
        trusted_proxy_hops: config.trusted_proxy_hops,
        trusted_proxy_ips: Arc::new(trusted_proxy_ips),
    };

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "jarvis-api listening");
    // `into_make_service_with_connect_info` exposes the peer address so the
    // rate limiter can key per client IP.
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}
