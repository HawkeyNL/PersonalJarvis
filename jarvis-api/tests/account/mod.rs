use super::*;
use jarvis_client_core::account::{account_approval_message, AccountApproval};
use sha2::{Digest, Sha256};

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn first_activation_requires_local_network_code_and_password_atomically(
) -> Result<(), Box<dyn std::error::Error>> {
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
async fn pairing_is_refused_while_the_owner_has_no_password(
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
    // A dev-enrolled owner has an active device but no stored password.
    enroll_login(&app, &SigningKey::from_bytes(&rand::random())).await;
    let owner = jarvis_identity::first_user(&db).await?.ok_or("no owner")?;
    assert!(!jarvis_identity::surreal::account::password_required(&db, owner.id).await?);
    let candidate = SigningKey::from_bytes(&rand::random());
    for password in [None, Some("any disposable fixture password")] {
        let mut body = json!({"name":"candidate", "platform":"android",
            "public_key":hex::encode(candidate.verifying_key().to_bytes())});
        if let Some(password) = password {
            body["password"] = json!(password);
        }
        let refused = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/pairing/requests")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&body)?))?,
            )
            .await?;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert_eq!(json_body(refused).await["code"], "pairing_password_not_set");
    }
    assert!(jarvis_identity::pending_pairing_requests(&db, owner.id)
        .await?
        .is_empty());
    assert_no_push(&mut publishes).await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn pairing_records_validated_candidate_details_for_the_approver(
) -> Result<(), Box<dyn std::error::Error>> {
    use jarvis_identity::password::{AccountPassword, PasswordService};
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("pairing_details_{}", uuid::Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let mut fixture = state(db.clone(), None).await;
    let (notifier, mut publishes) = ntfy_mock().await?;
    fixture.notifier = notifier;
    let app = build_router(fixture);
    let password = "disposable pairing details fixture password";
    let owner_key = SigningKey::from_bytes(&rand::random());
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64
        + 600;
    let (owner, device) = jarvis_identity::surreal::account::bootstrap_account(
        &db,
        "owner",
        jarvis_identity::Platform::Ios,
        &owner_key.verifying_key().to_bytes(),
        PasswordService::shared()
            .hash(AccountPassword::new(password.into())?)
            .await?,
        expires,
    )
    .await?;
    let challenge = json_body(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/challenge")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(
                        &json!({"device_id": device.id}),
                    )?))?,
            )
            .await?,
    )
    .await;
    let nonce = hex::decode(challenge["nonce"].as_str().ok_or("no nonce")?)?;
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(
                    &json!({"device_id": device.id,
                    "challenge_id": challenge["challenge_id"], "password": password,
                    "signature": hex::encode(owner_key.sign(&nonce).to_bytes())}),
                )?))?,
        )
        .await?;
    assert_eq!(login.status(), StatusCode::OK);
    let token = json_body(login).await["token"]
        .as_str()
        .ok_or("no token")?
        .to_string();

    let candidate = SigningKey::from_bytes(&rand::random());
    let base = json!({"name":"candidate", "platform":"android", "password": password,
        "public_key":hex::encode(candidate.verifying_key().to_bytes())});
    // Each request comes from its own address so the per-source limit of the
    // pairing route does not mask the handler's answer.
    let pairing = |body: &Value, peer: &str, user_agent: Option<&[u8]>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/auth/pairing/requests")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(axum::extract::ConnectInfo(jarvis_api::Peer::Tcp(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            )));
        if let Some(user_agent) = user_agent {
            builder = builder.header(
                header::USER_AGENT,
                header::HeaderValue::from_bytes(user_agent).unwrap(),
            );
        }
        builder
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    };
    let invalid = [
        ("app_version", json!("v0.1.14"), StatusCode::BAD_REQUEST),
        (
            "app_version",
            json!("1".repeat(33)),
            StatusCode::BAD_REQUEST,
        ),
        ("app_version", json!(""), StatusCode::BAD_REQUEST),
        ("model", json!("Pixel\u{202E}9"), StatusCode::BAD_REQUEST),
        ("model", json!("x".repeat(65)), StatusCode::BAD_REQUEST),
        ("model", json!(" "), StatusCode::BAD_REQUEST),
        ("model", json!("Pixel 9 Pr\u{f6}"), StatusCode::BAD_REQUEST),
        ("name", json!("Gus\u{202E}enohp"), StatusCode::BAD_REQUEST),
        ("ip", json!("203.0.113.9"), StatusCode::UNPROCESSABLE_ENTITY),
    ];
    for (index, (field, value, expected)) in invalid.into_iter().enumerate() {
        let mut body = base.clone();
        body[field] = value;
        let peer = format!("198.51.100.{}:4000", index + 1);
        let status = app
            .clone()
            .oneshot(pairing(&body, &peer, None))
            .await?
            .status();
        assert_eq!(status, expected, "{field}");
    }
    assert!(jarvis_identity::pending_pairing_requests(&db, owner.id)
        .await?
        .is_empty());
    assert_no_push(&mut publishes).await;

    let mut body = base.clone();
    body["app_version"] = json!("0.1.14+build.7");
    body["model"] = json!("Pixel 9 Pro");
    let created = app
        .clone()
        .oneshot(pairing(
            &body,
            "[::ffff:192.0.2.44]:4000",
            Some(b"JarvisApp/0.1.14 (Android 15)"),
        ))
        .await?;
    assert_eq!(created.status(), StatusCode::OK);
    tokio::time::timeout(std::time::Duration::from_secs(5), publishes.recv())
        .await?
        .ok_or("no notification")?;
    // A client without the optional details still pairs; a non-ASCII
    // User-Agent is dropped rather than stored.
    let other = SigningKey::from_bytes(&rand::random());
    let mut plain = base.clone();
    plain["public_key"] = json!(hex::encode(other.verifying_key().to_bytes()));
    // Visible Unicode, as in a default iOS device name, stays allowed.
    plain["name"] = json!("Gus\u{2019}s iPhone");
    let created = app
        .clone()
        .oneshot(pairing(
            &plain,
            "[2001:db8::7]:4000",
            Some("Jarvis caf\u{e9}".as_bytes()),
        ))
        .await?;
    assert_eq!(created.status(), StatusCode::OK);

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/auth/pairing/requests")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = json_body(listed).await;
    let requests = listed["requests"].as_array().ok_or("no requests")?;
    assert_eq!(requests.len(), 2);
    let full = hex::encode(sha2::Sha256::digest(candidate.verifying_key().to_bytes()));
    let detailed = requests
        .iter()
        .find(|item| item["fingerprint_full"] == full.as_str())
        .ok_or("detailed request missing")?;
    assert_eq!(detailed["fingerprint"], &full[..16]);
    assert_eq!(detailed["ip"], "192.0.2.44");
    assert_eq!(detailed["user_agent"], "JarvisApp/0.1.14 (Android 15)");
    assert_eq!(detailed["app_version"], "0.1.14+build.7");
    assert_eq!(detailed["model"], "Pixel 9 Pro");
    let plain = requests
        .iter()
        .find(|item| item["fingerprint_full"] != full.as_str())
        .ok_or("plain request missing")?;
    assert_eq!(plain["ip"], "2001:db8::7");
    assert_eq!(plain["device_name"], "Gus\u{2019}s iPhone");
    for field in ["user_agent", "app_version", "model"] {
        assert!(plain[field].is_null(), "{field}");
    }
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
