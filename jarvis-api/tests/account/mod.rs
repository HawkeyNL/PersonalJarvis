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
            .extension(axum::extract::ConnectInfo(
                peer.parse::<std::net::SocketAddr>().unwrap(),
            ))
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
    assert_eq!(pending[0].status, "pending");
    assert_eq!(jarvis_identity::active_device_count(&db).await?, 0);
    assert!(!jarvis_identity::surreal::account::bootstrap_available(&db).await?);
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
