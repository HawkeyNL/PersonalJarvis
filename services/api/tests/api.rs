//! Integration tests for the Jarvis API — full HTTP round-trips through the
//! public router against a real Postgres (via `#[sqlx::test]`). These exercise
//! the crate exactly as a client would: only its public surface (`build_router`,
//! `AppState`, and the re-exported auth limits) is used here. Unit tests for the
//! private handlers (livez/root/readyz) and pure helpers stay in `src/lib.rs`.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ed25519_dalek::{Signer, SigningKey};
use jarvis_agent as agent;
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use jarvis_api::{build_router, AppState, JARVIS_SYSTEM_FALLBACK};

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
        rate_limiter: std::sync::Arc::new(jarvis_api::RateLimiter::new()),
        auth_limits: jarvis_api::AuthLimits::default(),
        trusted_proxy_hops: 0,
        trusted_proxy_ips: std::sync::Arc::new(Vec::new()),
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
    let device_id = body_json(resp).await["device_id"]
        .as_str()
        .unwrap()
        .to_string();

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
        rate_limiter: std::sync::Arc::new(jarvis_api::RateLimiter::new()),
        auth_limits: jarvis_api::AuthLimits::default(),
        trusted_proxy_hops: 0,
        trusted_proxy_ips: std::sync::Arc::new(Vec::new()),
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
        rate_limiter: std::sync::Arc::new(jarvis_api::RateLimiter::new()),
        auth_limits: jarvis_api::AuthLimits::default(),
        trusted_proxy_hops: 0,
        trusted_proxy_ips: std::sync::Arc::new(Vec::new()),
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
    let (device_id, token) = enroll_and_login(&app, &signing).await;

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

    let audit_details: Vec<Option<String>> =
        sqlx::query_scalar("SELECT detail FROM agent_audit WHERE device_id = $1 ORDER BY ts")
            .bind(device_id.parse::<Uuid>().unwrap())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        audit_details.iter().all(Option::is_none),
        "agent audit must not store action payloads"
    );

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
    assert!(body["conversation_title"]
        .as_str()
        .unwrap()
        .contains("rust"));

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
        app.oneshot(
            b.body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
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
                    serde_json::to_vec(
                        &json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/list" }),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
