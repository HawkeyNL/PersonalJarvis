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
use sha2::Digest as _;
use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
use tower::ServiceExt;

use jarvis_api::{build_router, AppState, AuthLimits, RateLimiter, JARVIS_SYSTEM_FALLBACK};

#[path = "realtime/mod.rs"]
mod realtime;

#[path = "account/mod.rs"]
mod account;

async fn json_body(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn state(db: jarvis_store::Database, sandbox: Option<Sandbox>) -> AppState {
    AppState {
        realtime: Default::default(),
        db,
        environment: "test".to_string(),
        require_https: false,
        ibkr_gateway_url: "https://localhost:5000/v1/api".to_string(),
        llm: jarvis_llm::stub(),
        fast_intent_router: None,
        llm_max_tokens: 256,
        jarvis_system: Arc::from(JARVIS_SYSTEM_FALLBACK),
        speech: jarvis_speech::stub(),
        speech_verify_threshold: 0.5,
        registry: Arc::new(RwLock::new(jarvis_registry::Registry {
            host: jarvis_registry::HostInfo {
                os: "fixture".into(),
                arch: "x86_64".into(),
                cpu: "fixture".into(),
                cpu_cores: 1,
                mem_total_gb: 1.0,
                gpu: String::new(),
            },
            software: vec![],
            brains: vec![],
            models: vec![],
            active_brain: "fixture".into(),
        })),
        registry_input: Arc::new(jarvis_registry::CollectInput::default()),
        model_policy: Arc::new(jarvis_llm::LiveModelPolicy::new(
            jarvis_llm::ModelAccessPolicy::deny_by_default(),
        )),
        model_routing: Arc::default(),
        pricing_registry: Arc::new(jarvis_usage::PricingRegistry::builtin()),
        usage_snapshot_path: None,
        privileged_broker_socket: None,
        model_control: Arc::new(jarvis_api::model_control::ModelControl::new(None)),
        codex_broker_socket: None,
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
        agent_registry: None,
        agent_sandbox: sandbox.map(Arc::new),
        rate_limiter: Arc::new(RateLimiter::new()),
        auth_limits: AuthLimits::default(),
        trusted_proxy_hops: 0,
        trusted_proxy_ips: Arc::new(Vec::new()),
        bootstrap_enrollment: None,
        app_update_mirror: None,
    }
}

/// SurrealDB may reject the losing concurrent lease with a retryable
/// transaction conflict instead of matching zero rows. Both mean "not leased";
/// any other error still fails the test.
fn lost_race(
    result: Result<
        Option<jarvis_usage::coding_reservations::LeasedReservation>,
        jarvis_store::StoreError,
    >,
) -> Result<Option<jarvis_usage::coding_reservations::LeasedReservation>, jarvis_store::StoreError>
{
    match result {
        Err(error) if format!("{error:?}").contains("read or write conflict") => Ok(None),
        other => other,
    }
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn coding_subscription_reservation_is_server_issued_single_lease_and_zero_api_spend(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!(
        "reservation_fixture_{}",
        uuid::Uuid::now_v7().simple()
    ))
    .use_db("test")
    .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let user = uuid::Uuid::now_v7();
    let other_user = uuid::Uuid::now_v7();
    let session = uuid::Uuid::now_v7();
    let id = jarvis_usage::coding_reservations::reserve(&db, user, session).await?;
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        id,
        other_user,
        session,
        uuid::Uuid::now_v7(),
        60
    )
    .await?
    .is_none());
    let run_a = uuid::Uuid::now_v7();
    let run_b = uuid::Uuid::now_v7();
    let (a, b) = tokio::join!(
        jarvis_usage::coding_reservations::lease(&db, id, user, session, run_a, 60),
        jarvis_usage::coding_reservations::lease(&db, id, user, session, run_b, 60),
    );
    let a = lost_race(a)?;
    let b = lost_race(b)?;
    assert_eq!(
        usize::from(a.is_some()) + usize::from(b.is_some()),
        1,
        "concurrent lease must have exactly one winner"
    );
    let winner = if a.is_some() { run_a } else { run_b };
    assert!(jarvis_usage::coding_reservations::finish(&db, id, winner, true).await?);
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        id,
        user,
        session,
        uuid::Uuid::now_v7(),
        60
    )
    .await?
    .is_none());
    let mut query = db.query("SELECT status,api_spend_cents,compute_class,run_id FROM coding_reservations WHERE record::id(id)=$id LIMIT 1")
        .bind(json!({"id":id.to_string()})).await?.check()?;
    let rows: Vec<Value> = query.take(0)?;
    assert_eq!(rows[0]["status"], "settled");
    assert_eq!(rows[0]["compute_class"], "subscription");
    assert_eq!(rows[0]["api_spend_cents"], 0);
    assert_eq!(rows[0]["run_id"], winner.to_string());
    let expired = jarvis_usage::coding_reservations::reserve(&db, user, session).await?;
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        expired,
        user,
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7(),
        60
    )
    .await?
    .is_none());
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        expired,
        user,
        session,
        uuid::Uuid::now_v7(),
        u64::from(jarvis_usage::coding_reservations::MAX_RUNTIME_SECS) + 1
    )
    .await?
    .is_none());
    db.query("UPDATE coding_reservations SET expires_at=time::now()-1s WHERE record::id(id)=$id RETURN NONE")
        .bind(json!({"id":expired.to_string()})).await?.check()?;
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        expired,
        user,
        session,
        uuid::Uuid::now_v7(),
        60
    )
    .await?
    .is_none());
    let released = jarvis_usage::coding_reservations::reserve(&db, user, session).await?;
    let cancelled_run = uuid::Uuid::now_v7();
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        released,
        user,
        session,
        cancelled_run,
        60
    )
    .await?
    .is_some());
    assert!(jarvis_usage::coding_reservations::finish(&db, released, cancelled_run, false).await?);
    assert!(jarvis_usage::coding_reservations::lease(
        &db,
        released,
        user,
        session,
        uuid::Uuid::now_v7(),
        60
    )
    .await?
    .is_none());
    Ok(())
}

#[tokio::test]
async fn every_application_update_route_requires_authentication_before_storage_access() {
    // An unconnected database proves missing authentication does not query it.
    let mut fixture = state(jarvis_store::Database::init(), None).await;
    fixture.require_https = true;
    fixture.app_update_mirror = Some(
        jarvis_api::AppUpdateMirror::new(
            "/nonexistent-fixture/desktop",
            "https://jarvis.example.com",
        )
        .unwrap()
        .with_mobile_root("/nonexistent-fixture/mobile")
        .unwrap(),
    );
    let app = build_router(fixture);
    for path in [
        "/v1/app-updates/capability",
        "/v1/app-updates/stable/linux/x86_64/0.1.0?client_protocol=1",
        "/v1/app-updates/artifacts/0.1.0/linux/x86_64/Jarvis_0.1.0_linux_x86_64.AppImage",
        "/v1/app-updates/android/1?client_protocol=1",
        "/v1/app-updates/android/download",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

/// Owner read models for the app share the `Authed` gate of `/v1/system/*`.
const OWNER_READ_MODELS: [&str; 2] = ["/v1/agents", "/v1/system/services"];

#[tokio::test]
async fn owner_read_models_refuse_missing_or_invalid_sessions() {
    // An unconnected database cannot authenticate any token.
    let app = build_router(state(jarvis_store::Database::init(), None).await);
    for path in OWNER_READ_MODELS {
        for authorization in [None, Some("Bearer not-a-session"), Some("Basic b3duZXI=")] {
            let mut request = Request::builder().uri(path);
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path} {authorization:?}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn owner_read_models_answer_an_owner_session() -> Result<(), Box<dyn std::error::Error>> {
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
    let app = build_router(state(db, None).await);
    let (token, _) = enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let get = |path: &'static str| {
        app.clone().oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
    };

    // No bundle (development): an empty list with a reason, never a 500.
    let agents = get("/v1/agents").await?;
    assert_eq!(agents.status(), StatusCode::OK);
    let agents = json_body(agents).await;
    assert_eq!(agents["agents"], json!([]));
    assert_eq!(agents["agent_count"], 0);
    assert_eq!(agents["unavailable_reason"], "agent_bundle_unavailable");

    // Fixed labels in a fixed order, whatever the runner's systemd says.
    let services = get("/v1/system/services").await?;
    assert_eq!(services.status(), StatusCode::OK);
    let services = json_body(services).await;
    assert_eq!(services["services"][0]["label"], "Core");
    assert_eq!(services["services"].as_array().map(Vec::len), Some(7));
    assert_eq!(services["disks"][0]["label"], "system");
    assert_eq!(services["disks"][1]["label"], "data");
    Ok(())
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
    let app = build_router(state(db.clone(), Some(Sandbox::new(&root)?)).await);
    let signing = SigningKey::from_bytes(&rand::random());
    let (token, _) = enroll_login(&app, &signing).await;
    let device_id = {
        let me = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/auth/me")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        uuid::Uuid::parse_str(json_body(me).await["device_id"].as_str().unwrap())?
    };
    let post = |uri: String, body: Value| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    let propose = |content: &str| {
        post(
            "/v1/agent/action".into(),
            json!({"type":"write_file","path":"note.txt","content":content}),
        )
    };
    let approve = |id: &str, signature: &str| {
        post(
            format!("/v1/agent/pending/{id}/approve"),
            json!({ "signature": signature }),
        )
    };
    // Sign exactly what `GET /v1/agent/pending` lists for `id`.
    let list = || {
        Request::builder()
            .uri("/v1/agent/pending")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    let listed = json_body(app.clone().oneshot(list()).await.unwrap()).await;
    assert_eq!(listed["pending"].as_array().map(Vec::len), Some(0));
    let sign_listed = |listed: &Value, id: &str| -> String {
        let entry = listed["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pending_id"] == id)
            .unwrap();
        assert_eq!(entry["approval_message"], "agent-approval-v1");
        let message = jarvis_client_core::agent_approval_message(
            uuid::Uuid::parse_str(id).unwrap(),
            &hex::decode(entry["nonce"].as_str().unwrap()).unwrap(),
            &hex::decode(entry["action_sha256"].as_str().unwrap()).unwrap(),
            device_id,
        )
        .unwrap();
        hex::encode(signing.sign(&message).to_bytes())
    };

    let a = json_body(app.clone().oneshot(propose("ok")).await.unwrap()).await;
    let a_id = a["pending_id"].as_str().unwrap().to_string();
    let b = json_body(app.clone().oneshot(propose("other")).await.unwrap()).await;
    let b_id = b["pending_id"].as_str().unwrap().to_string();
    let listed = json_body(app.clone().oneshot(list()).await.unwrap()).await;
    let a_entry = listed["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pending_id"] == a_id.as_str())
        .unwrap();
    assert_eq!(
        a_entry["action_sha256"],
        hex::encode(sha2::Sha256::digest(
            br#"{"type":"write_file","path":"note.txt","content":"ok"}"#
        ))
    );
    let a_signature = sign_listed(&listed, &a_id);
    let b_signature = sign_listed(&listed, &b_id);

    // A login/unlock-style raw nonce signature is refused.
    let raw = hex::encode(
        signing
            .sign(&hex::decode(a["nonce"].as_str().unwrap())?)
            .to_bytes(),
    );
    assert_eq!(
        app.clone()
            .oneshot(approve(&a_id, &raw))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // A signature for action A never approves action B.
    assert_eq!(
        app.clone()
            .oneshot(approve(&b_id, &a_signature))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // The kill switch refuses approval before anything runs.
    let mut disabled = state(db.clone(), Some(Sandbox::new(&root)?)).await;
    disabled.agent_enabled = false;
    assert_eq!(
        build_router(disabled)
            .oneshot(approve(&a_id, &a_signature))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // A stored action changed after it was listed no longer matches its hash.
    db.query("UPDATE agent_pending_actions SET action = $action WHERE record::id(id) = $id")
        .bind(json!({
            "id": b_id,
            "action": r#"{"type":"write_file","path":"note.txt","content":"tampered"}"#,
        }))
        .await?
        .check()?;
    assert_eq!(
        app.clone()
            .oneshot(approve(&b_id, &b_signature))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Deny still needs no signature.
    let denied = app
        .clone()
        .oneshot(post(format!("/v1/agent/pending/{b_id}/deny"), json!({})))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(json_body(denied).await["status"], "denied");

    assert_eq!(
        app.clone()
            .oneshot(approve(&a_id, &a_signature))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(std::fs::read_to_string(root.join("note.txt"))?, "ok");
    assert_eq!(
        app.clone()
            .oneshot(approve(&a_id, &a_signature))
            .await
            .unwrap()
            .status(),
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

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn current_month_usage_aggregates_work_before_and_after_first_call(
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
    db.use_ns(format!("jarvis_usage_{}", uuid::Uuid::now_v7().simple()))
        .use_db("core")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;

    let empty = jarvis_usage::month_statistics(&db).await?;
    assert_eq!(empty.totals.requests, 0);
    assert!(empty.by_backend.is_empty());
    assert!(empty.by_model.is_empty());
    assert!(empty.daily.is_empty());

    jarvis_usage::record(
        &db,
        &jarvis_usage::UsageEntry {
            request_id: uuid::Uuid::now_v7().to_string(),
            backend: "ollama-cloud".to_owned(),
            model: "fixture-model".to_owned(),
            requested_route: None,
            actual_provider: None,
            cost_estimate_classification: "known".to_owned(),
            routing_mode: "test".to_owned(),
            quality_tier: "test".to_owned(),
            agent_id: None,
            latency_ms: 25,
            status: "succeeded".to_owned(),
            failure_category: None,
            fallback_count: 0,
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
            cost_eur: 0.01,
        },
    )
    .await?;

    let populated = jarvis_usage::month_statistics(&db).await?;
    assert_eq!(populated.totals.requests, 1);
    assert_eq!(populated.totals.total_tokens, 135);
    assert_eq!(populated.by_backend.len(), 1);
    assert_eq!(populated.by_backend[0].backend, "ollama-cloud");
    assert_eq!(populated.by_model.len(), 1);
    assert_eq!(
        populated.by_model[0].model.as_deref(),
        Some("fixture-model")
    );
    assert_eq!(populated.daily.len(), 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn month_statistics_break_down_agents_latency_failures_and_fallbacks(
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
    db.use_ns(format!("jarvis_usage_{}", uuid::Uuid::now_v7().simple()))
        .use_db("core")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;

    // A pre-telemetry row has none of the routing fields and must not break
    // the aggregate (NONE latency, failure category and fallback count).
    db.query(
        "CREATE llm_usage SET id = $id, ts = time::now(), backend = 'ollama-cloud', model = 'legacy', \
         input_tokens = 1, output_tokens = 1, cache_read_tokens = 0, cache_write_tokens = 0, cost_eur = 0.0",
    )
    .bind(json!({"id": uuid::Uuid::now_v7().to_string()}))
    .await?
    .check()?;
    let entry = |agent: Option<&str>, latency_ms, failure: Option<&str>, fallback_count| {
        jarvis_usage::UsageEntry {
            request_id: uuid::Uuid::now_v7().to_string(),
            backend: "ollama-cloud".to_owned(),
            model: "fixture-model".to_owned(),
            requested_route: None,
            actual_provider: None,
            cost_estimate_classification: "known".to_owned(),
            routing_mode: "test".to_owned(),
            quality_tier: "test".to_owned(),
            agent_id: agent.map(str::to_owned),
            latency_ms,
            status: if failure.is_some() {
                "failed"
            } else {
                "succeeded"
            }
            .to_owned(),
            failure_category: failure.map(str::to_owned),
            fallback_count,
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_eur: 0.01,
        }
    };
    for row in [
        entry(Some("researcher"), 100, None, 0),
        entry(Some("researcher"), 300, Some("timeout"), 2),
        // Unmeasured internal call: counted, but excluded from latency.
        entry(None, 0, None, 0),
    ] {
        jarvis_usage::record(&db, &row).await?;
    }

    let stats = jarvis_usage::month_statistics(&db).await?;
    assert_eq!(stats.totals.requests, 4);
    assert_eq!(stats.totals.failures, 1);
    assert_eq!(stats.totals.fallbacks, 2);
    let p50 = stats.totals.latency_p50_ms.ok_or("missing p50")?;
    let p95 = stats.totals.latency_p95_ms.ok_or("missing p95")?;
    assert!((100..=300).contains(&p50) && (p50..=300).contains(&p95));
    assert_eq!(stats.by_backend.len(), 1);
    assert_eq!(stats.by_backend[0].totals.failures, 1);
    assert!(stats.by_backend[0].totals.latency_p95_ms.is_some());

    let agents = jarvis_usage::month_agent_statistics(&db).await?;
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.agent_id, "researcher");
    assert_eq!((agent.totals.requests, agent.totals.total_tokens), (2, 30));
    assert_eq!((agent.totals.failures, agent.totals.fallbacks), (1, 2));
    assert!(agent.totals.latency_p50_ms.is_some());
    // RFC 3339 text from `time::format(.., '%+')`, e.g. 2026-10-04T11:07:31.123+00:00.
    let last_used = agent.last_used.as_deref().ok_or("missing last_used")?;
    assert_eq!(last_used.as_bytes().get(10), Some(&b'T'), "{last_used}");
    assert!(last_used.ends_with("+00:00"), "{last_used}");

    assert_eq!(stats.failures_by_category.len(), 1);
    assert_eq!(stats.failures_by_category[0].category, "timeout");
    assert_eq!(stats.failures_by_category[0].requests, 1);
    Ok(())
}
