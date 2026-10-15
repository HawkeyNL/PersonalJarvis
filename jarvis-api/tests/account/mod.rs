use super::*;
use jarvis_client_core::account::{account_approval_message, AccountApproval};
use sha2::{Digest, Sha256};

/// The Argon2 admission pools (`PasswordService::shared` and `pre_auth`) are
/// process-wide by design and refuse work instead of queueing it. Tests in
/// this binary run in parallel, so every test that hashes or verifies a
/// password holds this lock; otherwise they starve each other into 429/503.
async fn argon2_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(Default::default).lock().await
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn first_activation_requires_local_network_code_and_password_atomically(
) -> Result<(), Box<dyn std::error::Error>> {
    let _argon2 = argon2_guard().await;
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("bootstrap_http_{}", uuid::Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let mut fixture = state(db.clone(), None).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let code = "ab".repeat(32);
    fixture.bootstrap_enrollment = Some(
        jarvis_config::activation::ActivationPolicy {
            schema_version: 1,
            secret_sha256: hex::encode(Sha256::digest(code.as_bytes())),
            allowed_cidrs: vec!["10.23.45.0/24".into()],
            issued_at: now,
            expires_at: now + 600,
        }
        .validate(now)?,
    );
    let (notifier, mut publishes) = ntfy_mock().await?;
    fixture.notifier = notifier;
    let app = build_router(fixture);
    let key = SigningKey::from_bytes(&rand::random());
    let body = json!({"name":"fixture", "platform":"ios", "public_key":hex::encode(key.verifying_key().to_bytes()),
        "password":"disposable activation fixture password"});
    let activate = |peer: &str, supplied: &str| {
        Request::builder()
            .method("POST")
            .uri("/v1/auth/bootstrap")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-jarvis-bootstrap-secret", supplied)
            // A forged proxy header must not make a public peer local.
            .header("x-forwarded-for", "10.23.45.10")
            .extension(axum::extract::ConnectInfo(jarvis_api::Peer::Tcp(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            )))
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(activate("192.0.2.10:32000", &code))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(activate("10.23.45.10:32000", "incorrect"))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 0);
    let accepted = app
        .clone()
        .oneshot(activate("10.23.45.10:32000", &code))
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 1);
    let owner = jarvis_identity::first_user(&db).await?.unwrap();
    assert!(jarvis_identity::surreal::account::password_required(&db, owner.id).await?);
    assert_eq!(
        app.clone()
            .oneshot(activate("10.23.45.10:32000", &code))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 1);
    // Losing the last device must not reopen bootstrap, but a password-gated
    // pending request must remain possible for explicit local owner approval.
    let devices = jarvis_identity::list_active_devices(&db, owner.id).await?;
    jarvis_identity::revoke_device(&db, devices[0].id).await?;
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 0);
    assert!(!jarvis_identity::surreal::account::bootstrap_available(&db).await?);
    assert_eq!(
        app.clone()
            .oneshot(activate("10.23.45.11:32000", &code))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let replacement = SigningKey::from_bytes(&rand::random());
    let pairing = |password: Option<&str>| {
        let mut body = json!({"name":"replacement", "platform":"macos",
            "public_key":hex::encode(replacement.verifying_key().to_bytes())});
        if let Some(password) = password {
            body["password"] = json!(password);
        }
        Request::builder()
            .method("POST")
            .uri("/v1/auth/pairing/requests")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    };
    for password in [None, Some("incorrect fixture password")] {
        assert_eq!(
            app.clone().oneshot(pairing(password)).await?.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert!(jarvis_identity::pending_pairing_requests(&db, owner.id)
        .await?
        .is_empty());
    assert_eq!(
        app.clone()
            .oneshot(pairing(Some("disposable activation fixture password")))
            .await?
            .status(),
        StatusCode::OK
    );
    let pending = jarvis_identity::pending_pairing_requests(&db, owner.id).await?;
    assert_eq!(pending.len(), 1);
    // Only the persisted request is announced, once, without its details.
    let push = tokio::time::timeout(std::time::Duration::from_secs(5), publishes.recv())
        .await?
        .ok_or("no notification")?;
    assert!(push.contains("click: jarvis://approvals/pairing\r\n"));
    assert!(
        !push.contains("replacement") && !push.contains(&pending[0].candidate_fingerprint[..16])
    );
    assert_no_push(&mut publishes).await;
    assert_eq!(pending[0].status, "pending");
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 0);
    assert!(!jarvis_identity::surreal::account::bootstrap_available(&db).await?);
    Ok(())
}

/// Loopback ntfy stand-in: a notifier publishing to it, and every request
/// it receives (read up to its content-length).
async fn ntfy_mock() -> Result<
    (
        jarvis_api::notify::Notifier,
        tokio::sync::mpsc::UnboundedReceiver<String>,
    ),
    Box<dyn std::error::Error>,
> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ntfy = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let notifier = jarvis_api::notify::Notifier::start(
        &format!("http://{}/jarvis-test", ntfy.local_addr()?),
        "tk_fixture",
    )?;
    let (published, publishes) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = ntfy.accept().await {
            let mut raw = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap_or(0);
                raw.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&raw);
                let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    body.len() >= length
                });
                if complete || read == 0 {
                    break;
                }
            }
            let _ = published.send(String::from_utf8_lossy(&raw).to_string());
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    Ok((notifier, publishes))
}

async fn assert_no_push(publishes: &mut tokio::sync::mpsc::UnboundedReceiver<String>) {
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), publishes.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn pairing_without_a_verified_password_sends_no_push(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("pairing_push_{}", uuid::Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let mut fixture = state(db.clone(), None).await;
    let (notifier, mut publishes) = ntfy_mock().await?;
    fixture.notifier = notifier;
    let app = build_router(fixture);
    // A dev-enrolled owner has an active device and no stored password, so a
    // pairing request is accepted without any password check.
    enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let owner = jarvis_identity::first_user(&db).await?.ok_or("no owner")?;
    assert!(!jarvis_identity::surreal::account::password_required(&db, owner.id).await?);
    let candidate = SigningKey::from_bytes(&rand::random());
    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/pairing/requests")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&json!({"name":"candidate",
                    "platform":"android",
                    "public_key":hex::encode(candidate.verifying_key().to_bytes())}))?))?,
        )
        .await?;
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(
        jarvis_identity::pending_pairing_requests(&db, owner.id)
            .await?
            .len(),
        1
    );
    assert_no_push(&mut publishes).await;
    Ok(())
}

async fn request(
    app: &axum::Router,
    path: &str,
    token: &str,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn password_activation_requires_signed_approval_and_invalidates_live_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let _argon2 = argon2_guard().await;
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("account_http_{}", uuid::Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let fixture = state(db.clone(), None).await;
    let hub = fixture.realtime.clone();
    let app = build_router(fixture);
    let key = SigningKey::from_bytes(&rand::random());
    let (token, _) = enroll_login(&app, &key).await;
    let auth = jarvis_identity::authenticate(&db, &token).await?;
    let subscription = hub.subscribe(auth.user.id, auth.device.id).unwrap();
    let password = "only a disposable HTTP fixture password";
    let prepared = request(
        &app,
        "/v1/auth/account/password/requests",
        &token,
        json!({"password":password}),
    )
    .await;
    assert_eq!(prepared.status(), StatusCode::OK);
    let body = json_body(prepared).await;
    assert!(!body.to_string().contains(password));
    assert!(!body.to_string().contains("argon2"));
    let approval: AccountApproval = serde_json::from_value(body)?;
    let path = format!("/v1/auth/account/requests/{}/approve", approval.request_id);
    let refused = request(&app, &path, &token, json!({"signature":"00".repeat(64)})).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    assert!(!jarvis_identity::surreal::account::password_required(&db, auth.user.id).await?);
    assert!(!subscription.receiver.is_closed());
    let signature = hex::encode(key.sign(&account_approval_message(&approval)?).to_bytes());
    let accepted = request(&app, &path, &token, json!({"signature":signature})).await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert!(subscription.receiver.is_closed());
    assert!(jarvis_identity::authenticate(&db, &token).await.is_err());
    assert!(jarvis_identity::surreal::account::password_required(&db, auth.user.id).await?);
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn password_reset_waits_for_local_confirmation_after_signing(
) -> Result<(), Box<dyn std::error::Error>> {
    let _argon2 = argon2_guard().await;
    use jarvis_client_core::account::AccountAction;
    use jarvis_identity::{
        password::{AccountPassword, PasswordService},
        surreal::account,
    };
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("recovery_http_{}", uuid::Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let app = build_router(state(db.clone(), None).await);
    let key = SigningKey::from_bytes(&rand::random());
    let (token, _) = enroll_login(&app, &key).await;
    let auth = jarvis_identity::authenticate(&db, &token).await?;
    let path = "/v1/auth/account/password/reset-requests";
    let new_password = "a disposable recovered HTTP fixture password";
    // Without a password there is nothing to recover; use password-set.
    let refused = request(&app, path, &token, json!({"password":new_password})).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);

    let hash = |password: &str| {
        PasswordService::shared().hash(AccountPassword::new(password.to_owned()).unwrap())
    };
    let set = account::request_action(
        &db,
        auth.user.id,
        auth.device.id,
        AccountAction::PasswordSet,
        auth.user.id,
        Some(hash("a disposable forgotten HTTP fixture password").await?),
    )
    .await?;
    let signature = key.sign(&account_approval_message(&set)?).to_bytes();
    account::approve_action(
        &db,
        set.request_id,
        auth.user.id,
        auth.device.id,
        &signature,
    )
    .await?;
    let challenge = jarvis_identity::create_challenge(&db, auth.device.id).await?;
    let proof = jarvis_client_core::login_message(challenge.id, auth.device.id, &challenge.nonce)?;
    let token = jarvis_identity::surreal::login_with_password(
        &db,
        auth.device.id,
        challenge.id,
        &key.sign(&proof).to_bytes(),
        Some(AccountPassword::new(
            "a disposable forgotten HTTP fixture password".to_owned(),
        )?),
    )
    .await?
    .token;

    // No current password; unknown fields are refused.
    let extra = request(
        &app,
        path,
        &token,
        json!({"password":new_password, "current_password":"x"}),
    )
    .await;
    assert!(extra.status().is_client_error());
    let prepared = request(&app, path, &token, json!({"password":new_password})).await;
    assert_eq!(prepared.status(), StatusCode::OK);
    let body = json_body(prepared).await;
    assert!(!body.to_string().contains(new_password));
    assert!(!body.to_string().contains("argon2"));
    let approval: AccountApproval = serde_json::from_value(body)?;
    assert_eq!(approval.action, AccountAction::PasswordReset);
    let approve = format!("/v1/auth/account/requests/{}/approve", approval.request_id);
    let signature = hex::encode(key.sign(&account_approval_message(&approval)?).to_bytes());
    let accepted = request(&app, &approve, &token, json!({"signature":signature})).await;
    assert_eq!(accepted.status(), StatusCode::OK);
    let body = json_body(accepted).await;
    assert_eq!(body["status"], "pending_confirmation");
    assert_eq!(body["request_id"], approval.request_id.to_string());
    // Nothing changes until local root confirms on the Home Node.
    assert!(jarvis_identity::authenticate(&db, &token).await.is_ok());
    let again = request(&app, path, &token, json!({"password":new_password})).await;
    assert_eq!(again.status(), StatusCode::TOO_MANY_REQUESTS);
    Ok(())
}
