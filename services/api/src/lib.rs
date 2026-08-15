//! Jarvis API / BFF — Axum router, handlers, and the auth extractor.
//!
//! Public endpoints: liveness/readiness, and device-bound auth
//! (`/v1/auth/challenge`, `/v1/auth/login`). Protected endpoints require a
//! `Bearer` session token (see [`Authed`]).

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    middleware,
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use jarvis_identity as identity;
use jarvis_agent as agent;
use jarvis_llm as llm;
use jarvis_registry as registry;
use jarvis_speech as speech;
use jarvis_usage as usage;
// std (not tokio) RwLock: the router's `Availability` reads it synchronously,
// and the registry is small with brief, await-free critical sections.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

mod audit;
mod error;
mod mcp;
mod metering;
mod rate_limit;
mod routes;
mod validation;

use audit::{agent_audit_log, security_audit_log};
use error::unauthorized;
use mcp::mcp_endpoint;
use rate_limit::rate_limit_mw;
use routes::agent::{agent_action, agent_pending, agent_pending_approve, agent_pending_deny};
use routes::auth::{
    auth_challenge, auth_enroll, auth_login, auth_logout, delete_device, list_devices,
    unlock_approve, unlock_deny, unlock_pending, unlock_request, unlock_status,
};
use routes::broker::{ibkr_positions, ibkr_status};
use routes::chat::{
    assistant_chat, assistant_orchestrate, delete_conversation, get_conversation,
    list_conversations,
};
use routes::portfolio::{add_holding, get_holdings, remove_holding};
use routes::system::{
    system_registry, system_registry_refresh, system_self_improve, system_usage,
};
use routes::voice::{voice_enroll, voice_status, voice_verify};
pub use rate_limit::{AuthLimits, RateLimiter};

/// Shared, cheaply-cloneable application state.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub environment: String,
    pub ibkr_gateway_url: String,
    /// The brain (DEC-001) — provider-abstracted, swappable at runtime.
    pub llm: Arc<dyn llm::LlmProvider>,
    /// Max output tokens per assistant reply.
    pub llm_max_tokens: u32,
    /// Jarvis' identity/persona (from `core/Jarvis.md`), prepended as the system
    /// prompt on every chat. The single source of truth for "what Jarvis is".
    pub jarvis_system: Arc<str>,
    /// Server-side speech engine (STT + speaker verification).
    pub speech: Arc<dyn speech::SpeechEngine>,
    /// Cosine threshold to accept a voice as the enrolled speaker.
    pub speech_verify_threshold: f32,
    /// Resource/agent registry — Jarvis' "instant memory" (ADR-027 stage 3).
    pub registry: Arc<RwLock<registry::Registry>>,
    /// Inputs to re-collect the registry on refresh.
    pub registry_input: Arc<registry::CollectInput>,
    /// Hard monthly spend cap in EUR-cents across metered API backends (ADR-027).
    pub budget_cents: u64,
    /// Metered spend so far this month, in EUR-cents. Mirrors the DB (refreshed
    /// after each call) so the router's sync budget gate can read it cheaply.
    pub spent_cents: Arc<AtomicU64>,
    /// EUR per 1 USD, to price provider (USD) usage into the EUR budget.
    pub eur_per_usd: f64,
    /// Agentic execution kill switch (ADR-029) — Jarvis has no hands unless true.
    pub agent_enabled: bool,
    /// The sandbox Jarvis' read-only actions are confined to. `None` ⇒ no
    /// workspace configured (actions refused even when enabled).
    pub agent_sandbox: Option<Arc<agent::Sandbox>>,
    /// Per-IP rate limiter for auth-sensitive endpoints (enroll/challenge/login).
    pub rate_limiter: Arc<rate_limit::RateLimiter>,
    /// Tunable thresholds for the auth rate limiter (from `JARVIS_AUTH_*`).
    pub auth_limits: rate_limit::AuthLimits,
    /// Number of trusted proxy hops in front of the API (0 ⇒ never trust
    /// `X-Forwarded-For`; use the socket peer). See [`client_ip`].
    pub trusted_proxy_hops: u32,
}

/// Build the application router.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .route("/v1/auth/enroll", post(auth_enroll))
        .route("/v1/auth/challenge", post(auth_challenge))
        .route("/v1/auth/login", post(auth_login))
        .route("/v1/auth/logout", post(auth_logout))
        .route("/v1/auth/unlock/request", post(unlock_request))
        .route("/v1/auth/unlock/pending", get(unlock_pending))
        .route("/v1/auth/unlock/{id}", get(unlock_status))
        .route("/v1/auth/unlock/{id}/approve", post(unlock_approve))
        .route("/v1/auth/unlock/{id}/deny", post(unlock_deny))
        .route("/v1/devices", get(list_devices))
        .route("/v1/devices/{id}", delete(delete_device))
        .route("/v1/holdings", get(get_holdings).post(add_holding))
        .route("/v1/holdings/{id}", delete(remove_holding))
        .route("/v1/broker/ibkr/status", get(ibkr_status))
        .route("/v1/broker/ibkr/positions", get(ibkr_positions))
        .route("/v1/assistant/chat", post(assistant_chat))
        .route("/v1/assistant/orchestrate", post(assistant_orchestrate))
        .route("/v1/conversations", get(list_conversations))
        .route(
            "/v1/conversations/{id}",
            get(get_conversation).delete(delete_conversation),
        )
        .route("/v1/voice/status", get(voice_status))
        .route("/v1/voice/enroll", post(voice_enroll))
        .route("/v1/voice/verify", post(voice_verify))
        .route("/v1/system/registry", get(system_registry))
        .route("/v1/system/registry/refresh", post(system_registry_refresh))
        .route("/v1/system/usage", get(system_usage))
        .route("/v1/system/self-improve", post(system_self_improve))
        .route("/v1/agent/action", post(agent_action))
        .route("/v1/agent/pending", get(agent_pending))
        .route("/v1/agent/pending/{id}/approve", post(agent_pending_approve))
        .route("/v1/agent/pending/{id}/deny", post(agent_pending_deny))
        .route("/v1/agent/audit", get(agent_audit_log))
        .route("/v1/system/audit", get(security_audit_log))
        .route("/mcp", post(mcp_endpoint))
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
    match sqlx::query("SELECT 1").fetch_one(&state.db).await {
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ready", "environment": state.environment })),
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

/// Authenticated principal, extracted from a `Bearer` session token.
pub struct Authed {
    pub user: identity::User,
    pub device: identity::Device,
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let auth = identity::authenticate(&state.db, token)
            .await
            .map_err(|_| unauthorized())?;
        Ok(Authed {
            user: auth.user,
            device: auth.device,
            session_id: auth.session_id,
        })
    }
}

/// Fallback persona when `core/Jarvis.md` is absent (keeps dev/CI green without
/// the file). The real identity lives in `core/Jarvis.md`, loaded at startup
/// into [`AppState::jarvis_system`]. Kept plain: modern Claude models follow the
/// system prompt closely.
pub const JARVIS_SYSTEM_FALLBACK: &str = "Je bent Jarvis, de persoonlijke AI-assistent op het HUD-dashboard van de gebruiker. \
Antwoord in het Nederlands, kort en duidelijk, in een rustige en behulpzame toon. \
Je helpt met het systeem, de portfolio en trading-inzichten. \
Zeg het eerlijk wanneer je iets niet zeker weet in plaats van te gokken. \
Voer nooit trades of onomkeerbare acties uit — die vereisen altijd een expliciete bevestiging van de gebruiker.";

/// Load Jarvis' persona from `path` (typically `core/Jarvis.md`). A missing,
/// unreadable, or empty file falls back to [`JARVIS_SYSTEM_FALLBACK`] so the
/// brain always has an identity. Returns the text and whether the file loaded.
pub fn load_persona(path: &str) -> (Arc<str>, bool) {
    match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => (Arc::from(text.trim()), true),
        _ => (Arc::from(JARVIS_SYSTEM_FALLBACK), false),
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
            .map(|reg| {
                reg.brains
                    .iter()
                    .any(|b| b.id == backend_id && b.available)
            })
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use ed25519_dalek::{Signer, SigningKey};
    use rand::{rngs::OsRng, RngCore};
    use tower::ServiceExt;

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

    #[test]
    fn persona_falls_back_when_file_is_absent() {
        let (text, loaded) = load_persona("does/not/exist/Jarvis.md");
        assert!(!loaded);
        assert_eq!(&*text, JARVIS_SYSTEM_FALLBACK);
    }

    #[test]
    fn persona_loads_from_file_when_present() {
        let path = std::env::temp_dir().join("jarvis_persona_test.md");
        std::fs::write(&path, "  Je bent Jarvis, de kern.  \n").unwrap();
        let (text, loaded) = load_persona(path.to_str().unwrap());
        assert!(loaded);
        assert_eq!(&*text, "Je bent Jarvis, de kern.");
        let _ = std::fs::remove_file(&path);
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn auth_flow_over_http(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let public_key = hex::encode(signing.verifying_key().to_bytes());

        let app = build_router(AppState {
            db: pool.clone(),
            environment: "test".to_string(),
            ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
            llm: jarvis_llm::stub(),
            llm_max_tokens: 256,
            jarvis_system: std::sync::Arc::from(JARVIS_SYSTEM_FALLBACK),
            speech: jarvis_speech::stub(),
            speech_verify_threshold: 0.5,
            registry: std::sync::Arc::new(std::sync::RwLock::new(
                jarvis_registry::collect(&jarvis_registry::CollectInput::default()).await,
            )),
            registry_input: std::sync::Arc::new(jarvis_registry::CollectInput::default()),
            budget_cents: 5000,
            spent_cents: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eur_per_usd: 0.92,
            agent_enabled: false,
            agent_sandbox: None,
            rate_limiter: std::sync::Arc::new(crate::rate_limit::RateLimiter::new()),
            auth_limits: crate::rate_limit::AuthLimits::default(),
            trusted_proxy_hops: 0,
        });

        // 1. enroll this device (dev endpoint)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "name": "iPhone",
                            "platform": "ios",
                            "public_key": public_key,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let enroll = body_json(resp).await;
        let device_id = enroll["device_id"].as_str().unwrap().to_string();

        // 2. request a challenge
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/challenge")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "device_id": device_id })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ch = body_json(resp).await;
        let challenge_id = ch["challenge_id"].as_str().unwrap().to_string();
        let nonce = hex::decode(ch["nonce"].as_str().unwrap()).unwrap();

        // 2. sign the nonce and log in
        let signature = hex::encode(signing.sign(&nonce).to_bytes());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "device_id": device_id,
                            "challenge_id": challenge_id,
                            "signature": signature,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let login = body_json(resp).await;
        let token = login["token"].as_str().unwrap().to_string();

        // 3. protected route without a token -> 401
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/devices")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 4. protected route with the token -> 200 and one device
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/devices")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["devices"].as_array().unwrap().len(), 1);

        // 4b. add a holding, then list it (both protected)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/holdings")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "symbol": "aapl",
                            "quantity": "10",
                            "avg_cost": "150.25",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/holdings")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["holdings"].as_array().unwrap().len(), 1);
        assert_eq!(body["holdings"][0]["symbol"], "AAPL");
        assert_eq!(body["total_cost"], "1502.5");

        // 4c. assistant chat replies via the stub brain (protected)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/assistant/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "messages": [{ "role": "user", "content": "hoi Jarvis" }],
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["reply"], "echo: hoi Jarvis");

        // 4d. the chat endpoint is protected (no token -> 401)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/assistant/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "messages": [{ "role": "user", "content": "hoi" }],
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // 4d-bis. agent is off by default → even an authenticated read-only
        // action is refused (kill switch, ADR-029).
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agent/action")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "type": "list_dir", "path": "." })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "agent disabled");

        // 4e. voice: not enrolled → enroll → verify the same audio as "you"
        let pcm: Vec<i16> = (0..2000).map(|i| ((i * 7) % 5000) as i16).collect();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/voice/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["enrolled"], false);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/voice/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "sample_rate": 16000, "pcm": pcm })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_json(resp).await["status"], "enrolled");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/voice/verify")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "sample_rate": 16000, "pcm": pcm })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["enrolled"], true);
        assert_eq!(body["is_you"], true); // identical audio → perfect self-match

        // 5. logout revokes the session server-side
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/logout")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // 6. the revoked token no longer authenticates
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/devices")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Enroll a device and log it in, returning `(device_id, session token)`.
    async fn enroll_and_login(app: &axum::Router, signing: &SigningKey) -> (String, String) {
        let public_key = hex::encode(signing.verifying_key().to_bytes());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "name": "iPhone",
                            "platform": "ios",
                            "public_key": public_key,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let device_id = body_json(resp).await["device_id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/challenge")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "device_id": device_id })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let ch = body_json(resp).await;
        let challenge_id = ch["challenge_id"].as_str().unwrap().to_string();
        let nonce = hex::decode(ch["nonce"].as_str().unwrap()).unwrap();
        let signature = hex::encode(signing.sign(&nonce).to_bytes());

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "device_id": device_id,
                            "challenge_id": challenge_id,
                            "signature": signature,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let token = body_json(resp).await["token"].as_str().unwrap().to_string();
        (device_id, token)
    }

    async fn agent_enabled_state(pool: PgPool, sandbox: agent::Sandbox) -> AppState {
        AppState {
            db: pool,
            environment: "test".to_string(),
            ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
            llm: jarvis_llm::stub(),
            llm_max_tokens: 256,
            jarvis_system: std::sync::Arc::from(JARVIS_SYSTEM_FALLBACK),
            speech: jarvis_speech::stub(),
            speech_verify_threshold: 0.5,
            registry: std::sync::Arc::new(std::sync::RwLock::new(
                jarvis_registry::collect(&jarvis_registry::CollectInput::default()).await,
            )),
            registry_input: std::sync::Arc::new(jarvis_registry::CollectInput::default()),
            budget_cents: 5000,
            spent_cents: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eur_per_usd: 0.92,
            agent_enabled: true,
            agent_sandbox: Some(std::sync::Arc::new(sandbox)),
            rate_limiter: std::sync::Arc::new(crate::rate_limit::RateLimiter::new()),
            auth_limits: crate::rate_limit::AuthLimits::default(),
            trusted_proxy_hops: 0,
        }
    }

    async fn stub_state(pool: PgPool) -> AppState {
        AppState {
            db: pool,
            environment: "test".to_string(),
            ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
            llm: jarvis_llm::stub(),
            llm_max_tokens: 256,
            jarvis_system: std::sync::Arc::from(JARVIS_SYSTEM_FALLBACK),
            speech: jarvis_speech::stub(),
            speech_verify_threshold: 0.5,
            registry: std::sync::Arc::new(std::sync::RwLock::new(
                jarvis_registry::collect(&jarvis_registry::CollectInput::default()).await,
            )),
            registry_input: std::sync::Arc::new(jarvis_registry::CollectInput::default()),
            budget_cents: 5000,
            spent_cents: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            eur_per_usd: 0.92,
            agent_enabled: false,
            agent_sandbox: None,
            rate_limiter: std::sync::Arc::new(crate::rate_limit::RateLimiter::new()),
            auth_limits: crate::rate_limit::AuthLimits::default(),
            trusted_proxy_hops: 0,
        }
    }

    async fn enroll_status(app: &axum::Router, body: Value) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Malformed enrollment input is rejected (400) before any device is created:
    /// bounded name/platform and an exact-length hex public key.
    #[sqlx::test(migrations = "../../migrations")]
    async fn enroll_rejects_malformed_input(pool: PgPool) {
        let app = build_router(stub_state(pool).await);

        // A well-formed request is accepted (64-hex key = 32 bytes).
        let ok = enroll_status(
            &app,
            json!({ "name": "iPhone", "platform": "ios", "public_key": "aa".repeat(32) }),
        )
        .await;
        assert_eq!(ok, StatusCode::OK);

        // Oversized device name.
        let long = enroll_status(
            &app,
            json!({ "name": "x".repeat(200), "platform": "ios", "public_key": "aa".repeat(32) }),
        )
        .await;
        assert_eq!(long, StatusCode::BAD_REQUEST);

        // Wrong-length public key (not 64 hex chars).
        let short_key = enroll_status(
            &app,
            json!({ "name": "iPhone", "platform": "ios", "public_key": "abcd" }),
        )
        .await;
        assert_eq!(short_key, StatusCode::BAD_REQUEST);

        // Non-hex public key of the right length.
        let non_hex = enroll_status(
            &app,
            json!({ "name": "iPhone", "platform": "ios", "public_key": "z".repeat(64) }),
        )
        .await;
        assert_eq!(non_hex, StatusCode::BAD_REQUEST);
    }

    /// A failed readiness check reports "degraded" without leaking the internal
    /// database error into the response body (detail belongs in the logs).
    #[tokio::test]
    async fn readyz_does_not_leak_db_errors() {
        // A lazy pool pointed at a dead port: the SELECT 1 fails at call time.
        // A short acquire timeout keeps the test fast (no 30s default retry loop).
        let dead = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(200))
            .connect_lazy("postgres://127.0.0.1:1/none")
            .unwrap();
        let (status, Json(body)) = readyz(State(stub_state(dead).await)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "degraded");
        assert!(body.get("error").is_none(), "must not leak DB error detail");
    }

    /// Oversized free-text focus is rejected (400) before any LLM call.
    #[sqlx::test(migrations = "../../migrations")]
    async fn self_improve_rejects_oversized_focus(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/system/self-improve")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "focus": "x".repeat(600) })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Auth endpoints are rate limited per client IP: once the per-window budget
    /// is spent, further attempts get 429 (before reaching the handler). In-process
    /// test requests carry no peer address, so they all share one "local" bucket.
    #[sqlx::test(migrations = "../../migrations")]
    async fn enroll_is_rate_limited(pool: PgPool) {
        let app = build_router(stub_state(pool).await);
        let mut statuses = Vec::new();
        for _ in 0..12 {
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/auth/enroll")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::to_vec(&json!({
                                "name": "iPhone",
                                "platform": "ios",
                                "public_key": hex::encode(key),
                            }))
                            .unwrap(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            statuses.push(resp.status());
        }
        // First 10 within the window are allowed; the 11th trips the limit.
        assert_eq!(statuses[0], StatusCode::OK);
        assert_eq!(statuses[9], StatusCode::OK);
        assert_eq!(statuses[10], StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses[11], StatusCode::TOO_MANY_REQUESTS);
    }

    /// Repeated *failed* logins from one IP lock it out (429) after the failure
    /// threshold, even while below the flat per-minute limit.
    #[sqlx::test(migrations = "../../migrations")]
    async fn repeated_failed_logins_lock_out(pool: PgPool) {
        let app = build_router(stub_state(pool).await);
        let mut statuses = Vec::new();
        for _ in 0..7 {
            // Valid-format but wrong signature over random ids → the handler
            // returns 401, which the middleware counts as a failed login.
            let body = json!({
                "device_id": Uuid::now_v7(),
                "challenge_id": Uuid::now_v7(),
                "signature": "ab".repeat(64), // 128 hex chars
            });
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/auth/login")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            statuses.push(resp.status());
        }
        // First 5 are genuine 401s; after 5 failures the IP is locked (429).
        assert_eq!(statuses[0], StatusCode::UNAUTHORIZED);
        assert_eq!(statuses[4], StatusCode::UNAUTHORIZED);
        assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses[6], StatusCode::TOO_MANY_REQUESTS);
    }

    /// An oversized chat message is rejected (400) before any LLM call.
    #[sqlx::test(migrations = "../../migrations")]
    async fn chat_rejects_oversized_message(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/assistant/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "messages": [{ "role": "user", "content": "x".repeat(25_000) }]
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Enrolment and login are recorded in the security audit trail, readable by
    /// the owner at /v1/system/audit.
    #[sqlx::test(migrations = "../../migrations")]
    async fn security_audit_records_auth_events(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/system/audit")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let events: Vec<String> = body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_string())
            .collect();
        assert!(events.contains(&"auth.enroll".to_string()));
        assert!(events.contains(&"auth.login".to_string()));
    }

    /// A mutating action must not run until the owner signs its nonce on a
    /// trusted device; a signed approval executes it exactly once (ADR-029 4b).
    #[sqlx::test(migrations = "../../migrations")]
    async fn agent_mutating_needs_signed_approval(pool: PgPool) {
        // A unique sandbox root so parallel test runs never collide.
        let mut suffix = [0u8; 8];
        OsRng.fill_bytes(&mut suffix);
        let root = std::env::temp_dir().join(format!("jarvis_agent_{}", hex::encode(suffix)));
        std::fs::create_dir_all(&root).unwrap();
        let sandbox = agent::Sandbox::new(&root).unwrap();

        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);

        let app = build_router(agent_enabled_state(pool.clone(), sandbox).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        // 1. A write is not executed inline — it returns a pending action + nonce.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agent/action")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "type": "write_file",
                            "path": "note.txt",
                            "content": "hallo van jarvis",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["needs_approval"], true);
        let pending_id = body["pending_id"].as_str().unwrap().to_string();
        let nonce = hex::decode(body["nonce"].as_str().unwrap()).unwrap();
        // The file must NOT exist yet — nothing ran.
        assert!(!root.join("note.txt").exists());

        // 2. Signing the nonce approves exactly this action; it executes once.
        let signature = hex::encode(signing.sign(&nonce).to_bytes());
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/agent/pending/{pending_id}/approve"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "signature": signature })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(root.join("note.txt")).unwrap(),
            "hallo van jarvis"
        );

        // 3. Replay: the same signed approval cannot execute twice.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/agent/pending/{pending_id}/approve"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "signature": signature })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // 4. The Core is never writable — refused before any pending is created.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/agent/action")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "type": "write_file",
                            "path": "core/Jarvis.md",
                            "content": "hack",
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Chat persists under a conversation, a follow-up appends to it, and the
    /// thread survives to be listed + fetched + deleted (ADR-030). With the stub
    /// brain the classifier can't return JSON, so it falls back deterministically:
    /// no current conversation ⇒ new; an existing id ⇒ append.
    #[sqlx::test(migrations = "../../migrations")]
    async fn chat_is_persisted_and_grouped(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool.clone()).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        let chat = |body: Value, token: String, app: axum::Router| async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/assistant/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
        };

        // 1. First message with no conversation → a new conversation is created.
        let resp = chat(
            json!({ "messages": [{ "role": "user", "content": "hoe werkt rust ownership?" }] }),
            token.clone(),
            app.clone(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["new_topic"], true);
        assert_eq!(body["reply"], "echo: hoe werkt rust ownership?");
        let conv_id = body["conversation_id"].as_str().unwrap().to_string();
        assert!(body["conversation_title"].as_str().unwrap().contains("rust"));

        // 2. Follow-up carrying the conversation id → appended to the same thread.
        let resp = chat(
            json!({
                "conversation_id": conv_id,
                "messages": [
                    { "role": "user", "content": "hoe werkt rust ownership?" },
                    { "role": "assistant", "content": "echo: hoe werkt rust ownership?" },
                    { "role": "user", "content": "en borrowing?" }
                ]
            }),
            token.clone(),
            app.clone(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["new_topic"], false);
        assert_eq!(body["conversation_id"].as_str().unwrap(), conv_id);

        // 3. It lists as exactly one conversation.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/conversations")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["conversations"].as_array().unwrap().len(), 1);

        // 4. The thread holds all four turns in order.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/conversations/{conv_id}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "hoe werkt rust ownership?");
        assert_eq!(msgs[3]["role"], "assistant");

        // 5. Delete removes it; the list is empty again.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/conversations/{conv_id}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/conversations")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp).await;
        assert_eq!(body["conversations"].as_array().unwrap().len(), 0);
    }

    /// Self-development is advisory + owner-only: it needs auth and returns a
    /// proposal shape without ever acting (ADR-029 fase 4d).
    #[sqlx::test(migrations = "../../migrations")]
    async fn self_improve_is_advisory_and_protected(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool.clone()).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        // Unauthenticated → 401.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/system/self-improve")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Authenticated → 200 advisory shape (stub brain → no JSON → summary +
        // empty proposals + the owner-only note).
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/system/self-improve")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "focus": "goedkopere modellen" })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["summary"].is_string());
        assert!(body["proposals"].is_array());
        assert!(body["note"].as_str().unwrap().contains("goedkeuring"));
    }

    /// The MCP server is authenticated, read-only, and speaks the minimal
    /// JSON-RPC contract (ADR-031).
    #[sqlx::test(migrations = "../../migrations")]
    async fn mcp_exposes_read_only_tools(pool: PgPool) {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        let signing = SigningKey::from_bytes(&seed);
        let app = build_router(stub_state(pool.clone()).await);
        let (_device_id, token) = enroll_and_login(&app, &signing).await;

        let rpc = |body: Value, token: Option<String>, app: axum::Router| async move {
            let mut b = Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(t) = token {
                b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
            }
            app.oneshot(b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap())
                .await
                .unwrap()
        };

        // Unauthenticated → 401.
        let resp = rpc(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            None,
            app.clone(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // initialize → capabilities + serverInfo, echoing the protocol version.
        let resp = rpc(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
            Some(token.clone()),
            app.clone(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        assert_eq!(body["result"]["serverInfo"]["name"], "jarvis");

        // tools/list → the read-only catalog.
        let resp = rpc(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            Some(token.clone()),
            app.clone(),
        )
        .await;
        let body = body_json(resp).await;
        let names: Vec<&str> = body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"portfolio_summary"));
        assert!(names.contains(&"jarvis_status"));
        assert!(names.contains(&"recent_conversations"));

        // tools/call jarvis_status → text content, not an error.
        let resp = rpc(
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": { "name": "jarvis_status", "arguments": {} } }),
            Some(token.clone()),
            app.clone(),
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["result"]["isError"], false);
        assert!(body["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Budget"));

        // Unknown tool → JSON-RPC method error, never a crash.
        let resp = rpc(
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": { "name": "drop_everything", "arguments": {} } }),
            Some(token.clone()),
            app.clone(),
        )
        .await;
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32601);

        // A non-local browser Origin is refused (DNS-rebinding guard).
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header("origin", "https://evil.example")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
