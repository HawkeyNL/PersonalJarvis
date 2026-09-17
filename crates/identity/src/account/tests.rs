use super::*;
use ed25519_dalek::{Signer, SigningKey};
use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

const PASSWORD: &str = "fixture account password never used in production";

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn first_account_is_atomic_expiring_and_cannot_be_reopened(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("activation_fixture_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    assert!(bootstrap_available(&db).await?);
    let key = SigningKey::from_bytes(&rand::random());
    let stored = PasswordService::shared()
        .hash(AccountPassword::new(PASSWORD.to_owned())?)
        .await?;
    assert!(bootstrap_account(
        &db,
        "fixture",
        Platform::Ios,
        &key.verifying_key().to_bytes(),
        stored,
        1
    )
    .await
    .is_err());
    assert!(first_user(&db).await?.is_none());
    assert_eq!(active_device_count(&db).await?, 0);
    let stored = PasswordService::shared()
        .hash(AccountPassword::new(PASSWORD.to_owned())?)
        .await?;
    let deadline = OffsetDateTime::now_utc().unix_timestamp() + 600;
    let (owner, device) = bootstrap_account(
        &db,
        "fixture",
        Platform::Ios,
        &key.verifying_key().to_bytes(),
        stored,
        deadline,
    )
    .await?;
    assert!(password_required(&db, owner.id).await?);
    assert!(!bootstrap_available(&db).await?);
    let challenge = create_challenge(&db, device.id).await?;
    assert!(login(
        &db,
        device.id,
        challenge.id,
        &key.sign(&challenge.nonce).to_bytes()
    )
    .await
    .is_err());
    login_with_password(
        &db,
        device.id,
        challenge.id,
        &key.sign(&challenge.nonce).to_bytes(),
        Some(AccountPassword::new(PASSWORD.to_owned())?),
    )
    .await?;
    revoke_device(&db, device.id).await?;
    let stored = PasswordService::shared()
        .hash(AccountPassword::new(PASSWORD.to_owned())?)
        .await?;
    assert!(bootstrap_account(
        &db,
        "fixture",
        Platform::Ios,
        &key.verifying_key().to_bytes(),
        stored,
        deadline
    )
    .await
    .is_err());
    assert_eq!(active_device_count(&db).await?, 0);
    assert!(!bootstrap_available(&db).await?);
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn signed_account_changes_enforce_password_and_revoke_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("account_fixture_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let owner = create_user(&db, "fixture owner").await?;
    let key = SigningKey::from_bytes(&rand::random());
    let (device, _) = register_device(
        &db,
        owner.id,
        "fixture device",
        Platform::Linux,
        "ed25519",
        &key.verifying_key().to_bytes(),
    )
    .await?;
    let challenge = create_challenge(&db, device.id).await?;
    let session = login(
        &db,
        device.id,
        challenge.id,
        &key.sign(&challenge.nonce).to_bytes(),
    )
    .await?;
    let stored = PasswordService::shared()
        .hash(AccountPassword::new(PASSWORD.to_owned())?)
        .await?;
    let request = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::PasswordSet,
        owner.id,
        Some(stored),
    )
    .await?;
    assert!(!password_required(&db, owner.id).await?);
    assert!(
        approve_action(&db, request.request_id, owner.id, device.id, &[0; 64])
            .await
            .is_err()
    );
    let signature = key.sign(&account_approval_message(&request)?).to_bytes();
    approve_action(&db, request.request_id, owner.id, device.id, &signature).await?;
    assert!(password_required(&db, owner.id).await?);
    assert!(authenticate(&db, &session.token).await.is_err());
    assert!(
        approve_action(&db, request.request_id, owner.id, device.id, &signature)
            .await
            .is_err()
    );
    let challenge = create_challenge(&db, device.id).await?;
    let signed = key.sign(&challenge.nonce).to_bytes();
    assert!(login(&db, device.id, challenge.id, &signed).await.is_err());
    assert!(login_with_password(
        &db,
        device.id,
        challenge.id,
        &signed,
        Some(AccountPassword::new(
            "incorrect fixture password".to_owned()
        )?)
    )
    .await
    .is_err());
    let session = login_with_password(
        &db,
        device.id,
        challenge.id,
        &signed,
        Some(AccountPassword::new(PASSWORD.to_owned())?),
    )
    .await?;
    assert!(authenticate(&db, &session.token).await.is_ok());
    let request = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::DeviceRevoke,
        device.id,
        None,
    )
    .await?;
    let signature = key.sign(&account_approval_message(&request)?).to_bytes();
    let other = create_user(&db, "other fixture owner").await?;
    assert!(
        approve_action(&db, request.request_id, other.id, device.id, &signature)
            .await
            .is_err()
    );
    approve_action(&db, request.request_id, owner.id, device.id, &signature).await?;
    assert!(authenticate(&db, &session.token).await.is_err());
    assert_eq!(get_device(&db, device.id).await?.unwrap().status, "revoked");
    Ok(())
}
