//! Jarvis API / BFF — Axum router, handlers, and the auth extractor.
//!
//! Public endpoints: liveness/readiness, and device-bound auth
//! (`/v1/auth/challenge`, `/v1/auth/login`). Protected endpoints require a
//! `Bearer` session token (see [`Authed`]).

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde_json::{json, Value};
use tower_http::trace::TraceLayer;

use jarvis_llm as llm;
use jarvis_registry as registry;
use jarvis_usage as usage;
// std (not tokio) RwLock: the router's `Availability` reads it synchronously,
// and the registry is small with brief, await-free critical sections.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

mod audit;
mod error;
mod extract;
mod mcp;
mod metering;
mod rate_limit;
mod routes;
mod state;
mod validation;

pub use extract::Authed;
pub use state::{AppState, AppUpdateMirror};
// Compatibility re-export: persona ownership is in `jarvis-core`, while the
// transport crate keeps this public helper available to existing callers.
pub use jarvis_core::{load_persona, JARVIS_SYSTEM_FALLBACK};

use audit::{agent_audit_log, security_audit_log};
use mcp::mcp_endpoint;
use rate_limit::rate_limit_mw;
pub use rate_limit::{AuthLimits, RateLimiter};
use routes::agent::{agent_action, agent_pending, agent_pending_approve, agent_pending_deny};
use routes::app_updates::{
    android_update_check, android_update_download, app_update_artifact, app_update_capability,
    app_update_check,
};
use routes::auth::{
    auth_bootstrap, auth_challenge, auth_enroll, auth_login, auth_logout, auth_me, delete_device,
    list_devices, pairing_approve, pairing_create, pairing_deny, pairing_pending, pairing_status,
    unlock_approve, unlock_deny, unlock_pending, unlock_request, unlock_status,
};
use routes::broker::{ibkr_positions, ibkr_status};
use routes::chat::{
    assistant_chat, assistant_orchestrate, conversation_brain_set, delete_conversation,
    get_conversation, list_conversations,
};
use routes::coding::{
    create as coding_create, lifecycle as coding_lifecycle, list as coding_list,
    start_or_resume as coding_start_or_resume,
};
use routes::portfolio::{add_holding, get_holdings, remove_holding};
use routes::system::{
    system_brain, system_brain_set, system_budget_preflight, system_model_policy,
    system_privileged_config, system_registry, system_registry_refresh, system_self_improve,
    system_usage,
};
use routes::voice::{voice_enroll, voice_status, voice_verify};

/// Build the application router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/v1/auth/enroll", post(auth_enroll))
        .route("/v1/auth/bootstrap", post(auth_bootstrap))
        .route(
            "/v1/auth/pairing/requests",
            post(pairing_create).get(pairing_pending),
        )
        .route(
            "/v1/auth/pairing/requests/{id}/approve",
            post(pairing_approve),
        )
        .route("/v1/auth/pairing/requests/{id}/deny", post(pairing_deny))
        .route("/v1/auth/pairing/requests/{id}/status", get(pairing_status))
        .route("/v1/auth/challenge", post(auth_challenge))
        .route("/v1/auth/login", post(auth_login))
        .route("/v1/auth/me", get(auth_me))
        .route("/v1/auth/logout", post(auth_logout))
        .route("/v1/auth/unlock/request", post(unlock_request))
        .route("/v1/auth/unlock/pending", get(unlock_pending))
        .route("/v1/auth/unlock/{id}", get(unlock_status))
        .route("/v1/auth/unlock/{id}/approve", post(unlock_approve))
        .route("/v1/auth/unlock/{id}/deny", post(unlock_deny))
        .route("/v1/devices", get(list_devices))
        .route("/v1/devices/{id}", delete(delete_device))
        .route("/v1/app-updates/capability", get(app_update_capability))
        .route(
            "/v1/app-updates/android/{current_version_code}",
            get(android_update_check),
        )
        .route(
            "/v1/app-updates/android/download",
            get(android_update_download),
        )
        .route(
            "/v1/app-updates/{channel}/{target}/{arch}/{current_version}",
            get(app_update_check),
        )
        .route(
            "/v1/app-updates/artifacts/{version}/{target}/{arch}/{filename}",
            get(app_update_artifact),
        )
        .route("/v1/holdings", get(get_holdings).post(add_holding))
        .route("/v1/holdings/{id}", delete(remove_holding))
        .route("/v1/broker/ibkr/status", get(ibkr_status))
        .route("/v1/broker/ibkr/positions", get(ibkr_positions))
        .route("/v1/assistant/chat", post(assistant_chat))
        .route("/v1/assistant/orchestrate", post(assistant_orchestrate))
        .route("/v1/coding/sessions", get(coding_list).post(coding_create))
        .route("/v1/coding/sessions/{id}", post(coding_lifecycle))
        .route(
            "/v1/coding/sessions/{id}/{mode}",
            post(coding_start_or_resume),
        )
        .route("/v1/conversations", get(list_conversations))
        .route(
            "/v1/conversations/{id}",
            get(get_conversation).delete(delete_conversation),
        )
        .route("/v1/conversations/{id}/brain", put(conversation_brain_set))
        .route("/v1/voice/status", get(voice_status))
        .route("/v1/voice/enroll", post(voice_enroll))
        .route("/v1/voice/verify", post(voice_verify))
        .route("/v1/system/registry", get(system_registry))
        .route("/v1/system/registry/refresh", post(system_registry_refresh))
        .route("/v1/system/usage", get(system_usage))
        .route("/v1/system/models", get(system_model_policy))
        .route("/v1/system/brain", get(system_brain).put(system_brain_set))
        .route("/v1/system/budget/preflight", post(system_budget_preflight))
        .route(
            "/v1/system/config/privileged",
            post(system_privileged_config),
        )
        .route("/v1/system/self-improve", post(system_self_improve))
        .route("/v1/agent/action", post(agent_action))
        .route("/v1/agent/pending", get(agent_pending))
        .route(
            "/v1/agent/pending/{id}/approve",
            post(agent_pending_approve),
        )
        .route("/v1/agent/pending/{id}/deny", post(agent_pending_deny))
        .route("/v1/agent/audit", get(agent_audit_log))
        .route("/v1/system/audit", get(security_audit_log))
        .route("/mcp", post(mcp_endpoint))
        // Every JSON endpoint has an explicit, bounded total request size.
        // Endpoint-specific semantic limits remain in the handlers.
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Per-IP rate limiting on auth-sensitive endpoints (enroll/challenge/login).
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit_mw))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
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
    match state.db.query("RETURN 1").await {
        Ok(_) => (
            StatusCode::OK,
            // These probes are intentionally safe to publish through Caddy.
            // Dependency names, environment and failure detail stay in the
            // authenticated diagnostics and server logs.
            Json(json!({ "status": "ready" })),
        ),
        Err(e) => {
            // Log the detail; never leak internal DB errors in the response body.
            tracing::warn!(error = %e, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "degraded" })),
            )
        }
    }
}

/// Live brain availability for the router (`jarvis-llm`) — the bridge that makes
/// it route on what's actually up *and* affordable (ADR-027). A backend is
/// available iff the registry marks it available AND, for metered API backends,
/// this month's spend is still under the budget. A poisoned lock degrades to
/// "try it" so a bug here never bricks the brain.
pub struct BrainAvailability {
    pub registry: Arc<RwLock<registry::Registry>>,
    pub spent_cents: Arc<AtomicU64>,
    pub budget_cents: u64,
}

/// Map the registry's model catalog (available models only) into the router's
/// catalog so it can pick the cheapest sufficient model per task (ADR-028 fase 2).
pub fn router_catalog(reg: &Arc<RwLock<registry::Registry>>) -> Vec<llm::CatalogModel> {
    let Ok(reg) = reg.read() else {
        return Vec::new();
    };
    reg.models
        .iter()
        .filter(|m| m.available)
        .map(|m| llm::CatalogModel {
            backend: m.backend.clone(),
            id: m.id.clone(),
            class: match m.class {
                registry::ModelClass::Light => llm::ModelClass::Light,
                registry::ModelClass::Mid => llm::ModelClass::Mid,
                registry::ModelClass::Heavy => llm::ModelClass::Heavy,
                registry::ModelClass::Reasoning => llm::ModelClass::Reasoning,
            },
        })
        .collect()
}

impl llm::Availability for BrainAvailability {
    fn is_available(&self, backend_id: &str) -> bool {
        // Metered backends are cut off once the monthly budget is reached, so
        // the router falls back to the free plan/Ollama.
        if usage::is_metered(backend_id)
            && self.spent_cents.load(Ordering::Relaxed) >= self.budget_cents
        {
            return false;
        }
        self.registry
            .read()
            .map(|reg| reg.brains.iter().any(|b| b.id == backend_id && b.available))
            .unwrap_or(true)
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
