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

use jarvis_api::{
    build_router, build_tcp_router, AppState, AuthLimits, RateLimiter, JARVIS_SYSTEM_FALLBACK,
};

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
        db: db.clone(),
        environment: "test".to_string(),
        require_https: false,
        dev_enrollment: true,
        ibkr: None,
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
        budget_gate: Arc::new(jarvis_api::budget_gate::BudgetGate::new(
            jarvis_usage::BudgetBook::new(
                jarvis_usage::BudgetLimits {
                    monthly_soft_cents: 4_000,
                    monthly_hard_cents: 5_000,
                    per_request_hard_cents: 500,
                },
                0,
            ),
            Arc::new(db),
        )),
        eur_per_usd: 0.92,
        agent_enabled: sandbox.is_some(),
        agent_registry: None,
        agent_sandbox: sandbox.map(Arc::new),
        rate_limiter: Arc::new(RateLimiter::new()),
        auth_limits: AuthLimits::default(),
        unix_proxy_uid: None,
        bootstrap_enrollment: None,
        app_update_mirror: None,
        notifier: Default::default(),
        memecoin: Default::default(),
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
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn durable_budget_reservations_survive_restart_and_expire(
) -> Result<(), Box<dyn std::error::Error>> {
    use jarvis_api::budget_gate::BudgetGate;
    use std::time::Duration;

    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("budget_fixture_{}", uuid::Uuid::now_v7().simple()))
        .use_db("test")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let gate = || {
        BudgetGate::new(
            jarvis_usage::BudgetBook::new(
                jarvis_usage::BudgetLimits {
                    monthly_soft_cents: 100,
                    monthly_hard_cents: 100,
                    per_request_hard_cents: 100,
                },
                0,
            ),
            Arc::new(db.clone()),
        )
    };
    let before = gate();
    let held = before.reserve("held", 60, Duration::from_secs(600)).await;
    let lapsing = before.reserve("lapsing", 40, Duration::from_secs(1)).await;
    assert_eq!((held, lapsing), (Ok(()), Ok(())));
    tokio::time::sleep(Duration::from_millis(2_100)).await;

    // A fresh gate stands in for a restarted Core.
    let after = gate();
    after.reconcile().await?;
    assert_eq!(after.snapshot().reserved_cents, 60);
    assert!(after.cancel("held").await);
    after.reconcile().await?;
    assert_eq!(after.snapshot().reserved_cents, 0);
    let mut rows = db
        .query("SELECT task_id, status FROM llm_budget_reservations ORDER BY task_id")
        .await?
        .check()?;
    let rows: Vec<Value> = rows.take(0)?;
    let statuses: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["status"].as_str())
        .collect();
    assert_eq!(statuses, ["released", "expired"]);
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
        "/v1/app-updates/ios/0.1.0",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn api_responses_are_never_cached_by_default() {
    let app = build_router(state(jarvis_store::Database::init(), None).await);
    for path in ["/livez", "/v1/auth/me"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{path}"
        );
    }
}

#[tokio::test]
async fn production_tcp_listener_serves_only_health_probes() {
    let mut fixture = state(jarvis_store::Database::init(), None).await;
    fixture.environment = "production".to_string();
    let app = build_tcp_router(fixture);
    let get = |path: &str| {
        Request::builder()
            .uri(path)
            // A local process forging the proxy's headers gains nothing.
            .header("x-forwarded-for", "10.23.45.10")
            .header("x-forwarded-proto", "https")
            .header(header::AUTHORIZATION, "Bearer forged")
            .body(Body::empty())
            .unwrap()
    };
    for path in [
        "/",
        "/v1/auth/me",
        "/v1/auth/challenge",
        "/v1/events",
        "/mcp",
    ] {
        let response = app.clone().oneshot(get(path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let response = app.clone().oneshot(get("/livez")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Routed to the probe: an unconnected database reports not ready.
    let response = app.oneshot(get("/readyz")).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(response).await["status"], "degraded");

    // Outside production TCP keeps the full API for development and tests.
    let response = build_tcp_router(state(jarvis_store::Database::init(), None).await)
        .oneshot(get("/v1/auth/me"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dev_enrollment_is_refused_unless_enabled_outside_production() {
    let body = json!({"name": "attacker", "platform": "ios", "public_key": hex::encode([7u8; 32])});
    for (environment, dev_enrollment) in [("test", false), ("production", true)] {
        let mut fixture = state(jarvis_store::Database::init(), None).await;
        fixture.environment = environment.to_string();
        fixture.dev_enrollment = dev_enrollment;
        let response = build_router(fixture)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/enroll")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{environment} {dev_enrollment}"
        );
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

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn owner_brain_preference_round_trips() -> Result<(), Box<dyn std::error::Error>> {
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
    let request = |method: &'static str, body: Option<Value>| {
        let builder = Request::builder()
            .method(method)
            .uri("/v1/system/brain")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json");
        app.clone().oneshot(
            builder
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
    };

    // Local Ollama is allowed without a policy entry. Set twice: the second
    // write must update the same row, not trip the unique user index.
    for model in ["llama3", "qwen3"] {
        let set = request("PUT", Some(json!({"provider": "ollama", "model": model}))).await?;
        assert_eq!(set.status(), StatusCode::OK);
        let get = json_body(request("GET", None).await?).await;
        assert_eq!(
            get["default"],
            json!({"mode": "pinned", "provider": "ollama", "model": model})
        );
    }

    let auto = request("PUT", Some(json!({"provider": null, "model": null}))).await?;
    assert_eq!(auto.status(), StatusCode::OK);
    let get = json_body(request("GET", None).await?).await;
    assert_eq!(get["default"], json!({"mode": "auto"}));
    Ok(())
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn security_audit_retention_and_dropped_summary() -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("audit_fixture_{}", uuid::Uuid::now_v7().simple()))
        .use_db("test")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let mut fixture = state(db.clone(), None).await;
    // Let every failed login reach the handler; only the audit budget applies.
    fixture.auth_limits.login_per_min = 100;
    fixture.auth_limits.login_max_failures = 100;
    let app = build_router(fixture.clone());

    // Eleven unauthenticated failures: ten fill the per-kind budget, one is
    // dropped and counted.
    for _ in 0..11 {
        let body = json!({
            "device_id": uuid::Uuid::now_v7(),
            "challenge_id": uuid::Uuid::now_v7(),
            "signature": "00".repeat(64),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body)?))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    for (age, event) in [
        ("181d", "fixture.old"),
        ("200d", "fixture.old"),
        ("1000d", "fixture.old"),
        ("1d", "fixture.recent"),
        ("179d", "fixture.recent"),
    ] {
        db.query(format!(
            "CREATE security_audit SET id = $id, ts = time::now() - {age}, \
             device_id = NONE, event = $event, outcome = 'ok', detail = NONE RETURN NONE"
        ))
        .bind(json!({"id": uuid::Uuid::now_v7().to_string(), "event": event}))
        .await?
        .check()?;
    }

    assert_eq!(jarvis_api::prune_security_audit(&db).await?, 3);
    jarvis_api::run_audit_maintenance(&fixture).await;

    let count = |event: &'static str| {
        let db = db.clone();
        async move {
            let mut response = db
                .query("SELECT count() AS n FROM security_audit WHERE event = $event GROUP ALL")
                .bind(json!({"event": event}))
                .await?
                .check()?;
            let rows: Vec<Value> = response.take(0)?;
            Ok::<_, surrealdb::Error>(rows.first().and_then(|r| r["n"].as_u64()).unwrap_or(0))
        }
    };
    assert_eq!(count("fixture.old").await?, 0);
    assert_eq!(count("fixture.recent").await?, 2);
    assert_eq!(count("auth.login").await?, 10);
    let mut summary = db
        .query("SELECT outcome, detail FROM security_audit WHERE event = 'audit.unauthenticated'")
        .await?
        .check()?;
    let summary: Vec<Value> = summary.take(0)?;
    assert_eq!(summary, vec![json!({"outcome": "dropped", "detail": "1"})]);
    // Nothing left to prune or report.
    assert_eq!(jarvis_api::prune_security_audit(&db).await?, 0);
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
    let login = |signature: [u8; 64]| {
        app.clone().oneshot(Request::builder().method("POST").uri("/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json").body(Body::from(serde_json::to_vec(&json!({"device_id": device_id, "challenge_id": challenge["challenge_id"], "signature": hex::encode(signature)})).unwrap())).unwrap())
    };
    // A signature over the bare nonce is not a login proof.
    let raw = login(signing.sign(&nonce).to_bytes()).await.unwrap();
    assert_eq!(raw.status(), StatusCode::UNAUTHORIZED);
    let message = jarvis_client_core::login_message(
        challenge["challenge_id"].as_str().unwrap().parse().unwrap(),
        device_id.parse().unwrap(),
        &nonce,
    )
    .unwrap();
    let login = login(signing.sign(&message).to_bytes()).await.unwrap();
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
async fn unlock_requests_are_rate_limited_superseded_and_audited(
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
    let app = build_router(state(db.clone(), None).await);
    let (token, _) = enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let request = |uri: String, method: &str| {
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
    };
    let mut ids = Vec::new();
    for _ in 0..3 {
        let response = request("/v1/auth/unlock/request".into(), "POST").await?;
        assert_eq!(response.status(), StatusCode::OK);
        ids.push(
            json_body(response).await["request_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let limited = request("/v1/auth/unlock/request".into(), "POST").await?;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    // Superseded requests are denied in storage but still read as pending.
    let live: Vec<String> = db
        .query("SELECT VALUE record::id(id) FROM unlock_requests WHERE status = 'pending'")
        .await?
        .take(0)?;
    assert_eq!(live, vec![ids[2].clone()]);
    let status = request(format!("/v1/auth/unlock/{}", ids[0]), "GET").await?;
    assert_eq!(json_body(status).await["status"], "pending");
    let mut audited: Vec<String> = db
        .query("SELECT VALUE detail FROM security_audit WHERE event = 'unlock.request' AND outcome = 'created'")
        .await?
        .take(0)?;
    audited.sort();
    ids.sort();
    assert_eq!(audited, ids);
    Ok(())
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn push_targets_are_bound_to_the_caller_validated_and_revoked_with_the_device(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("jarvis_api_{}", uuid::Uuid::now_v7().simple()))
        .use_db("core")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let app = build_router(state(db.clone(), None).await);
    let (phone, _) = enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let (laptop, _) = enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let call = |token: &str, method: &str, body: Option<Value>| {
        let mut request = Request::builder()
            .method(method)
            .uri("/v1/devices/self/push")
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        app.clone().oneshot(
            request
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
    };
    let device_of = |token: String| {
        let app = app.clone();
        async move {
            let me = app
                .oneshot(
                    Request::builder()
                        .uri("/v1/auth/me")
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            json_body(me).await["device_id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap()
        }
    };
    let (phone_id, laptop_id) = (
        device_of(phone.clone()).await,
        device_of(laptop.clone()).await,
    );
    let user = jarvis_identity::get_device(&db, phone_id)
        .await?
        .unwrap()
        .user_id;
    let phone_token = "AB".repeat(32);
    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/devices/self/push")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"apns_token": phone_token, "apns_env": "sandbox"}).to_string(),
                ))
                .unwrap(),
        )
        .await?;
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    for bad in [
        json!({"apns_token": "ab".repeat(31), "apns_env": "sandbox"}),
        json!({"apns_token": "ab".repeat(101), "apns_env": "sandbox"}),
        json!({"apns_token": "zz".repeat(32), "apns_env": "sandbox"}),
        json!({"apns_token": "ab".repeat(32), "apns_env": "development"}),
    ] {
        assert_eq!(
            call(&phone, "PUT", Some(bad)).await?.status(),
            StatusCode::BAD_REQUEST
        );
    }
    // Unknown fields (such as another device id) are refused, not ignored.
    let foreign =
        json!({"apns_token": "ab".repeat(32), "apns_env": "sandbox", "device_id": laptop_id});
    assert!(call(&phone, "PUT", Some(foreign))
        .await?
        .status()
        .is_client_error());
    let set = |token: &str, apns: &str| {
        call(
            token,
            "PUT",
            Some(json!({"apns_token": apns, "apns_env": "sandbox"})),
        )
    };
    // The rate limit counts every write, valid or not: continue on a fresh
    // limiter so the budget below is exact.
    assert_eq!(
        set(&laptop, &"cd".repeat(32)).await?.status(),
        StatusCode::NO_CONTENT
    );
    let fresh = build_router(state(db.clone(), None).await);
    let set_fresh = |token: &str, apns: String| {
        fresh.clone().oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/devices/self/push")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"apns_token": apns, "apns_env": "sandbox"}).to_string(),
                ))
                .unwrap(),
        )
    };
    assert_eq!(
        set_fresh(&phone, phone_token.clone()).await?.status(),
        StatusCode::NO_CONTENT
    );
    // Stored lowercase, one row per device; the requester is never a target.
    let targets = |except| jarvis_identity::apns_targets(&db, user, except, "sandbox", 8);
    let mut all = targets(None).await?;
    all.sort();
    assert_eq!(all, ["ab".repeat(32), "cd".repeat(32)]);
    assert_eq!(targets(Some(laptop_id)).await?, ["ab".repeat(32)]);
    assert!(
        jarvis_identity::apns_targets(&db, user, None, "production", 8)
            .await?
            .is_empty()
    );
    // A token re-registered by another device moves to it.
    assert_eq!(
        set_fresh(&laptop, phone_token.clone()).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(targets(Some(laptop_id)).await?, Vec::<String>::new());
    assert_eq!(
        set_fresh(&phone, phone_token.clone()).await?.status(),
        StatusCode::NO_CONTENT
    );
    // Six writes per minute: two used above.
    for _ in 0..4 {
        assert_eq!(
            set_fresh(&phone, phone_token.clone()).await?.status(),
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        set_fresh(&phone, phone_token.clone()).await?.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    // DELETE removes only the caller's own target.
    assert_eq!(
        call(&laptop, "DELETE", None).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(targets(None).await?, ["ab".repeat(32)]);
    // Revoking the device deletes its target with it.
    jarvis_identity::revoke_device(&db, phone_id).await?;
    let left: Vec<Value> = db
        .query("SELECT * FROM device_push_targets")
        .await?
        .take(0)?;
    assert!(left.is_empty());

    // A token held by another account's device never moves across accounts,
    // and the stranger's device cannot take this account's token either.
    let stranger = jarvis_identity::create_user(&db, "other fixture owner").await?;
    let (stranger_device, _) = jarvis_identity::register_device(
        &db,
        stranger.id,
        "other phone",
        jarvis_identity::Platform::Ios,
        "ed25519",
        &SigningKey::from_bytes(&rand::random())
            .verifying_key()
            .to_bytes(),
    )
    .await?;
    let stranger_token = "ef".repeat(32);
    jarvis_identity::set_push_target(&db, stranger_device.id, &stranger_token, "sandbox").await?;
    assert_eq!(
        set(&laptop, &stranger_token).await?.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        set(&laptop, &"cd".repeat(32)).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert!(matches!(
        jarvis_identity::set_push_target(&db, stranger_device.id, &"cd".repeat(32), "sandbox")
            .await,
        Err(jarvis_identity::IdentityError::AuthFailed)
    ));
    assert_eq!(
        jarvis_identity::apns_targets(&db, stranger.id, None, "sandbox", 8).await?,
        [stranger_token]
    );
    assert_eq!(targets(None).await?, ["cd".repeat(32)]);
    Ok(())
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
    // The list carries the exact bytes `action_sha256` hashes.
    assert_eq!(
        a_entry["action_json"],
        r#"{"type":"write_file","path":"note.txt","content":"ok"}"#
    );
    let a_signature = sign_listed(&listed, &a_id);
    let b_signature = sign_listed(&listed, &b_id);

    // A phone cannot approve an action too large to list in full, nor a kind
    // outside the mobile allowlist, even with a valid signature. The limit is
    // on the stored JSON: 16384 bytes are listed, 16385 are not.
    let overhead = r#"{"type":"write_file","path":"note.txt","content":""}"#.len();
    let max = jarvis_client_core::MAX_REVIEWABLE_ACTION_JSON_LEN;
    let at_limit = json_body(
        app.clone()
            .oneshot(propose(&"x".repeat(max - overhead)))
            .await
            .unwrap(),
    )
    .await;
    let at_limit_id = at_limit["pending_id"].as_str().unwrap().to_string();
    let large = json_body(
        app.clone()
            .oneshot(propose(&"x".repeat(max + 1 - overhead)))
            .await
            .unwrap(),
    )
    .await;
    let large_id = large["pending_id"].as_str().unwrap().to_string();
    let other = json_body(app.clone().oneshot(propose("kind")).await.unwrap()).await;
    let other_id = other["pending_id"].as_str().unwrap().to_string();
    db.query("UPDATE agent_pending_actions SET action = $action WHERE record::id(id) = $id")
        .bind(json!({"id": other_id, "action": r#"{"type":"claude_code","prompt":"x"}"#}))
        .await?
        .check()?;
    let listed = json_body(app.clone().oneshot(list()).await.unwrap()).await;
    let large_entry = listed["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pending_id"] == large_id.as_str())
        .unwrap();
    assert!(large_entry["action_json"].is_null());
    let at_limit_entry = listed["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pending_id"] == at_limit_id.as_str())
        .unwrap();
    assert_eq!(
        at_limit_entry["action_json"].as_str().map(str::len),
        Some(max)
    );
    // A device that cannot approve never receives action_json.
    let set_platform = |platform: &'static str| {
        let db = db.clone();
        async move {
            db.query("UPDATE devices SET platform = $platform WHERE record::id(id) = $id")
                .bind(json!({"platform": platform, "id": device_id.to_string()}))
                .await?
                .check()
        }
    };
    set_platform("linux").await?;
    let as_linux = json_body(app.clone().oneshot(list()).await.unwrap()).await;
    set_platform("ios").await?;
    assert!(as_linux["pending"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["action_json"].is_null()));
    for id in [&large_id, &other_id] {
        let signature = sign_listed(&listed, id);
        assert_eq!(
            app.clone()
                .oneshot(approve(id, &signature))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

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
        .clone()
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
    // At most 16 live pending actions per user; three (at_limit, large,
    // other) are live. A deny or an expiry frees a slot.
    let mut more = Vec::new();
    for _ in 0..13 {
        let created = app.clone().oneshot(propose("more")).await.unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        more.push(
            json_body(created).await["pending_id"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let status = |response: axum::response::Response| response.status();
    assert_eq!(
        status(app.clone().oneshot(propose("one too many")).await.unwrap()),
        StatusCode::TOO_MANY_REQUESTS
    );
    let listed = json_body(app.clone().oneshot(list()).await.unwrap()).await;
    assert_eq!(listed["pending"].as_array().map(Vec::len), Some(16));
    let denied = app
        .clone()
        .oneshot(post(
            format!("/v1/agent/pending/{}/deny", more[0]),
            json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(
        status(app.clone().oneshot(propose("after deny")).await.unwrap()),
        StatusCode::OK
    );
    assert_eq!(
        status(app.clone().oneshot(propose("full again")).await.unwrap()),
        StatusCode::TOO_MANY_REQUESTS
    );
    db.query(
        "UPDATE agent_pending_actions SET expires_at = time::now() - 1s WHERE record::id(id) = $id",
    )
    .bind(json!({"id": more[1]}))
    .await?
    .check()?;
    assert_eq!(
        status(app.clone().oneshot(propose("after expiry")).await.unwrap()),
        StatusCode::OK
    );
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
    let entry = |agent: Option<&str>, latency_ms, fallback_count| jarvis_usage::UsageEntry {
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
        status: "succeeded".to_owned(),
        failure_category: None,
        fallback_count,
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_eur: 0.01,
    };
    // The same constructor Core's metering uses for a call without a reply.
    let failed = jarvis_usage::UsageEntry::failed(
        "ollama-cloud",
        "fixture-model",
        "timeout",
        jarvis_usage::UsageMetadata {
            agent_id: Some("researcher".to_owned()),
            latency_ms: 300,
            ..Default::default()
        },
    );
    for row in [
        entry(Some("researcher"), 100, 2),
        failed,
        // Unmeasured internal call: counted, but excluded from latency.
        entry(None, 0, 0),
    ] {
        jarvis_usage::record(&db, &row).await?;
    }

    // A failed call is not billed, so it never consumes the monthly cap.
    let month_total = jarvis_usage::month_total_eur(&db).await?;
    assert!((month_total - 0.02).abs() < 1e-9, "{month_total}");
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
    assert_eq!((agent.totals.requests, agent.totals.total_tokens), (2, 15));
    assert!((agent.totals.cost_eur - 0.01).abs() < 1e-9);
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

const MEME_ROUTES: [(&str, &str); 4] = [
    ("GET", "/v1/trading/meme/overview"),
    ("GET", "/v1/trading/meme/candidates?limit=5"),
    ("GET", "/v1/trading/meme/trades"),
    ("POST", "/v1/trading/meme/pause"),
];

#[tokio::test]
async fn memecoin_paper_routes_refuse_missing_or_invalid_sessions() {
    // An unconnected database cannot authenticate any token.
    let fixture = state(jarvis_store::Database::init(), None).await;
    let handle = fixture.memecoin.clone();
    let app = build_router(fixture);
    for (method, path) in MEME_ROUTES {
        for authorization in [None, Some("Bearer not-a-session")] {
            let mut request = Request::builder().method(method).uri(path);
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
                "{method} {path}"
            );
        }
    }
    // Disabled by default, and an unauthenticated pause changes nothing.
    assert!(!handle.enabled() && !handle.running() && !handle.paused());
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn memecoin_paper_routes_are_read_only_except_pause() -> Result<(), Box<dyn std::error::Error>>
{
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
    let audit_db = db.clone();
    let app = build_router(state(db, None).await);
    let (token, _) = enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let call = |method: &'static str, path: &'static str| {
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
    };

    let overview = json_body(call("GET", "/v1/trading/meme/overview").await?).await;
    assert_eq!(overview["mode"], "paper");
    assert_eq!(overview["enabled"], false);
    assert_eq!(overview["paused"], false);
    assert!(overview.get("equity_usd").is_none());
    let candidates = json_body(call("GET", "/v1/trading/meme/candidates").await?).await;
    assert_eq!(candidates["candidates"], json!([]));
    let trades = json_body(call("GET", "/v1/trading/meme/trades?limit=100").await?).await;
    assert_eq!(trades["trades"], json!([]));
    for bad in [
        "/v1/trading/meme/trades?limit=0",
        "/v1/trading/meme/candidates?limit=101",
        "/v1/trading/meme/candidates?limit=x",
    ] {
        assert_eq!(
            call("GET", bad).await?.status(),
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // There is no way to resume over HTTP.
    for path in ["/v1/trading/meme/resume", "/v1/trading/meme/unpause"] {
        assert_eq!(call("POST", path).await?.status(), StatusCode::NOT_FOUND);
    }
    for _ in 0..2 {
        let paused = call("POST", "/v1/trading/meme/pause").await?;
        assert_eq!(paused.status(), StatusCode::OK);
    }
    let overview = json_body(call("GET", "/v1/trading/meme/overview").await?).await;
    assert_eq!(overview["paused"], true);
    assert!(overview["paused_at"].is_string());
    // Each pause is in the security audit, with the device that made it.
    let audited: Vec<Option<String>> = audit_db
        .query("SELECT VALUE device_id FROM security_audit WHERE event = 'memecoin.paper.pause' AND outcome = 'paused'")
        .await?
        .check()?
        .take(0)?;
    assert_eq!(audited.len(), 2);
    assert!(audited.iter().all(Option::is_some));
    Ok(())
}

const TRADING_ROUTES: [&str; 3] = [
    "/v1/trading/portfolio",
    "/v1/trading/portfolio/history?range=1M",
    "/v1/trading/instruments/12345/bars?range=1D",
];

#[tokio::test]
async fn trading_routes_refuse_missing_or_invalid_sessions() {
    let app = build_router(state(jarvis_store::Database::init(), None).await);
    for path in TRADING_ROUTES {
        for authorization in [None, Some("Bearer not-a-session")] {
            let mut request = Request::builder().uri(path);
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }
}

/// A fake IBKR broker on a temporary socket: one canned reply per request
/// kind, like `jarvis-ibkr-broker` (JSON line in, JSON line out).
async fn fake_ibkr_broker(dir: &std::path::Path) -> jarvis_ibkr::broker::BrokerClient {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let socket = dir.join("broker.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                let _ = BufReader::new(read).read_line(&mut line).await;
                let request: Value = serde_json::from_str(&line).unwrap_or_default();
                let reply = match request["kind"].as_str() {
                    Some("accounts") => r#"{"ok":[{"accountId":"U1234567","currency":"EUR"}]}"#,
                    Some("positions") => {
                        r#"{"ok":[{"conid":12345,"contractDesc":"VANGUARD FTSE ALL-WORLD","ticker":"VWCE","assetClass":"STK","currency":"EUR","position":"10","mktPrice":"120.5","mktValue":"1205","avgPrice":"101.25"}]}"#
                    }
                    Some("ledger") => {
                        r#"{"ok":{"BASE":{"netliquidationvalue":"2205.5","cashbalance":"1000.5"}}}"#
                    }
                    Some("history") if request["period"] == "1d" && request["bar"] == "5min" => {
                        r#"{"ok":{"data":[{"t":1730419200000,"o":"120","h":"121.5","l":"119","c":"121","v":"1500"}]}}"#
                    }
                    _ => r#"{"error":{"code":"invalid_request"}}"#,
                };
                let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    // SAFETY: geteuid has no preconditions.
    jarvis_ibkr::broker::BrokerClient::new(socket, unsafe { libc::geteuid() })
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn trading_routes_serve_the_synced_portfolio_read_only(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("jarvis_api_{}", uuid::Uuid::now_v7().simple()))
        .use_db("core")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let dir = tempfile::tempdir()?;
    let client = fake_ibkr_broker(dir.path()).await;

    // Off by default: every route answers "not configured".
    let off = build_router(state(db.clone(), None).await);
    let (token, _) = enroll_login(&off, &SigningKey::from_bytes(&rand::random())).await;
    let call = |app: &axum::Router, method: &str, path: &str| {
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
    };
    for path in TRADING_ROUTES {
        let response = call(&off, "GET", path).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(
            json_body(response).await["error"],
            "IBKR portfolio sync is not configured"
        );
    }

    let mut on_state = state(db.clone(), None).await;
    on_state.ibkr = Some(Arc::new(client.clone()));
    let on = build_router(on_state);
    // Enabled, before the first sync: no data yet, still "not connected".
    for path in &TRADING_ROUTES[..2] {
        let response = call(&on, "GET", path).await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }

    let portfolio = jarvis_api::ibkr::fetch(&client).await?;
    jarvis_api::ibkr::store::write_snapshot(&db, &portfolio).await?;

    let body = json_body(call(&on, "GET", "/v1/trading/portfolio").await?).await;
    assert_eq!(body["source"], "IBKR");
    assert!(body["as_of"].is_string() && body.get("delay_minutes").is_none());
    assert_eq!(body["base_currency"], "EUR");
    assert_eq!(body["net_liquidation"], "2205.5");
    assert_eq!(body["cash"], "1000.5");
    assert_eq!(body["positions"][0]["conid"], 12345);
    assert_eq!(body["positions"][0]["symbol"], "VWCE");
    assert_eq!(body["positions"][0]["market_value"], "1205");
    assert_eq!(body["positions"][0]["weight_pct"], "54.64");

    let body = json_body(call(&on, "GET", "/v1/trading/portfolio/history?range=ALL").await?).await;
    assert_eq!(body["range"], "ALL");
    assert_eq!(body["points"].as_array().map(Vec::len), Some(1));
    assert_eq!(body["points"][0]["value"], "2205.5");

    let body = json_body(call(&on, "GET", TRADING_ROUTES[2]).await?).await;
    assert_eq!(body["conid"], 12345);
    assert_eq!(body["symbol"], "VWCE");
    assert_eq!(body["currency"], "EUR");
    assert_eq!(body["range"], "1D");
    assert_eq!(body["bars"][0]["c"], "121");

    for (path, status) in [
        ("/v1/trading/portfolio/history", StatusCode::BAD_REQUEST),
        (
            "/v1/trading/portfolio/history?range=2Y",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/trading/instruments/0/bars?range=1D",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/v1/trading/instruments/12345/bars?range=10Y",
            StatusCode::BAD_REQUEST,
        ),
        // Only held instruments: Core is no general market data proxy.
        (
            "/v1/trading/instruments/999/bars?range=1D",
            StatusCode::NOT_FOUND,
        ),
        // The fake broker refuses this period: the upstream error is a 503.
        (
            "/v1/trading/instruments/12345/bars?range=1W",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        assert_eq!(call(&on, "GET", path).await?.status(), status, "{path}");
    }
    // Read-only: no write method on any trading route, and the legacy
    // gateway routes are gone.
    for path in TRADING_ROUTES {
        for method in ["POST", "PUT", "DELETE"] {
            assert_eq!(
                call(&on, method, path).await?.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
        }
    }
    for path in ["/v1/broker/ibkr/status", "/v1/broker/ibkr/positions"] {
        assert_eq!(
            call(&on, "GET", path).await?.status(),
            StatusCode::NOT_FOUND
        );
    }
    Ok(())
}
