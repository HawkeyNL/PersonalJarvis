use super::*;
use jarvis_client_core::account::{account_approval_message, AccountApproval};

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
