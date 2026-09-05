//! Jarvis API / BFF — process entrypoint.
//!
//! Loads config, opens SurrealDB, applies the versioned baseline, and serves the
//! router from `jarvis_api::build_router`.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use jarvis_api::{build_router, AppState, BrainAvailability};
use jarvis_config::AppConfig;

/// `Some(trimmed)` for a non-empty secret, `None` otherwise — so an unset key
/// disables its backend instead of building a provider that always 401s.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn load_huggingface_catalog(
    path: &str,
    enforce_production_boundary: bool,
) -> Result<jarvis_llm::HuggingFaceCatalog, String> {
    if enforce_production_boundary {
        let path = Path::new(path);
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("Hugging Face catalog is unavailable: {error}"))?;
        let parent = path
            .parent()
            .ok_or_else(|| "Hugging Face catalog has no trusted parent".to_string())?;
        let parent_metadata = fs::symlink_metadata(parent)
            .map_err(|error| format!("Hugging Face catalog parent is unavailable: {error}"))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != 0
            || metadata.gid() != parent_metadata.gid()
            || metadata.permissions().mode() & 0o777 != 0o640
        {
            return Err("Hugging Face catalog permissions are unsafe".into());
        }
    }
    jarvis_llm::HuggingFaceCatalog::load(path)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A production service reads only its root-managed EnvironmentFile. Loading
    // a nearby `.env` there would turn a release-directory file into a secret
    // source. Development may still use the convenient local file.
    let mut config = AppConfig::load()?;
    if !config.environment.eq_ignore_ascii_case("production") {
        let _ = dotenvy::dotenv();
        config = AppConfig::load()?;
    }
    config
        .validate_runtime_security()
        .map_err(anyhow::Error::msg)?;
    jarvis_observability::init(config.log_json);

    // `config`'s Debug impl redacts database credentials, so this is safe to log.
    tracing::info!(?config, "starting jarvis-api");

    // Core requires an authenticated, private SurrealDB connection. It never
    // receives a root credential and refuses startup if the schema is unknown.
    let db = jarvis_store::connect(
        &config.surreal_endpoint,
        &config.surreal_namespace,
        &config.surreal_database,
        &config.surreal_username,
        &config.surreal_password,
    )
    .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    tracing::info!("SurrealDB schema verified");

    // Server-side speech (STT + speaker verification). `stub` by default; set
    // provider to `whisper` (with --features speech-whisper) for real STT.
    let speech = jarvis_speech::build_engine(&jarvis_speech::EngineConfig {
        provider: config.speech_provider.clone(),
        whisper_model: config.speech_whisper_model.clone(),
        whisper_language: config.speech_whisper_language.clone(),
    });
    tracing::info!(speech = %speech.label(), "speech engine configured");

    let mut model_policy = match jarvis_llm::ModelAccessPolicy::load(&config.llm_model_policy_path)
    {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(path = %config.llm_model_policy_path, %error, "model access policy unavailable; remote models disabled");
            jarvis_llm::ModelAccessPolicy::deny_by_default()
        }
    };
    let hf_catalog = load_huggingface_catalog(
        &config.llm_huggingface_catalog_path,
        config.environment.eq_ignore_ascii_case("production"),
    )
    .map_err(|error| {
        tracing::info!(%error, "Hugging Face catalog unavailable");
        error
    })
    .ok();
    for entry in model_policy
        .models
        .iter_mut()
        .filter(|entry| entry.provider == "huggingface" && entry.enabled)
    {
        let route = entry
            .route
            .as_deref()
            .unwrap_or(&config.llm_huggingface_route);
        if !matches!(route, "auto" | "fastest" | "cheapest" | "preferred")
            && !hf_catalog
                .as_ref()
                .is_some_and(|catalog| catalog.route_available(&entry.model, route))
        {
            tracing::warn!(model = %entry.model, requested_hf_route = %route, "disabled unavailable explicit Hugging Face route");
            entry.enabled = false;
        }
    }

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
        has_xai_key: !config.llm_xai_api_key.trim().is_empty(),
        xai_model: config.llm_xai_model.clone(),
        xai_model_hard: config.llm_xai_model_hard.clone(),
        xai_model_cheap: config.llm_xai_model_cheap.clone(),
        has_zai_key: !config.llm_zai_api_key.trim().is_empty(),
        zai_model: config.llm_zai_model.clone(),
        zai_model_hard: config.llm_zai_model_hard.clone(),
        zai_model_cheap: config.llm_zai_model_cheap.clone(),
        has_ollama_cloud_key: !config.llm_ollama_cloud_api_key.trim().is_empty(),
        ollama_cloud_model: config.llm_ollama_cloud_model.clone(),
        ollama_cloud_model_hard: config.llm_ollama_cloud_model_hard.clone(),
        ollama_cloud_model_cheap: config.llm_ollama_cloud_model_cheap.clone(),
        has_huggingface_key: !config.llm_huggingface_api_key.trim().is_empty(),
        huggingface_model: config.llm_huggingface_model.clone(),
        huggingface_model_hard: config.llm_huggingface_model_hard.clone(),
        huggingface_model_cheap: config.llm_huggingface_model_cheap.clone(),
        policy_models: model_policy
            .models
            .iter()
            .map(|entry| (entry.provider.clone(), entry.model.clone()))
            .collect(),
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
        xai: jarvis_llm::OpenAiBackend {
            api_key: non_empty(&config.llm_xai_api_key),
            base_url: config.llm_xai_base_url.clone(),
            model_default: config.llm_xai_model.clone(),
            model_hard: config.llm_xai_model_hard.clone(),
            model_cheap: config.llm_xai_model_cheap.clone(),
        },
        zai: jarvis_llm::OpenAiBackend {
            api_key: non_empty(&config.llm_zai_api_key),
            base_url: config.llm_zai_base_url.clone(),
            model_default: config.llm_zai_model.clone(),
            model_hard: config.llm_zai_model_hard.clone(),
            model_cheap: config.llm_zai_model_cheap.clone(),
        },
        ollama_cloud: jarvis_llm::OpenAiBackend {
            api_key: non_empty(&config.llm_ollama_cloud_api_key),
            base_url: config.llm_ollama_cloud_base_url.clone(),
            model_default: config.llm_ollama_cloud_model.clone(),
            model_hard: config.llm_ollama_cloud_model_hard.clone(),
            model_cheap: config.llm_ollama_cloud_model_cheap.clone(),
        },
        huggingface: jarvis_llm::HuggingFaceBackend {
            api_key: non_empty(&config.llm_huggingface_api_key),
            base_url: config.llm_huggingface_base_url.clone(),
            model_default: config.llm_huggingface_model.clone(),
            model_hard: config.llm_huggingface_model_hard.clone(),
            model_cheap: config.llm_huggingface_model_cheap.clone(),
            route_default: config.llm_huggingface_route.clone(),
            route_hard: config.llm_huggingface_route_hard.clone(),
            route_cheap: config.llm_huggingface_route_cheap.clone(),
        },
    };
    // Cost guardrail (ADR-027): a hard monthly EUR cap on metered API backends.
    // Seed the in-memory spend counter from this month's DB total so the gate is
    // correct across restarts; the router refuses paid calls once it's reached.
    let budget_cents = (config.llm_monthly_budget_eur * 100.0).round().max(0.0) as u64;
    let spent_eur = jarvis_usage::month_total_eur(&db).await.unwrap_or(0.0);
    let spent_cents = Arc::new(AtomicU64::new((spent_eur * 100.0).round().max(0.0) as u64));
    let budget_book = Arc::new(jarvis_usage::BudgetBook::new(
        jarvis_usage::BudgetLimits {
            monthly_soft_cents: (config.llm_monthly_soft_budget_eur * 100.0)
                .round()
                .max(0.0) as u64,
            monthly_hard_cents: budget_cents,
            per_request_hard_cents: (config.llm_request_hard_cap_eur * 100.0).round().max(0.0)
                as u64,
        },
        spent_cents.load(std::sync::atomic::Ordering::Relaxed),
    ));
    tracing::info!(
        budget_eur = config.llm_monthly_budget_eur,
        spent_eur,
        "llm monthly budget"
    );

    // All production requests go through this one router.  Keeping legacy
    // single-provider configuration values must not create an allowlist or
    // budget bypass around the owner model policy.
    let availability = Arc::new(BrainAvailability {
        registry: registry.clone(),
        spent_cents: spent_cents.clone(),
        budget_cents,
    });
    let catalog = jarvis_api::router_catalog(&registry);
    let mut pricing_registry = match jarvis_usage::PricingRegistry::load_with_builtin(
        &config.llm_pricing_registry_path,
    ) {
        Ok(registry) => registry,
        Err(error) => {
            tracing::warn!(path = %config.llm_pricing_registry_path, %error, "pricing registry unavailable; using conservative built-in pricing");
            jarvis_usage::PricingRegistry::builtin()
        }
    };
    match hf_catalog.as_ref() {
        Some(hf_catalog) => {
            let mut remaining_hf_prices = 2_000_usize.saturating_sub(pricing_registry.models.len());
            for entry in model_policy
                .models
                .iter()
                .filter(|entry| entry.provider == "huggingface")
            {
                let price = entry.route.as_deref().map_or_else(
                    || {
                        hf_catalog.conservative_price_for_routes(
                            &entry.model,
                            [
                                config.llm_huggingface_route.as_str(),
                                config.llm_huggingface_route_cheap.as_str(),
                                config.llm_huggingface_route_hard.as_str(),
                            ],
                        )
                    },
                    |route| hf_catalog.conservative_price(&entry.model, route),
                );
                if let Some((input, output)) = price {
                    if !pricing_registry
                        .models
                        .iter()
                        .any(|price| price.provider == "huggingface" && price.model == entry.model)
                        && remaining_hf_prices > 0
                    {
                        pricing_registry.models.push(jarvis_usage::PricingEntry {
                            provider: "huggingface".into(),
                            model: entry.model.clone(),
                            input_per_million_usd: input,
                            output_per_million_usd: output,
                            cache_read_per_million_usd: Some(input),
                            price_status: if entry.route.as_deref().is_some_and(|route| {
                                !matches!(route, "auto" | "fastest" | "cheapest" | "preferred")
                            }) {
                                jarvis_usage::PriceStatus::Estimated
                            } else {
                                jarvis_usage::PriceStatus::Conservative
                            },
                        });
                        remaining_hf_prices -= 1;
                    }
                }
            }
            pricing_registry.source =
                format!("{} + huggingface-conservative", pricing_registry.source);
        }
        None => tracing::info!(
            "Hugging Face catalog unavailable; conservative unknown pricing remains active"
        ),
    }
    let llm = jarvis_llm::build_router_with_policy(
        provider_cfg,
        availability,
        catalog,
        model_policy.clone(),
    );
    tracing::info!(brain = %llm.label(), "llm brain configured");

    // Load Jarvis' protected identity (/etc/jarvis/Jarvis.md) as the system prompt — the single
    // source of truth for "what Jarvis is". Falls back to a built-in persona if
    // the file is absent, so the brain always has an identity.
    let (jarvis_system, persona_loaded) = jarvis_api::load_persona(&config.llm_persona_path);
    if persona_loaded {
        tracing::info!(path = %config.llm_persona_path, chars = jarvis_system.len(), "Jarvis persona loaded");
    } else {
        tracing::warn!(path = %config.llm_persona_path, "no persona file; using built-in fallback persona");
    }

    // Private agent definitions are installed by the owner as an immutable
    // bundle. Public releases never contain them. Production fails closed when
    // the registry is absent or malformed; development can still run without a
    // private checkout and uses the generic Core only.
    let agent_bundle_path = std::env::var("JARVIS_AGENT_BUNDLE_PATH")
        .unwrap_or_else(|_| "/var/lib/jarvis/agents/current".to_string());
    match jarvis_core::AgentRegistry::load(&agent_bundle_path) {
        Ok(agent_registry) => tracing::info!(
            bundle = %agent_registry.bundle_id(),
            agent_count = agent_registry.agents().len(),
            "private AgentRegistry loaded"
        ),
        Err(error) if config.environment.eq_ignore_ascii_case("production") => {
            anyhow::bail!("protected AgentRegistry is unavailable: {error}");
        }
        Err(error) => {
            tracing::warn!(path = %agent_bundle_path, %error, "private AgentRegistry unavailable in development")
        }
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
                Ok(sb) => {
                    // Direct host-process Claude Code is intentionally retired.
                    // A future approved OpenSandbox-backed broker owns coding
                    // execution; the flag cannot re-enable a host fallback.
                    if config.agent_claude_code_enabled {
                        tracing::warn!(
                            "JARVIS_AGENT_CLAUDE_CODE_ENABLED is ignored: direct execution requires OpenSandbox"
                        );
                    }
                    tracing::info!(
                        root = %sb.root().display(),
                        enabled = config.agent_enabled,
                        claude_code = false,
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
    }

    let trusted_proxy_ips = config.trusted_proxy_ips().map_err(anyhow::Error::msg)?;
    let bootstrap_enrollment = config.bootstrap_enrollment().map_err(anyhow::Error::msg)?;
    let update_mirror_root = std::env::var("JARVIS_APP_UPDATE_MIRROR_ROOT")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let update_public_base = std::env::var("JARVIS_APP_UPDATE_PUBLIC_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let app_update_mirror = match (update_mirror_root, update_public_base) {
        (None, None) => None,
        (Some(root), Some(public_base)) => Some(
            jarvis_api::AppUpdateMirror::new(root, &public_base)
                .map_err(anyhow::Error::msg)?,
        ),
        _ => anyhow::bail!(
            "JARVIS_APP_UPDATE_MIRROR_ROOT and JARVIS_APP_UPDATE_PUBLIC_BASE_URL must be configured together"
        ),
    };
    let app_update_mirror = match std::env::var("JARVIS_MOBILE_APP_UPDATE_MIRROR_ROOT")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(root) => Some(
            app_update_mirror
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "mobile mirror requires the application update origin configuration"
                    )
                })?
                .with_mobile_root(root)
                .map_err(anyhow::Error::msg)?,
        ),
        None => app_update_mirror,
    };

    let state = AppState {
        db,
        environment: config.environment.clone(),
        require_https: config.environment.eq_ignore_ascii_case("production"),
        ibkr_gateway_url: config.ibkr_gateway_url.clone(),
        llm,
        llm_max_tokens: config.llm_max_tokens,
        jarvis_system,
        speech,
        speech_verify_threshold: config.speech_verify_threshold,
        registry,
        registry_input: Arc::new(registry_input),
        model_policy: Arc::new(model_policy),
        pricing_registry: Arc::new(pricing_registry),
        usage_snapshot_path: Some(Arc::new(PathBuf::from(
            "/var/lib/jarvis/usage-summary.json",
        ))),
        privileged_broker_socket: (!config.privileged_broker_socket.trim().is_empty())
            .then(|| Arc::<str>::from(config.privileged_broker_socket.trim())),
        codex_broker_socket: (!config.codex_broker_socket.trim().is_empty())
            .then(|| Arc::<str>::from(config.codex_broker_socket.trim())),
        budget_cents,
        spent_cents,
        budget_book,
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
            authenticated_per_min: config.authenticated_rate_per_min,
            llm_per_min: config.llm_rate_per_min,
        },
        trusted_proxy_hops: config.trusted_proxy_hops,
        trusted_proxy_ips: Arc::new(trusted_proxy_ips),
        bootstrap_enrollment,
        app_update_mirror,
    };

    jarvis_api::refresh_usage_snapshot(&state).await;
    let usage_snapshot_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(60),
        );
        loop {
            interval.tick().await;
            jarvis_api::refresh_usage_snapshot(&usage_snapshot_state).await;
        }
    });
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
