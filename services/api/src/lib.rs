//! Jarvis API / BFF — Axum router, handlers, and the auth extractor.
//!
//! Public endpoints: liveness/readiness, and device-bound auth
//! (`/v1/auth/challenge`, `/v1/auth/login`). Protected endpoints require a
//! `Bearer` session token (see [`Authed`]).

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Path, Query, State},
    http::{header::AUTHORIZATION, request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
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
use jarvis_agent as agent;
use jarvis_llm as llm;
use jarvis_orchestrator as orchestrator;
use jarvis_portfolio as portfolio;
use jarvis_registry as registry;
use jarvis_selfdev as selfdev;
use jarvis_speech as speech;
use rust_decimal::Decimal;
use jarvis_usage as usage;
// std (not tokio) RwLock: the router's `Availability` reads it synchronously,
// and the registry is small with brief, await-free critical sections.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

mod validation;

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
        .route("/mcp", post(mcp_endpoint))
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
    // Bound the free-text fields and pin the key to an exact-length hex string
    // before it ever reaches the DB or crypto layer (fail closed on junk input).
    if !validation::bounded_text(&req.name, validation::MAX_DEVICE_NAME_LEN) {
        return Err(bad_request("invalid device name"));
    }
    if !validation::bounded_text(&req.platform, validation::MAX_PLATFORM_LEN) {
        return Err(bad_request("invalid platform"));
    }
    if !validation::is_hex_of_len(&req.public_key, validation::ED25519_PUBLIC_KEY_HEX_LEN) {
        return Err(bad_request("invalid public_key"));
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
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
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
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
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
    /// The conversation this turn belongs to (ADR-030). Absent ⇒ start a fresh
    /// one; present ⇒ append, unless the topic shifted (then Jarvis splits it off
    /// into a new conversation and returns the new id).
    #[serde(default)]
    conversation_id: Option<Uuid>,
}

/// Chat with the brain (protected). Persists the turn under a conversation and
/// auto-splits a new topic into its own conversation (ADR-030). The persona is
/// prepended server-side; the API key is never exposed.
async fn assistant_chat(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let history: Vec<llm::ChatMessage> = req
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
    if history.is_empty() {
        return Err(bad_request("messages is required"));
    }
    // The last user turn is the new message to store and (maybe) reclassify.
    let new_msg = req
        .messages
        .iter()
        .rev()
        .find(|t| !matches!(t.role.as_str(), "assistant" | "jarvis"))
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_request("a user message is required"))?;

    // Which conversation does this belong to? Append to the current one unless
    // the topic shifted; with no valid current one, start fresh.
    let existing = match req.conversation_id {
        Some(cid) => conversation_title(&state.db, cid, authed.user.id)
            .await
            .map(|t| (cid, t)),
        None => None,
    };
    let (conv_id, conv_title, new_topic) = match existing {
        Some((cid, title)) => {
            let (same, proposed) = classify_topic(&state, Some(&title), &new_msg).await;
            if same {
                (cid, title, false)
            } else {
                let id = create_conversation(&state.db, authed.user.id, &proposed)
                    .await
                    .map_err(db_err)?;
                (id, proposed, true)
            }
        }
        None => {
            let (_same, proposed) = classify_topic(&state, None, &new_msg).await;
            let id = create_conversation(&state.db, authed.user.id, &proposed)
                .await
                .map_err(db_err)?;
            (id, proposed, true)
        }
    };

    // Save what the owner said up front, so it survives even a brain outage.
    append_message(&state.db, conv_id, authed.user.id, "user", &new_msg, None).await;

    // A fresh topic starts with a clean slate; a continuation keeps its context.
    let messages = if new_topic {
        vec![llm::ChatMessage::user(&new_msg)]
    } else {
        history
    };
    let chat = llm::ChatRequest {
        system: Some(req.system.unwrap_or_else(|| state.jarvis_system.to_string())),
        tier: req.tier.as_deref().map(llm::Tier::parse).unwrap_or_default(),
        messages,
        max_tokens: state.llm_max_tokens,
        // The router picks the concrete model per backend (ADR-028 fase 2).
        model: None,
    };

    match state.llm.chat(&chat).await {
        Ok(reply) => {
            record_usage(&state, &reply).await;
            append_message(
                &state.db,
                conv_id,
                authed.user.id,
                "assistant",
                &reply.text,
                Some(reply.model.as_str()),
            )
            .await;
            Ok(Json(json!({
                "reply": reply.text,
                "model": reply.model,
                "stop_reason": reply.stop_reason,
                "conversation_id": conv_id,
                "conversation_title": conv_title,
                "new_topic": new_topic,
            })))
        }
        Err(llm::LlmError::Refused) => {
            let text = "Sorry, daar kan ik niet op antwoorden.";
            append_message(&state.db, conv_id, authed.user.id, "assistant", text, None).await;
            Ok(Json(json!({
                "reply": text,
                "model": Value::Null,
                "stop_reason": "refusal",
                "conversation_id": conv_id,
                "conversation_title": conv_title,
                "new_topic": new_topic,
            })))
        }
        Err(e) => {
            // Details stay in logs; the client gets an opaque, actionable hint.
            // The user's message is already saved under `conv_id`.
            tracing::warn!(error = %e, "assistant chat failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer JARVIS_LLM_API_KEY of start Ollama lokaal",
                    "conversation_id": conv_id,
                })),
            ))
        }
    }
}

/// Ask a cheap model whether `new_msg` continues the current topic, and get a
/// short title for the (new) topic. Best-effort: any failure keeps the current
/// conversation (or, with none, derives a title) so chat never breaks (ADR-030).
async fn classify_topic(
    state: &AppState,
    current_title: Option<&str>,
    new_msg: &str,
) -> (bool, String) {
    let snippet: String = new_msg.chars().take(600).collect();
    let system = "Je bepaalt of een nieuw bericht bij het lopende gespreksonderwerp \
         hoort of een nieuw onderwerp begint. Antwoord UITSLUITEND met JSON: \
         {\"same_topic\": true of false, \"title\": \"korte titel, max 5 woorden\"}. \
         Is er geen lopend onderwerp, dan is same_topic altijd false.";
    let user = format!(
        "Lopend onderwerp: \"{}\"\nNieuw bericht: \"{}\"",
        current_title.unwrap_or("(geen)"),
        snippet
    );
    let req = llm::ChatRequest {
        system: Some(system.to_string()),
        tier: llm::Tier::Cheap,
        messages: vec![llm::ChatMessage::user(&user)],
        max_tokens: 60,
        model: None,
    };
    match state.llm.chat(&req).await {
        Ok(reply) => {
            record_usage(state, &reply).await;
            parse_topic(&reply.text)
                .unwrap_or_else(|| (current_title.is_some(), derive_title(new_msg)))
        }
        // Brain down for the classifier: don't fragment — keep the current
        // conversation if there is one, else start one with a derived title.
        Err(_) => (current_title.is_some(), derive_title(new_msg)),
    }
}

/// Extract `{same_topic, title}` from a model reply (tolerant of prose around
/// the JSON). Returns None if no usable object is found.
fn parse_topic(text: &str) -> Option<(bool, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let same = v.get("same_topic").and_then(|b| b.as_bool()).unwrap_or(false);
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(clean_title)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Nieuw gesprek".to_string());
    Some((same, title))
}

/// A short, single-line title derived from the first user message.
fn derive_title(msg: &str) -> String {
    let t = clean_title(msg);
    if t.is_empty() {
        "Nieuw gesprek".to_string()
    } else {
        t
    }
}

/// Normalize a title: single line, trimmed, capped at ~48 chars.
fn clean_title(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = one_line.chars().take(48).collect();
    capped.trim().to_string()
}

/// A conversation's title, if it belongs to this user.
async fn conversation_title(pool: &PgPool, id: Uuid, user_id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT title FROM conversations WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Create a new conversation and return its id (ADR-030).
async fn create_conversation(
    pool: &PgPool,
    user_id: Uuid,
    title: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO conversations (id, user_id, title) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(user_id)
        .bind(title)
        .execute(pool)
        .await?;
    Ok(id)
}

/// Append a message and bump the conversation's `updated_at`. Best-effort:
/// persistence must never break the reply, so a failure is logged, not surfaced.
async fn append_message(
    pool: &PgPool,
    conv_id: Uuid,
    user_id: Uuid,
    role: &str,
    content: &str,
    model: Option<&str>,
) {
    let res = sqlx::query(
        "INSERT INTO chat_messages (id, conversation_id, user_id, role, content, model) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(conv_id)
    .bind(user_id)
    .bind(role)
    .bind(content)
    .bind(model)
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "failed to persist chat message");
        return;
    }
    let _ = sqlx::query("UPDATE conversations SET updated_at = now() WHERE id = $1")
        .bind(conv_id)
        .execute(pool)
        .await;
}

/// List the owner's conversations, newest-active first (ADR-030).
async fn list_conversations(authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, title, to_char(updated_at, 'YYYY-MM-DD HH24:MI') \
         FROM conversations WHERE user_id = $1 ORDER BY updated_at DESC LIMIT 100",
    )
    .bind(authed.user.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, title, updated)| json!({ "id": id, "title": title, "updated_at": updated }))
        .collect();
    Json(json!({ "conversations": items }))
}

/// A single conversation's messages, in order (ADR-030).
async fn get_conversation(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let title = conversation_title(&state.db, id, authed.user.id)
        .await
        .ok_or_else(|| {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "no such conversation" })))
        })?;
    let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT role, content, model, to_char(created_at, 'YYYY-MM-DD HH24:MI') \
         FROM chat_messages WHERE conversation_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await
    .map_err(db_err)?;
    let messages: Vec<Value> = rows
        .into_iter()
        .map(|(role, content, model, at)| {
            json!({ "role": role, "content": content, "model": model, "at": at })
        })
        .collect();
    Ok(Json(json!({ "id": id, "title": title, "messages": messages })))
}

/// Delete a conversation and its messages (ON DELETE CASCADE) — owner-only.
async fn delete_conversation(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let res = sqlx::query("DELETE FROM conversations WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(authed.user.id)
        .execute(&state.db)
        .await
        .map_err(db_err)?;
    if res.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "no such conversation" }))));
    }
    Ok(Json(json!({ "status": "deleted" })))
}

#[derive(Deserialize)]
struct OrchestrateReq {
    /// The task to plan and carry out.
    task: String,
}

/// Plan→execute a task (ADR-028 fase 3): a strong model plans, cheap models run
/// the steps, a synthesis composes + checks. Pure reasoning — no tools/actions.
/// Every underlying call is billed against the budget (ADR-027).
async fn assistant_orchestrate(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<OrchestrateReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let task = req.task.trim();
    if task.is_empty() {
        return Err(bad_request("task is required"));
    }
    match orchestrator::plan_and_execute(&state.llm, task, &state.jarvis_system).await {
        Ok(run) => {
            for reply in &run.calls {
                record_usage(&state, reply).await;
            }
            let steps: Vec<Value> = run
                .steps
                .iter()
                .map(|s| json!({ "step": s.step, "output": s.output, "model": s.model }))
                .collect();
            Ok(Json(json!({
                "plan": run.plan,
                "steps": steps,
                "answer": run.answer,
            })))
        }
        Err(llm::LlmError::Refused) => Ok(Json(json!({
            "answer": "Sorry, daar kan ik niet op antwoorden.",
            "plan": Value::Array(vec![]),
            "steps": Value::Array(vec![]),
        }))),
        Err(e) => {
            tracing::warn!(error = %e, "orchestration failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer je brein-config (router/keys/Ollama)",
                })),
            ))
        }
    }
}

// ---- Agentic execution: read-only, policy-gated, sandboxed (ADR-029 4a) -----

/// Run a single read-only agent action (ADR-029 phase 4a). Gated by the kill
/// switch + a configured sandbox; only `Auto` (read-only) actions run; every
/// attempt — ok, denied, or error — is written to the append-only audit log.
async fn agent_action(
    authed: Authed,
    State(state): State<AppState>,
    Json(action): Json<agent::Action>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !state.agent_enabled {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "agent disabled", "hint": "zet JARVIS_AGENT_ENABLED=true" })),
        ));
    }
    let Some(sandbox) = state.agent_sandbox.clone() else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "no workspace", "hint": "zet JARVIS_AGENT_WORKSPACE_ROOT" })),
        ));
    };
    let at = agent::action_type(&action).to_string();
    let risk = agent::classify(&action);
    let detail = serde_json::to_string(&action).ok();

    // Mutating actions (4b) need a device-signed approval: validate + preview,
    // store a pending action, and return its nonce for the owner to sign.
    if agent::is_mutating(&action) {
        let preview = match agent::preview(&sandbox, &action).await {
            Ok(p) => p,
            Err(e) => {
                // A protected/escaping target is refused now — no pending created.
                record_agent_audit(&state, authed.device.id, &at, detail, risk, "denied", Some(&e.to_string())).await;
                return Err((StatusCode::FORBIDDEN, Json(json!({ "error": e.to_string() }))));
            }
        };
        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
        let id = Uuid::now_v7();
        let action_json = serde_json::to_string(&action).unwrap_or_default();
        let res = sqlx::query(
            "INSERT INTO agent_pending_actions \
             (id, user_id, requesting_device_id, action_type, action, preview, nonce, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now() + interval '5 minutes')",
        )
        .bind(id)
        .bind(authed.user.id)
        .bind(authed.device.id)
        .bind(&at)
        .bind(&action_json)
        .bind(&preview)
        .bind(&nonce[..])
        .execute(&state.db)
        .await;
        if let Err(e) = res {
            tracing::warn!(error = %e, "failed to create pending action");
            return Err(db_err(e));
        }
        record_agent_audit(&state, authed.device.id, &at, detail, risk, "pending", None).await;
        return Ok(Json(json!({
            "needs_approval": true,
            "pending_id": id,
            "nonce": hex::encode(nonce),
            "action": at,
            "preview": preview,
        })));
    }

    match agent::execute(&sandbox, &action).await {
        Ok(outcome) => {
            let note = outcome.truncated.then_some("output truncated");
            record_agent_audit(&state, authed.device.id, &at, detail, risk, "ok", note).await;
            Ok(Json(json!({
                "action": at,
                "output": outcome.output,
                "truncated": outcome.truncated,
            })))
        }
        Err(e) => {
            let denied = matches!(
                e,
                agent::AgentError::Denied(_) | agent::AgentError::OutsideSandbox
            );
            let (label, code) = if denied {
                ("denied", StatusCode::FORBIDDEN)
            } else {
                ("error", StatusCode::BAD_REQUEST)
            };
            record_agent_audit(&state, authed.device.id, &at, detail, risk, label, Some(&e.to_string())).await;
            Err((code, Json(json!({ "error": e.to_string() }))))
        }
    }
}

/// Mutating actions awaiting the owner's device-signed approval (ADR-029 4b).
async fn agent_pending(authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT id, action_type, preview, encode(nonce, 'hex'), \
         to_char(created_at, 'YYYY-MM-DD HH24:MI:SS') \
         FROM agent_pending_actions \
         WHERE user_id = $1 AND status = 'pending' AND expires_at > now() \
         ORDER BY created_at DESC",
    )
    .bind(authed.user.id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(id, at, preview, nonce, created)| {
            json!({ "pending_id": id, "action": at, "preview": preview, "nonce": nonce, "created_at": created })
        })
        .collect();
    Json(json!({ "pending": entries }))
}

/// Approve a pending mutation by signing its nonce with a trusted device, then
/// execute it once (ADR-029 4b). The signature proves owner presence (the device
/// key is biometric-gated); the stored action is what runs — the LLM can propose,
/// only a signed human can commit.
async fn agent_pending_approve(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
    let signature =
        hex::decode(&req.signature).map_err(|_| bad_request("invalid signature encoding"))?;
    let Some(sandbox) = state.agent_sandbox.clone() else {
        return Err((StatusCode::FORBIDDEN, Json(json!({ "error": "no workspace" }))));
    };

    // Fetch the pending action (must be this user's, still pending + unexpired).
    let row: Option<(Vec<u8>, String, String)> = sqlx::query_as(
        "SELECT nonce, action, action_type FROM agent_pending_actions \
         WHERE id = $1 AND user_id = $2 AND status = 'pending' AND expires_at > now()",
    )
    .bind(id)
    .bind(authed.user.id)
    .fetch_optional(&state.db)
    .await
    .map_err(db_err)?;
    let (nonce, action_json, at) = row.ok_or_else(|| {
        (StatusCode::NOT_FOUND, Json(json!({ "error": "no such pending action" })))
    })?;

    // Verify the owner's device signature over the nonce.
    identity::verify_device_signature(&state.db, authed.user.id, authed.device.id, &nonce, &signature)
        .await
        .map_err(|_| unauthorized())?;

    // Consume it atomically — mark executed so it can never run twice (replay).
    let claimed = sqlx::query(
        "UPDATE agent_pending_actions SET status = 'executed', \
         approved_by_device_id = $2, resolved_at = now() \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(authed.device.id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;
    if claimed.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, Json(json!({ "error": "already resolved" }))));
    }

    let action: agent::Action = serde_json::from_str(&action_json).map_err(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "corrupt pending action" })))
    })?;

    match agent::execute(&sandbox, &action).await {
        Ok(outcome) => {
            let note = outcome.truncated.then_some("output truncated");
            record_agent_audit(&state, authed.device.id, &at, Some(action_json), agent::RiskClass::NeedsApproval, "ok", note).await;
            Ok(Json(json!({ "action": at, "output": outcome.output, "truncated": outcome.truncated })))
        }
        Err(e) => {
            record_agent_audit(&state, authed.device.id, &at, Some(action_json), agent::RiskClass::NeedsApproval, "error", Some(&e.to_string())).await;
            Err((StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))))
        }
    }
}

/// Deny a pending mutation (no signature needed — the denier is authenticated and
/// a denial only cancels).
async fn agent_pending_deny(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let res = sqlx::query(
        "UPDATE agent_pending_actions SET status = 'denied', resolved_at = now() \
         WHERE id = $1 AND user_id = $2 AND status = 'pending'",
    )
    .bind(id)
    .bind(authed.user.id)
    .execute(&state.db)
    .await
    .map_err(db_err)?;
    if res.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "no such pending action" }))));
    }
    record_agent_audit(&state, authed.device.id, "pending", None, agent::RiskClass::NeedsApproval, "denied", Some("denied by owner")).await;
    Ok(Json(json!({ "status": "denied" })))
}

/// Write one append-only audit row. Auditing must never break the action path,
/// so a DB failure is logged, not surfaced.
#[allow(clippy::too_many_arguments)]
async fn record_agent_audit(
    state: &AppState,
    device_id: Uuid,
    action_type: &str,
    detail: Option<String>,
    risk: agent::RiskClass,
    outcome: &str,
    note: Option<&str>,
) {
    let risk_str = match risk {
        agent::RiskClass::Auto => "auto",
        agent::RiskClass::NeedsApproval => "needs_approval",
        agent::RiskClass::Denied => "denied",
    };
    let res = sqlx::query(
        "INSERT INTO agent_audit (device_id, action_type, detail, risk_class, outcome, note) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(device_id)
    .bind(action_type)
    .bind(detail)
    .bind(risk_str)
    .bind(outcome)
    .bind(note)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "failed to write agent audit");
    }
}

/// The recent agent audit trail (ADR-029) — what Jarvis' hands have done.
async fn agent_audit_log(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let rows: Vec<(String, String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT action_type, risk_class, outcome, note, \
         to_char(ts, 'YYYY-MM-DD HH24:MI:SS') \
         FROM agent_audit ORDER BY ts DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(action, risk, outcome, note, ts)| {
            json!({ "action": action, "risk": risk, "outcome": outcome, "note": note, "ts": ts })
        })
        .collect();
    Json(json!({ "enabled": state.agent_enabled, "entries": entries }))
}

// ---- MCP: Jarvis' read-only tools over Streamable HTTP (ADR-031) ------------
//
// A minimal, defensive MCP server so Claude Code (and the owner's own Claude
// tooling) can *read* Jarvis' portfolio, status and memory. Every call is
// owner-scoped (the `Authed` extractor requires a valid session token) and
// read-only — no secrets, no Core, no mutations, no trading.
//
// The protocol shape has churned across MCP versions, so this is deliberately
// lenient: it answers both the classic `initialize` and the newer
// `server/discover` handshake, echoes the client's requested protocol version,
// and returns union results that satisfy either. The stable core — `tools/call`
// → `{content:[{type:"text"}], isError}` — is identical everywhere.

async fn mcp_endpoint(
    authed: Authed,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // DNS-rebinding guard: reject a browser Origin that isn't local. Non-browser
    // clients (Claude Code) send no Origin and are allowed.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !is_local_origin(origin) {
            return (StatusCode::FORBIDDEN, "bad origin").into_response();
        }
    }

    let id = body.get("id").cloned();
    // Notifications carry no id and expect no response body.
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let outcome: Result<Value, (i64, String)> = match method {
        "initialize" | "server/discover" => Ok(mcp_handshake(&body)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "resultType": "complete", "tools": mcp_tools() })),
        "tools/call" => {
            let name = body
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = body
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match mcp_call(&state, &authed, name, &args).await {
                Ok(text) => Ok(json!({
                    "resultType": "complete",
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                })),
                Err(ToolErr::Unknown) => Err((-32601, format!("onbekende tool: {name}"))),
                Err(ToolErr::Failed(msg)) => Ok(json!({
                    "resultType": "complete",
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true,
                })),
            }
        }
        other => Err((-32601, format!("methode niet gevonden: {other}"))),
    };

    let payload = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Json(payload).into_response()
}

/// A union handshake result that satisfies both `initialize` (classic) and
/// `server/discover`, echoing the client's requested protocol version.
fn mcp_handshake(body: &Value) -> Value {
    let requested = body
        .pointer("/params/protocolVersion")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("params")
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("2025-06-18");
    let info = json!({ "name": "jarvis", "version": env!("CARGO_PKG_VERSION") });
    json!({
        "resultType": "complete",
        "protocolVersion": requested,
        "supportedVersions": [requested],
        "capabilities": { "tools": {} },
        "serverInfo": info,
        "_meta": { "io.modelcontextprotocol/serverInfo": info },
        "instructions": "Read-only tools van Jarvis: portfolio, status en geheugen.",
    })
}

/// The read-only tool catalog. Definitions are static; execution is owner-scoped.
fn mcp_tools() -> Value {
    json!([
        {
            "name": "portfolio_summary",
            "description": "Jarvis' portfolio: posities, kostenbasis en allocatie (alleen-lezen).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "jarvis_status",
            "description": "Jarvis' ecosysteem: host, breinen, model-catalogus en maandbudget (alleen-lezen).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "recent_conversations",
            "description": "Recente gesprekstitels — Jarvis' geheugen, nieuwste eerst (alleen-lezen).",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "description": "max aantal (1-50)" } },
                "additionalProperties": false
            }
        }
    ])
}

enum ToolErr {
    Unknown,
    Failed(String),
}

/// Dispatch a read-only tool call, owner-scoped. Never mutates or exposes secrets.
async fn mcp_call(
    state: &AppState,
    authed: &Authed,
    name: &str,
    args: &Value,
) -> Result<String, ToolErr> {
    match name {
        "portfolio_summary" => mcp_portfolio(state, authed).await,
        "jarvis_status" => Ok(mcp_status(state)),
        "recent_conversations" => mcp_recent_conversations(state, authed, args).await,
        _ => Err(ToolErr::Unknown),
    }
}

async fn mcp_portfolio(state: &AppState, authed: &Authed) -> Result<String, ToolErr> {
    let holdings = portfolio::list_holdings(&state.db, authed.user.id)
        .await
        .map_err(|e| ToolErr::Failed(format!("portfolio niet leesbaar: {e}")))?;
    if holdings.is_empty() {
        return Ok("Geen posities in het portfolio.".to_string());
    }
    let total: Decimal = holdings.iter().map(|h| h.cost_basis()).sum();
    let hundred = Decimal::from(100);
    let mut s = format!(
        "Portfolio — {} posities, totale kostenbasis {}:\n",
        holdings.len(),
        total.normalize()
    );
    for h in &holdings {
        let cost = h.cost_basis();
        let weight = if total.is_zero() {
            Decimal::ZERO
        } else {
            (cost / total * hundred).round_dp(1)
        };
        s.push_str(&format!(
            "- {}: {} @ {} {} = {} ({}%)\n",
            h.symbol,
            h.quantity.normalize(),
            h.avg_cost.normalize(),
            h.currency,
            cost.normalize(),
            weight.normalize()
        ));
    }
    Ok(s)
}

fn mcp_status(state: &AppState) -> String {
    let eco = match state.registry.read() {
        Ok(reg) => render_ecosystem(&reg, state.agent_enabled, state.agent_sandbox.is_some()),
        Err(_) => "(ecosysteem tijdelijk niet leesbaar)".to_string(),
    };
    let spent = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget = state.budget_cents as f64 / 100.0;
    format!(
        "{eco}\nBudget: €{spent:.2} van €{budget:.2} gebruikt deze maand (€{:.2} over).",
        (budget - spent).max(0.0)
    )
}

async fn mcp_recent_conversations(
    state: &AppState,
    authed: &Authed,
    args: &Value,
) -> Result<String, ToolErr> {
    let limit = args
        .get("limit")
        .and_then(|l| l.as_i64())
        .unwrap_or(10)
        .clamp(1, 50);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, to_char(updated_at, 'YYYY-MM-DD HH24:MI') \
         FROM conversations WHERE user_id = $1 ORDER BY updated_at DESC LIMIT $2",
    )
    .bind(authed.user.id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ToolErr::Failed(format!("gesprekken niet leesbaar: {e}")))?;
    if rows.is_empty() {
        return Ok("Nog geen gesprekken.".to_string());
    }
    let mut s = String::from("Recente gesprekken:\n");
    for (title, updated) in rows {
        s.push_str(&format!("- {title} ({updated})\n"));
    }
    Ok(s)
}

/// Only local origins are allowed (DNS-rebinding protection); a client that sends
/// no Origin header (Claude Code) is allowed by the caller before this is reached.
fn is_local_origin(origin: &str) -> bool {
    origin.starts_with("http://localhost")
        || origin.starts_with("http://127.0.0.1")
        || origin.starts_with("https://localhost")
        || origin == "null"
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

#[derive(Deserialize)]
struct SelfImproveReq {
    /// Optional area to focus the advice on (e.g. "goedkopere modellen").
    #[serde(default)]
    focus: Option<String>,
}

/// Jarvis proposes improvements to ITSELF (ADR-029 fase 4d) — **advisory only**.
/// It reads its own ecosystem (registry + budget + agent capabilities) and returns
/// concrete proposals; it never acts. Carrying one out goes through the approval
/// gate (4b/4c); the Core and `Jarvis.md` stay owner-only, manual. On request only.
async fn system_self_improve(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<SelfImproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ecosystem = match state.registry.read() {
        Ok(reg) => render_ecosystem(&reg, state.agent_enabled, state.agent_sandbox.is_some()),
        Err(_) => "(ecosysteem tijdelijk niet leesbaar)".to_string(),
    };
    let spent_eur = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget_eur = state.budget_cents as f64 / 100.0;
    match selfdev::propose(
        &state.llm,
        &state.jarvis_system,
        &ecosystem,
        budget_eur,
        spent_eur,
        req.focus.as_deref(),
    )
    .await
    {
        Ok(report) => {
            for reply in &report.calls {
                record_usage(&state, reply).await;
            }
            let proposals: Vec<Value> = report
                .proposals
                .iter()
                .map(|p| {
                    json!({
                        "title": p.title,
                        "category": p.category,
                        "rationale": p.rationale,
                        "cost": p.cost,
                        "requires_approval": p.requires_approval,
                        "steps": p.steps,
                    })
                })
                .collect();
            Ok(Json(json!({
                "summary": report.summary,
                "proposals": proposals,
                "note": "Jarvis stelt alleen voor — uitvoeren gaat via jouw goedkeuring (4b/4c); \
                         de Core en Jarvis.md blijven handmatig, alleen door jou.",
            })))
        }
        Err(e) => {
            tracing::warn!(error = %e, "self-improve failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer je brein-config (router/keys/Ollama)",
                })),
            ))
        }
    }
}

/// Render the registry into a compact text snapshot for the self-dev advisor.
fn render_ecosystem(reg: &registry::Registry, agent_enabled: bool, has_workspace: bool) -> String {
    let h = &reg.host;
    let mut s = format!(
        "Host: {} {}, {} ({} cores), {:.1} GB RAM, GPU: {}\nActief brein: {}\n",
        h.os, h.arch, h.cpu, h.cpu_cores, h.mem_total_gb, h.gpu, reg.active_brain
    );
    s.push_str("\nBreinen:\n");
    for b in &reg.brains {
        s.push_str(&format!(
            "- {} [{}] beschikbaar: {} — {}\n",
            b.label,
            enum_str(&b.cost),
            yesno(b.available),
            b.note
        ));
    }
    s.push_str("\nModel-catalogus:\n");
    for m in &reg.models {
        s.push_str(&format!(
            "- {} ({}, {}, {}) beschikbaar: {}\n",
            m.id,
            m.backend,
            enum_str(&m.class),
            enum_str(&m.cost),
            yesno(m.available)
        ));
    }
    s.push_str("\nTools op de host:\n");
    for t in &reg.software {
        let v = t
            .version
            .as_deref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "- {}: {}{}\n",
            t.name,
            if t.present { "aanwezig" } else { "afwezig" },
            v
        ));
    }
    s.push_str(&format!(
        "\nAgent-capabilities: agent {}, werkmap {}\n",
        if agent_enabled { "AAN" } else { "uit" },
        if has_workspace { "geconfigureerd" } else { "geen" }
    ));
    s
}

/// Serialize a small lowercase-tagged enum (ModelClass/ModelCost/CostTier) to its
/// string form for display.
fn enum_str<T: serde::Serialize>(t: &T) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn yesno(b: bool) -> &'static str {
    if b {
        "ja"
    } else {
        "nee"
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
