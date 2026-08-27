//! Security-critical HTTP round trips against a disposable SurrealDB service.

use std::{
    env,
    sync::{Arc, RwLock},
};

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use ed25519_dalek::{Signer, SigningKey};
use jarvis_agent::Sandbox;
use serde_json::{json, Value};
use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
use tower::ServiceExt;

use jarvis_api::{build_router, AppState, AuthLimits, RateLimiter, JARVIS_SYSTEM_FALLBACK};

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn state(db: jarvis_store::Database, sandbox: Option<Sandbox>) -> AppState {
    AppState {
        db,
        environment: "test".to_string(),
        require_https: false,
        ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
        llm: jarvis_llm::stub(),
        llm_max_tokens: 256,
        jarvis_system: Arc::from(JARVIS_SYSTEM_FALLBACK),
        speech: jarvis_speech::stub(),
        speech_verify_threshold: 0.5,
        registry: Arc::new(RwLock::new(
            jarvis_registry::collect(&jarvis_registry::CollectInput::default()).await,
        )),
        registry_input: Arc::new(jarvis_registry::CollectInput::default()),
        model_policy: Arc::new(jarvis_llm::ModelAccessPolicy::deny_by_default()),
        budget_cents: 5000,
        spent_cents: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        budget_book: Arc::new(jarvis_usage::BudgetBook::new(
            jarvis_usage::BudgetLimits {
                monthly_soft_cents: 4_000,
                monthly_hard_cents: 5_000,
                per_request_hard_cents: 500,
            },
            0,
        )),
        eur_per_usd: 0.92,
        agent_enabled: sandbox.is_some(),
        agent_sandbox: sandbox.map(Arc::new),
        rate_limiter: Arc::new(RateLimiter::new()),
        auth_limits: AuthLimits::default(),
        trusted_proxy_hops: 0,
        trusted_proxy_ips: Arc::new(Vec::new()),
        bootstrap_enrollment: None,
    }
}

async fn enroll_login(app: &axum::Router, signing: &SigningKey) -> (String, Vec<u8>) {
    let enroll = app.clone().oneshot(Request::builder().method("POST").uri("/v1/auth/enroll")
        .header(header::CONTENT_TYPE, "application/json").body(Body::from(serde_json::to_vec(&json!({"name":"test", "platform":"ios", "public_key": hex::encode(signing.verifying_key().to_bytes())})).unwrap())).unwrap()).await.unwrap();
    assert_eq!(enroll.status(), StatusCode::OK);
    let device_id = json_body(enroll).await["device_id"]
        .as_str()
        .unwrap()
        .to_string();
    let challenge = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/challenge")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({"device_id": device_id})).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let challenge = json_body(challenge).await;
    let nonce = hex::decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let login = app.clone().oneshot(Request::builder().method("POST").uri("/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json").body(Body::from(serde_json::to_vec(&json!({"device_id": device_id, "challenge_id": challenge["challenge_id"], "signature": hex::encode(signing.sign(&nonce).to_bytes())})).unwrap())).unwrap()).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    (
        json_body(login).await["token"]
            .as_str()
            .unwrap()
            .to_string(),
        nonce,
    )
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn signed_agent_approval_is_single_use_and_core_stays_denied(
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("JARVIS_SURREAL_TEST_ENDPOINT")?;
    let user = env::var("JARVIS_SURREAL_TEST_USER")?;
    let pass = env::var("JARVIS_SURREAL_TEST_PASS")?;
    let db = Surreal::new::<Ws>(&endpoint).await?;
    db.signin(Root {
        username: &user,
        password: &pass,
    })
    .await?;
    db.use_ns(format!("jarvis_api_{}", uuid::Uuid::now_v7().simple()))
        .use_db("core")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let root = std::env::temp_dir().join(format!("jarvis_surreal_agent_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root)?;
    let app = build_router(state(db, Some(Sandbox::new(&root)?)).await);
    let signing = SigningKey::from_bytes(&rand::random());
    let (token, _) = enroll_login(&app, &signing).await;
    let pending = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent/action")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(
                        &json!({"type":"write_file","path":"note.txt","content":"ok"}),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending.status(), StatusCode::OK);
    let pending = json_body(pending).await;
    let id = pending["pending_id"].as_str().unwrap();
    let signature = hex::encode(
        signing
            .sign(&hex::decode(pending["nonce"].as_str().unwrap())?)
            .to_bytes(),
    );
    let approve = || {
        Request::builder()
            .method("POST")
            .uri(format!("/v1/agent/pending/{id}/approve"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(
                serde_json::to_vec(&json!({"signature": signature})).unwrap(),
            ))
            .unwrap()
    };
    assert_eq!(
        app.clone().oneshot(approve()).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(std::fs::read_to_string(root.join("note.txt"))?, "ok");
    assert_eq!(
        app.clone().oneshot(approve()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let core = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent/action")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(
                    serde_json::to_vec(
                        &json!({"type":"write_file","path":"jarvis-core/Jarvis.md","content":"no"}),
                    )
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(core.status(), StatusCode::FORBIDDEN);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
