//! Jarvis API / BFF — Axum router, handlers, and the auth extractor.
//!
//! Public endpoints: liveness/readiness, and device-bound auth
//! (`/v1/auth/challenge`, `/v1/auth/login`). Protected endpoints require a
//! `Bearer` session token (see [`Authed`]).

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use jarvis_ibkr as ibkr;
use jarvis_identity as identity;
use jarvis_llm as llm;
use jarvis_portfolio as portfolio;
use jarvis_registry as registry;
use jarvis_speech as speech;
use rust_decimal::Decimal;
use jarvis_usage as usage;
// std (not tokio) RwLock: the router's `Availability` reads it synchronously,
// and the registry is small with brief, await-free critical sections.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

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
        .route("/v1/voice/status", get(voice_status))
        .route("/v1/voice/enroll", post(voice_enroll))
        .route("/v1/voice/verify", post(voice_verify))
        .route("/v1/system/registry", get(system_registry))
        .route("/v1/system/registry/refresh", post(system_registry_refresh))
        .route("/v1/system/usage", get(system_usage))
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
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "degraded", "error": e.to_string() })),
        ),
    }
}

fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
}

fn internal(_e: identity::IdentityError) -> (StatusCode, Json<Value>) {
    // Errors are deliberately opaque to clients; details go to logs/traces.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

fn bad_request(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

fn portfolio_err(_e: portfolio::PortfolioError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
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

#[derive(Deserialize)]
struct EnrollReq {
    name: String,
    platform: String,
    /// Hex-encoded Ed25519 public key.
    public_key: String,
}

/// Dev-only device enrollment: create the single user if needed and register
/// the calling device with its public key. Disabled in production.
async fn auth_enroll(
    State(state): State<AppState>,
    Json(req): Json<EnrollReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if state.environment == "production" {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "enrollment is disabled in production" })),
        ));
    }
    let platform =
        identity::Platform::parse(&req.platform).map_err(|_| bad_request("unknown platform"))?;
    let public_key =
        hex::decode(&req.public_key).map_err(|_| bad_request("invalid public_key encoding"))?;

    let user = identity::first_user_or_create(&state.db, "Jarvis user")
        .await
        .map_err(internal)?;
    let (device, _key) = identity::register_device(
        &state.db,
        user.id,
        &req.name,
        platform,
        "ed25519",
        &public_key,
    )
    .await
    .map_err(internal)?;

    Ok(Json(json!({
        "user_id": user.id,
        "device_id": device.id,
    })))
}

#[derive(Deserialize)]
struct ChallengeReq {
    device_id: Uuid,
}

/// Issue a login challenge (nonce) for a device.
async fn auth_challenge(
    State(state): State<AppState>,
    Json(req): Json<ChallengeReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let challenge = identity::create_challenge(&state.db, req.device_id)
        .await
        .map_err(internal)?;
    Ok(Json(json!({
        "challenge_id": challenge.id,
        "nonce": hex::encode(challenge.nonce),
    })))
}

#[derive(Deserialize)]
struct LoginReq {
    device_id: Uuid,
    challenge_id: Uuid,
    /// Hex-encoded Ed25519 signature over the challenge nonce.
    signature: String,
}

/// Verify a signed challenge and issue a session token.
async fn auth_login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let signature = hex::decode(&req.signature).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid signature encoding" })),
        )
    })?;
    let result = identity::login(&state.db, req.device_id, req.challenge_id, &signature)
        .await
        .map_err(|_| unauthorized())?;
    Ok(Json(json!({
        "token": result.token,
        "expires_at": result.session.expires_at.unix_timestamp(),
    })))
}

/// List the authenticated user's active devices (protected).
async fn list_devices(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let devices = identity::list_active_devices(&state.db, authed.user.id)
        .await
        .map_err(internal)?;
    let items: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "id": d.id,
                "name": d.name,
                "platform": d.platform,
                "status": d.status,
                "created_at": d.created_at.unix_timestamp(),
            })
        })
        .collect();
    Ok(Json(json!({ "devices": items })))
}

/// Log out: revoke the current session server-side.
async fn auth_logout(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    identity::revoke_session(&state.db, authed.session_id)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "status": "logged out" })))
}

/// Ask a trusted device to approve unlocking this (requesting) device.
/// Returns the request id and the nonce an approver must sign.
async fn unlock_request(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (id, nonce) =
        identity::create_unlock_request(&state.db, authed.user.id, authed.device.id)
            .await
            .map_err(internal)?;
    Ok(Json(json!({
        "request_id": id,
        "nonce": hex::encode(nonce),
    })))
}

/// Longest a `?wait=` long-poll will hold a request open (seconds).
const UNLOCK_WAIT_CAP_SECS: u64 = 25;

/// Parse and clamp the `?wait=` long-poll seconds from query params.
fn wait_secs(params: &HashMap<String, String>) -> u64 {
    params
        .get("wait")
        .and_then(|w| w.parse::<u64>().ok())
        .unwrap_or(0)
        .min(UNLOCK_WAIT_CAP_SECS)
}

/// Poll the status of an unlock request: `pending` | `approved` | `denied` | `expired`.
///
/// With `?wait=<secs>` the request long-polls: it returns as soon as the status
/// leaves `pending` (near-instant approval), or after the timeout still pending.
async fn unlock_status(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut ticks = wait_secs(&params) * 2; // 500ms per tick
    loop {
        match identity::unlock_request_status(&state.db, id, authed.user.id)
            .await
            .map_err(internal)?
        {
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "unlock request not found" })),
                ))
            }
            Some(status) if status != "pending" => {
                return Ok(Json(json!({ "status": status })))
            }
            Some(status) => {
                if ticks == 0 {
                    return Ok(Json(json!({ "status": status })));
                }
                ticks -= 1;
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// Unlock requests this device can approve (same user, not itself). With
/// `?wait=<secs>` it long-polls: returns as soon as a request appears.
async fn unlock_pending(
    authed: Authed,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut ticks = wait_secs(&params) * 2;
    let requests = loop {
        let requests =
            identity::pending_unlock_requests(&state.db, authed.user.id, authed.device.id)
                .await
                .map_err(internal)?;
        if !requests.is_empty() || ticks == 0 {
            break requests;
        }
        ticks -= 1;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    let items: Vec<Value> = requests
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "device_name": r.requesting_device_name,
                "platform": r.requesting_device_platform,
                "nonce": hex::encode(&r.nonce),
                "created_at": r.created_at.unix_timestamp(),
            })
        })
        .collect();
    Ok(Json(json!({ "requests": items })))
}

#[derive(Deserialize)]
struct ApproveReq {
    /// Hex-encoded Ed25519 signature over the request nonce.
    signature: String,
}

/// Approve an unlock request by signing its nonce with this device's key.
async fn unlock_approve(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let signature =
        hex::decode(&req.signature).map_err(|_| bad_request("invalid signature encoding"))?;
    identity::approve_unlock_request(&state.db, id, authed.user.id, authed.device.id, &signature)
        .await
        .map_err(|_| unauthorized())?;
    Ok(Json(json!({ "status": "approved" })))
}

/// Deny (cancel) a pending unlock request from this device.
async fn unlock_deny(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    identity::deny_unlock_request(&state.db, id, authed.user.id, authed.device.id)
        .await
        .map_err(|_| unauthorized())?;
    Ok(Json(json!({ "status": "denied" })))
}

/// Revoke one of the authenticated user's devices.
async fn delete_device(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match identity::get_device(&state.db, id)
        .await
        .map_err(internal)?
    {
        Some(device) if device.user_id == authed.user.id => {
            identity::revoke_device(&state.db, id)
                .await
                .map_err(internal)?;
            Ok(Json(json!({ "status": "revoked" })))
        }
        _ => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "device not found" })),
        )),
    }
}

/// List the authenticated user's holdings with cost basis and allocation.
async fn get_holdings(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let holdings = portfolio::list_holdings(&state.db, authed.user.id)
        .await
        .map_err(portfolio_err)?;
    let total: Decimal = holdings.iter().map(|h| h.cost_basis()).sum();
    let hundred = Decimal::from(100);
    let items: Vec<Value> = holdings
        .iter()
        .map(|h| {
            let cost = h.cost_basis();
            let weight = if total.is_zero() {
                Decimal::ZERO
            } else {
                (cost / total) * hundred
            };
            json!({
                "id": h.id,
                "symbol": h.symbol,
                "quantity": h.quantity.normalize().to_string(),
                "avg_cost": h.avg_cost.normalize().to_string(),
                "currency": h.currency,
                "cost_basis": cost.normalize().to_string(),
                "weight_pct": weight.round_dp(1).normalize().to_string(),
            })
        })
        .collect();
    Ok(Json(json!({
        "holdings": items,
        "total_cost": total.normalize().to_string(),
    })))
}

#[derive(Deserialize)]
struct AddHoldingReq {
    symbol: String,
    quantity: Decimal,
    avg_cost: Decimal,
    currency: Option<String>,
}

/// Add a holding for the authenticated user.
async fn add_holding(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AddHoldingReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let symbol = req.symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Err(bad_request("symbol is required"));
    }
    if req.quantity <= Decimal::ZERO {
        return Err(bad_request("quantity must be positive"));
    }
    if req.avg_cost < Decimal::ZERO {
        return Err(bad_request("avg_cost must be non-negative"));
    }
    let currency = req.currency.as_deref().unwrap_or("EUR");
    let holding = portfolio::add_holding(
        &state.db,
        authed.user.id,
        &symbol,
        req.quantity,
        req.avg_cost,
        currency,
    )
    .await
    .map_err(portfolio_err)?;
    Ok(Json(json!({ "id": holding.id, "symbol": holding.symbol })))
}

/// Delete one of the authenticated user's holdings.
async fn remove_holding(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if portfolio::delete_holding(&state.db, authed.user.id, id)
        .await
        .map_err(portfolio_err)?
    {
        Ok(Json(json!({ "status": "deleted" })))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "holding not found" })),
        ))
    }
}

fn ibkr_down() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "IBKR gateway not connected",
            "hint": "start de Client Portal Gateway en log in (paper of live)",
        })),
    )
}

/// IBKR gateway reachability + auth status (read-only).
async fn ibkr_status(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let client = match ibkr::IbkrClient::new(&state.ibkr_gateway_url) {
        Ok(c) => c,
        Err(_) => return Json(json!({ "reachable": false, "authenticated": false })),
    };
    match client.auth_status().await {
        Ok(s) => Json(json!({
            "reachable": true,
            "authenticated": s.authenticated,
            "connected": s.connected,
        })),
        Err(_) => Json(json!({
            "reachable": false,
            "authenticated": false,
            "hint": "start de IBKR Client Portal Gateway en log in",
        })),
    }
}

/// IBKR positions for an account (read-only). `?account=` selects the account;
/// otherwise the first account in the session is used.
async fn ibkr_positions(
    _authed: Authed,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = ibkr::IbkrClient::new(&state.ibkr_gateway_url).map_err(|_| ibkr_down())?;
    let status = client.auth_status().await.map_err(|_| ibkr_down())?;
    if !status.authenticated {
        return Err(ibkr_down());
    }
    let account = match params.get("account") {
        Some(a) => a.clone(),
        None => client
            .accounts()
            .await
            .map_err(|_| ibkr_down())?
            .into_iter()
            .next()
            .map(|a| a.account_id)
            .ok_or_else(ibkr_down)?,
    };
    let positions = client.positions(&account).await.map_err(|_| ibkr_down())?;
    let positions = serde_json::to_value(&positions).unwrap_or_else(|_| json!([]));
    Ok(Json(json!({ "account": account, "positions": positions })))
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

#[derive(Deserialize)]
struct ChatTurn {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatReq {
    messages: Vec<ChatTurn>,
    /// Optional tier hint: `default` | `hard` | `cheap`.
    #[serde(default)]
    tier: Option<String>,
    /// Optional system-prompt override (defaults to the Jarvis persona).
    #[serde(default)]
    system: Option<String>,
}

/// Chat with the brain (protected). The client sends the conversation so far;
/// the persona is prepended server-side. Never exposes the API key.
async fn assistant_chat(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let messages: Vec<llm::ChatMessage> = req
        .messages
        .iter()
        .filter_map(|t| {
            let content = t.content.trim();
            if content.is_empty() {
                return None;
            }
            Some(match t.role.as_str() {
                "assistant" | "jarvis" => llm::ChatMessage::assistant(content),
                _ => llm::ChatMessage::user(content),
            })
        })
        .collect();
    if messages.is_empty() {
        return Err(bad_request("messages is required"));
    }

    let chat = llm::ChatRequest {
        system: Some(
            req.system
                .unwrap_or_else(|| state.jarvis_system.to_string()),
        ),
        tier: req.tier.as_deref().map(llm::Tier::parse).unwrap_or_default(),
        messages,
        max_tokens: state.llm_max_tokens,
    };

    match state.llm.chat(&chat).await {
        Ok(reply) => {
            record_usage(&state, &reply).await;
            Ok(Json(json!({
                "reply": reply.text,
                "model": reply.model,
                "stop_reason": reply.stop_reason,
            })))
        }
        Err(llm::LlmError::Refused) => Ok(Json(json!({
            "reply": "Sorry, daar kan ik niet op antwoorden.",
            "model": Value::Null,
            "stop_reason": "refusal",
        }))),
        Err(e) => {
            // Details stay in logs; the client gets an opaque, actionable hint.
            tracing::warn!(error = %e, "assistant chat failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer JARVIS_LLM_API_KEY of start Ollama lokaal",
                })),
            ))
        }
    }
}

// ---- Voice: server-side speaker verification + STT --------------------------

fn db_err(_e: sqlx::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

fn speech_err(e: speech::SpeechError) -> (StatusCode, Json<Value>) {
    match e {
        speech::SpeechError::TooShort => bad_request("audio was empty or too short"),
        speech::SpeechError::NotConfigured(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "speech engine not configured" })),
        ),
        speech::SpeechError::Failed(_) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "speech engine failed" })),
        ),
    }
}

fn encode_embedding(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Deserialize)]
struct AudioReq {
    sample_rate: u32,
    /// 16-bit mono PCM samples.
    pcm: Vec<i16>,
}

fn to_audio(req: AudioReq) -> Result<speech::Audio, (StatusCode, Json<Value>)> {
    if req.pcm.is_empty() {
        return Err(bad_request("audio is required"));
    }
    Ok(speech::Audio::new(req.pcm, req.sample_rate))
}

/// Whether the authenticated user has enrolled a voice profile.
async fn voice_status(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let exists: Option<i32> =
        sqlx::query_scalar("select 1 from voice_profiles where user_id = $1")
            .bind(authed.user.id)
            .fetch_optional(&state.db)
            .await
            .map_err(db_err)?;
    Ok(Json(json!({
        "enrolled": exists.is_some(),
        "engine": state.speech.label(),
    })))
}

/// Enroll (or re-enroll) the user's voice: embed the audio and store it centrally.
async fn voice_enroll(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AudioReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let audio = to_audio(req)?;
    let embedding = state.speech.embed(&audio).await.map_err(speech_err)?;
    let bytes = encode_embedding(&embedding);
    sqlx::query(
        "insert into voice_profiles (user_id, embedding, dims, engine, updated_at) \
         values ($1, $2, $3, $4, now()) \
         on conflict (user_id) do update set \
           embedding = $2, dims = $3, engine = $4, updated_at = now()",
    )
    .bind(authed.user.id)
    .bind(&bytes)
    .bind(embedding.len() as i32)
    .bind(state.speech.label())
    .execute(&state.db)
    .await
    .map_err(db_err)?;
    Ok(Json(json!({ "status": "enrolled", "dims": embedding.len() })))
}

/// Verify a voice against the enrolled profile and transcribe it.
async fn voice_verify(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AudioReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let audio = to_audio(req)?;
    let embedding = state.speech.embed(&audio).await.map_err(speech_err)?;
    let transcript = state.speech.transcribe(&audio).await.unwrap_or_default();

    let stored: Option<Vec<u8>> =
        sqlx::query_scalar("select embedding from voice_profiles where user_id = $1")
            .bind(authed.user.id)
            .fetch_optional(&state.db)
            .await
            .map_err(db_err)?;

    match stored {
        None => Ok(Json(json!({
            "enrolled": false,
            "is_you": false,
            "score": 0.0,
            "transcript": transcript,
        }))),
        Some(bytes) => {
            let profile = decode_embedding(&bytes);
            let score = speech::cosine(&profile, &embedding);
            Ok(Json(json!({
                "enrolled": true,
                "is_you": score >= state.speech_verify_threshold,
                "score": score,
                "transcript": transcript,
            })))
        }
    }
}

/// Record a reply's cost and refresh the monthly spend counter (ADR-027). Free
/// backends (plan/Ollama) cost nothing and are skipped. Billing must never break
/// a chat, so DB errors are logged, not surfaced.
async fn record_usage(state: &AppState, reply: &llm::ChatReply) {
    let (Some(u), Some(backend)) = (&reply.usage, reply.backend.as_deref()) else {
        return;
    };
    let cost = usage::cost_eur(
        backend,
        &reply.model,
        u.input_tokens,
        u.output_tokens,
        u.cache_read_tokens,
        state.eur_per_usd,
    );
    tracing::info!(
        %backend, model = %reply.model, input = u.input_tokens, output = u.output_tokens,
        cache_read = u.cache_read_tokens, cost_eur = cost, "assistant chat usage",
    );
    if !usage::is_metered(backend) {
        return; // plan/local: nothing to bill or count
    }
    let entry = usage::UsageEntry {
        backend: backend.to_string(),
        model: reply.model.clone(),
        input_tokens: u.input_tokens as i32,
        output_tokens: u.output_tokens as i32,
        cache_read_tokens: u.cache_read_tokens as i32,
        cache_write_tokens: u.cache_write_tokens as i32,
        cost_eur: cost,
    };
    if let Err(e) = usage::record(&state.db, &entry).await {
        tracing::warn!(error = %e, "failed to record llm usage");
    }
    // Re-read the month total so the gate stays correct across a month rollover.
    match usage::month_total_eur(&state.db).await {
        Ok(total) => state
            .spent_cents
            .store((total * 100.0).round() as u64, Ordering::Relaxed),
        Err(e) => tracing::warn!(error = %e, "failed to refresh monthly spend"),
    }
}

/// This month's LLM spend vs. the budget, with a per-backend breakdown (ADR-027).
async fn system_usage(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let spent_eur = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget_eur = state.budget_cents as f64 / 100.0;
    let breakdown = usage::month_breakdown(&state.db).await.unwrap_or_default();
    let by_backend: Vec<Value> = breakdown
        .into_iter()
        .map(|(backend, eur)| json!({ "backend": backend, "spent_eur": eur }))
        .collect();
    Json(json!({
        "budget_eur": budget_eur,
        "spent_eur": spent_eur,
        "remaining_eur": (budget_eur - spent_eur).max(0.0),
        "over_budget": spent_eur >= budget_eur,
        "by_backend": by_backend,
    }))
}

/// Jarvis' resource/agent registry — available brains + cost + the host it runs
/// on (ADR-027 stage 3). Cached from startup; POST `/refresh` re-probes.
async fn system_registry(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let value = state
        .registry
        .read()
        .map(|reg| serde_json::to_value(&*reg).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|_| json!({}));
    Json(value)
}

async fn system_registry_refresh(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let fresh = registry::collect(&state.registry_input).await;
    if let Ok(mut reg) = state.registry.write() {
        *reg = fresh.clone();
    }
    Json(serde_json::to_value(&fresh).unwrap_or_else(|_| json!({})))
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
}
