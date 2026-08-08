//! Jarvis API / BFF — Axum router, handlers, and the auth extractor.
//!
//! Public endpoints: liveness/readiness, and device-bound auth
//! (`/v1/auth/challenge`, `/v1/auth/login`). Protected endpoints require a
//! `Bearer` session token (see [`Authed`]).

use axum::{
    extract::{FromRequestParts, State},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

use jarvis_identity as identity;

/// Shared, cheaply-cloneable application state.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub environment: String,
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
        .route("/v1/devices", get(list_devices))
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

/// Authenticated principal, extracted from a `Bearer` session token.
pub struct Authed {
    pub user: identity::User,
    pub device: identity::Device,
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
        let (user, device) = identity::authenticate(&state.db, token)
            .await
            .map_err(|_| unauthorized())?;
        Ok(Authed { user, device })
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
    }
}
