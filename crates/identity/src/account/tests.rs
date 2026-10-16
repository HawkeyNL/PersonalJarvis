use super::*;
use ed25519_dalek::{Signer, SigningKey};
use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

const PASSWORD: &str = "fixture account password never used in production";

fn login_proof(key: &SigningKey, device_id: Uuid, challenge: &crate::Challenge) -> [u8; 64] {
    let message = jarvis_client_core::login_message(challenge.id, device_id, &challenge.nonce)
        .expect("fixture challenge nonce is valid");
    key.sign(&message).to_bytes()
}

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
        &login_proof(&key, device.id, &challenge)
    )
    .await
    .is_err());
    // A failed attempt with a valid signature burns the challenge.
    assert!(login_with_password(
        &db,
        device.id,
        challenge.id,
        &login_proof(&key, device.id, &challenge),
        Some(AccountPassword::new(PASSWORD.to_owned())?),
    )
    .await
    .is_err());
    let challenge = create_challenge(&db, device.id).await?;
    login_with_password(
        &db,
        device.id,
        challenge.id,
        &login_proof(&key, device.id, &challenge),
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
        Platform::Android,
        "ed25519",
        &key.verifying_key().to_bytes(),
    )
    .await?;
    let challenge = create_challenge(&db, device.id).await?;
    let session = login(
        &db,
        device.id,
        challenge.id,
        &login_proof(&key, device.id, &challenge),
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
    let signed = login_proof(&key, device.id, &challenge);
    assert!(login(&db, device.id, challenge.id, &signed).await.is_err());
    let challenge = create_challenge(&db, device.id).await?;
    let signed = login_proof(&key, device.id, &challenge);
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
    // One guess per signed challenge: the right password cannot reuse it.
    assert!(login_with_password(
        &db,
        device.id,
        challenge.id,
        &signed,
        Some(AccountPassword::new(PASSWORD.to_owned())?),
    )
    .await
    .is_err());
    let challenge = create_challenge(&db, device.id).await?;
    let signed = login_proof(&key, device.id, &challenge);
    let session = login_with_password(
        &db,
        device.id,
        challenge.id,
        &signed,
        Some(AccountPassword::new(PASSWORD.to_owned())?),
    )
    .await?;
    assert!(authenticate(&db, &session.token).await.is_ok());
    assert!(matches!(
        request_action(
            &db,
            owner.id,
            device.id,
            AccountAction::DeviceRevoke,
            device.id,
            None,
        )
        .await,
        Err(IdentityError::LastApprover)
    ));
    let other_key = SigningKey::from_bytes(&rand::random());
    register_device(
        &db,
        owner.id,
        "second approver",
        Platform::Ios,
        "ed25519",
        &other_key.verifying_key().to_bytes(),
    )
    .await?;
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

/// Bootstrap stores the first verifier under a generated record id, so a later
/// signed password change must replace that row instead of adding a second one
/// for the same owner (the owner index is UNIQUE).
#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn password_change_after_bootstrap_replaces_the_bootstrap_verifier(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!(
        "password_change_fixture_{}",
        Uuid::now_v7().simple()
    ))
    .use_db("fixture")
    .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let key = SigningKey::from_bytes(&rand::random());
    let hash = |password: &str| {
        PasswordService::shared().hash(AccountPassword::new(password.to_owned()).unwrap())
    };
    let deadline = OffsetDateTime::now_utc().unix_timestamp() + 600;
    let (owner, device) = bootstrap_account(
        &db,
        "fixture",
        Platform::Android,
        &key.verifying_key().to_bytes(),
        hash(PASSWORD).await?,
        deadline,
    )
    .await?;
    let changed = "changed fixture account password never used in production";
    let request = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::PasswordSet,
        owner.id,
        Some(hash(changed).await?),
    )
    .await?;
    let signature = key.sign(&account_approval_message(&request)?).to_bytes();
    approve_action(&db, request.request_id, owner.id, device.id, &signature).await?;

    let rows: Option<i64> = db
        .query("RETURN array::len(SELECT id FROM account_passwords WHERE user_id = $user)")
        .bind(("user", owner.id.to_string()))
        .await?
        .take(0)?;
    assert_eq!(rows, Some(1));
    let login_as = |password: &str| {
        let db = &db;
        let key = &key;
        let password = password.to_owned();
        async move {
            let challenge = create_challenge(db, device.id).await?;
            let signed = login_proof(key, device.id, &challenge);
            login_with_password(
                db,
                device.id,
                challenge.id,
                &signed,
                Some(AccountPassword::new(password)?),
            )
            .await
            .map_err(Box::<dyn std::error::Error>::from)
        }
    };
    assert!(login_as(PASSWORD).await.is_err());
    login_as(changed).await?;
    Ok(())
}

async fn fixture_db(prefix: &str) -> Result<Database, Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("{prefix}_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    Ok(db)
}

async fn device(
    db: &Database,
    owner: Uuid,
    platform: Platform,
) -> Result<(Device, SigningKey), IdentityError> {
    let key = SigningKey::from_bytes(&rand::random());
    let (device, _) = register_device(
        db,
        owner,
        platform.as_str(),
        platform,
        "ed25519",
        &key.verifying_key().to_bytes(),
    )
    .await?;
    Ok((device, key))
}

async fn approve_signed(
    db: &Database,
    owner: Uuid,
    (signer, key): &(Device, SigningKey),
    request: &AccountApproval,
) -> Result<(), IdentityError> {
    let message = account_approval_message(request).map_err(|_| IdentityError::AuthFailed)?;
    let signature = key.sign(&message).to_bytes();
    approve_action(db, request.request_id, owner, signer.id, &signature)
        .await
        .map(drop)
}

async fn revoke_request(
    db: &Database,
    owner: Uuid,
    (signer, _): &(Device, SigningKey),
    target: Uuid,
) -> Result<AccountApproval, IdentityError> {
    request_action(
        db,
        owner,
        signer.id,
        AccountAction::DeviceRevoke,
        target,
        None,
    )
    .await
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn signed_revoke_keeps_the_last_approver_but_local_root_does_not(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = fixture_db("last_approver_fixture").await?;
    let owner = create_user(&db, "fixture owner").await?.id;
    let phone = device(&db, owner, Platform::Ios).await?;
    let desktop = device(&db, owner, Platform::Linux).await?;
    // A Linux desktop cannot sign approvals, so it does not count.
    assert!(matches!(
        revoke_request(&db, owner, &phone, phone.0.id).await,
        Err(IdentityError::LastApprover)
    ));
    // Non-approver devices are unaffected by the guard.
    let request = revoke_request(&db, owner, &phone, desktop.0.id).await?;
    approve_signed(&db, owner, &phone, &request).await?;
    assert_eq!(
        get_device(&db, desktop.0.id).await?.unwrap().status,
        "revoked"
    );

    // Checked again when the signed revoke executes.
    let laptop = device(&db, owner, Platform::Macos).await?;
    let request = revoke_request(&db, owner, &phone, phone.0.id).await?;
    revoke_device(&db, laptop.0.id).await?;
    assert!(matches!(
        approve_signed(&db, owner, &phone, &request).await,
        Err(IdentityError::LastApprover)
    ));
    assert_eq!(get_device(&db, phone.0.id).await?.unwrap().status, "active");

    // The local root path (`local_devices::revoke`) is the recovery route.
    revoke_device(&db, phone.0.id).await?;
    assert_eq!(active_device_count(&db).await?, 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn concurrent_revokes_of_the_last_two_approvers_leave_one(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = fixture_db("approver_race_fixture").await?;
    // Each approver revokes itself, so neither signer check depends on the
    // other revoke. Repeated so the two transactions overlap in some rounds.
    for _ in 0..10 {
        let owner = create_user(&db, "fixture owner").await?.id;
        let phone = device(&db, owner, Platform::Android).await?;
        let laptop = device(&db, owner, Platform::Macos).await?;
        let revoke_phone = revoke_request(&db, owner, &phone, phone.0.id).await?;
        let revoke_laptop = revoke_request(&db, owner, &laptop, laptop.0.id).await?;
        let (first, second) = tokio::join!(
            approve_signed(&db, owner, &phone, &revoke_phone),
            approve_signed(&db, owner, &laptop, &revoke_laptop),
        );
        let refused = [&first, &second]
            .iter()
            .filter(|result| matches!(result, Err(IdentityError::LastApprover)))
            .count();
        assert_eq!(
            (first.is_ok() as u8 + second.is_ok() as u8, refused),
            (1, 1),
            "{first:?} {second:?}"
        );
        assert_eq!(list_active_devices(&db, owner).await?.len(), 1);
    }
    Ok(())
}

/// Forgotten-password recovery needs an approver device signature and local
/// root confirmation; it is one-use, expiring and rate limited.
#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn password_recovery_needs_signature_and_root_confirmation(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("recovery_fixture_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let key = SigningKey::from_bytes(&rand::random());
    let hash = |password: &str| {
        PasswordService::shared().hash(AccountPassword::new(password.to_owned()).unwrap())
    };
    let deadline = OffsetDateTime::now_utc().unix_timestamp() + 600;
    let (owner, device) = bootstrap_account(
        &db,
        "fixture phone",
        Platform::Ios,
        &key.verifying_key().to_bytes(),
        hash(PASSWORD).await?,
        deadline,
    )
    .await?;
    let login_as = |password: &str| {
        let (db, key, password) = (&db, &key, password.to_owned());
        async move {
            let challenge = create_challenge(db, device.id).await?;
            let signed = login_proof(key, device.id, &challenge);
            login_with_password(
                db,
                device.id,
                challenge.id,
                &signed,
                Some(AccountPassword::new(password)?),
            )
            .await
            .map_err(Box::<dyn std::error::Error>::from)
        }
    };
    let session = login_as(PASSWORD).await?;
    let recovered = "recovered fixture account password never used in production";
    let sign = |request: &AccountApproval| key.sign(&account_approval_message(request).unwrap());

    // A desktop device cannot even prepare a reset.
    let desktop_key = SigningKey::from_bytes(&rand::random());
    let (desktop, _) = register_device(
        &db,
        owner.id,
        "fixture desktop",
        Platform::Linux,
        "ed25519",
        &desktop_key.verifying_key().to_bytes(),
    )
    .await?;
    assert!(request_action(
        &db,
        owner.id,
        desktop.id,
        AccountAction::PasswordReset,
        owner.id,
        Some(hash(recovered).await?)
    )
    .await
    .is_err());

    // A reset signature over a password-set request does not approve it.
    let set = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::PasswordSet,
        owner.id,
        Some(hash(recovered).await?),
    )
    .await?;
    let as_reset = AccountApproval {
        action: AccountAction::PasswordReset,
        ..set.clone()
    };
    assert!(approve_action(
        &db,
        set.request_id,
        owner.id,
        device.id,
        &sign(&as_reset).to_bytes()
    )
    .await
    .is_err());

    // Signing records the recovery but changes nothing yet.
    recovery_available(&db, owner.id).await?;
    let request = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::PasswordReset,
        owner.id,
        Some(hash(recovered).await?),
    )
    .await?;
    let signature = sign(&request).to_bytes();
    // A password-set signature over the same request is not a reset approval.
    let as_set = AccountApproval {
        action: AccountAction::PasswordSet,
        ..request.clone()
    };
    assert!(approve_action(
        &db,
        request.request_id,
        owner.id,
        device.id,
        &sign(&as_set).to_bytes()
    )
    .await
    .is_err());
    approve_action(&db, request.request_id, owner.id, device.id, &signature).await?;
    assert!(authenticate(&db, &session.token).await.is_ok());
    assert!(
        approve_action(&db, request.request_id, owner.id, device.id, &signature)
            .await
            .is_err()
    );
    let pending = pending_recoveries(&db, owner.id).await?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, request.request_id);
    assert_eq!(pending[0].device_name, "fixture phone");
    let shown = serde_json::to_string(&pending)?;
    assert!(!shown.contains("argon2") && !shown.contains("nonce"));
    // One pending recovery per owner.
    assert!(recovery_available(&db, owner.id).await.is_err());

    // Pending pairing and unlock requests are cancelled by the confirmation.
    let candidate = SigningKey::from_bytes(&rand::random());
    let pairing = create_pairing_request(
        &db,
        owner.id,
        "candidate",
        Platform::Android,
        &candidate.verifying_key().to_bytes(),
    )
    .await?;
    let (unlock, _) = create_unlock_request(&db, owner.id, desktop.id).await?;

    assert!(deny_recovery(&db, owner.id, Uuid::now_v7()).await.is_err());
    assert!(confirm_recovery(&db, owner.id, Uuid::now_v7())
        .await
        .is_err());
    let other = create_user(&db, "other fixture owner").await?;
    assert!(confirm_recovery(&db, other.id, request.request_id)
        .await
        .is_err());
    confirm_recovery(&db, owner.id, request.request_id).await?;
    assert!(confirm_recovery(&db, owner.id, request.request_id)
        .await
        .is_err());
    assert!(authenticate(&db, &session.token).await.is_err());
    assert_eq!(
        pairing_request_status(&db, pairing.id, owner.id)
            .await?
            .as_deref(),
        Some("denied")
    );
    // A system-side deny has no deciding device: its requester keeps reading
    // `pending` until expiry (as for a superseded request), but it leaves
    // every approver's list and can no longer be approved.
    assert_eq!(
        unlock_request_status(&db, unlock, owner.id)
            .await?
            .as_deref(),
        Some("pending")
    );
    assert!(pending_unlock_requests(&db, owner.id, device.id)
        .await?
        .is_empty());
    assert!(pending_recoveries(&db, owner.id).await?.is_empty());
    let verifiers: Option<i64> = db
        .query("RETURN array::len(SELECT id FROM account_recoveries WHERE verifier IS NOT NONE)")
        .await?
        .take(0)?;
    assert_eq!(verifiers, Some(0));
    assert!(login_as(PASSWORD).await.is_err());
    login_as(recovered).await?;

    let fresh = |device: Uuid, key: SigningKey| {
        let db = db.clone();
        async move {
            let request = request_action(
                &db,
                owner.id,
                device,
                AccountAction::PasswordReset,
                owner.id,
                Some(
                    PasswordService::shared()
                        .hash(AccountPassword::new(
                            "another fixture account password never used".to_owned(),
                        )?)
                        .await?,
                ),
            )
            .await?;
            let signature = key.sign(&account_approval_message(&request)?).to_bytes();
            approve_action(&db, request.request_id, owner.id, device, &signature).await?;
            Ok::<_, Box<dyn std::error::Error>>(request.request_id)
        }
    };
    let verifiers = || async {
        let count: Option<i64> = db
            .query(
                "RETURN array::len(SELECT id FROM account_recoveries WHERE verifier IS NOT NONE)",
            )
            .await?
            .take(0)?;
        Ok::<_, Box<dyn std::error::Error>>(count)
    };

    // At most one completed recovery per cooldown, also on the signing path.
    assert!(recovery_available(&db, owner.id).await.is_err());
    assert!(fresh(device.id, key.clone()).await.is_err());
    db.query("UPDATE account_recoveries SET resolved_at = time::now() - 8d, created_at = time::now() - 8d")
        .await?
        .check()?;

    // Expired recoveries cannot be confirmed, free the slot and drop the verifier.
    let expired = fresh(device.id, key.clone()).await?;
    db.query(
        "UPDATE account_recoveries SET expires_at = time::now() - 1s WHERE record::id(id) = $id",
    )
    .bind(("id", expired.to_string()))
    .await?
    .check()?;
    assert!(pending_recoveries(&db, owner.id).await?.is_empty());
    assert_eq!(verifiers().await?, Some(0));
    assert!(confirm_recovery(&db, owner.id, expired).await.is_err());

    // The cooldown is enforced again at confirmation.
    let stale = fresh(device.id, key.clone()).await?;
    db.query("CREATE account_recoveries SET id = 'recent', user_id = $user, device_id = $device, slot = 'recent', verifier = NONE, status = 'confirmed', created_at = time::now(), expires_at = time::now(), resolved_at = time::now()")
        .bind(("user", owner.id.to_string()))
        .bind(("device", device.id.to_string()))
        .await?
        .check()?;
    assert!(confirm_recovery(&db, owner.id, stale).await.is_err());
    db.query("DELETE account_recoveries:recent")
        .await?
        .check()?;

    // A signed password change cancels the pending recovery.
    let set = request_action(
        &db,
        owner.id,
        device.id,
        AccountAction::PasswordSet,
        owner.id,
        Some(hash(PASSWORD).await?),
    )
    .await?;
    approve_action(
        &db,
        set.request_id,
        owner.id,
        device.id,
        &sign(&set).to_bytes(),
    )
    .await?;
    assert!(pending_recoveries(&db, owner.id).await?.is_empty());
    assert_eq!(verifiers().await?, Some(0));
    assert!(confirm_recovery(&db, owner.id, stale).await.is_err());
    assert!(deny_recovery(&db, owner.id, stale).await.is_err());

    // Deny is one-use and drops the verifier.
    let android_key = SigningKey::from_bytes(&rand::random());
    let (android, _) = register_device(
        &db,
        owner.id,
        "fixture tablet",
        Platform::Android,
        "ed25519",
        &android_key.verifying_key().to_bytes(),
    )
    .await?;
    let third = fresh(android.id, android_key.clone()).await?;
    deny_recovery(&db, owner.id, third).await?;
    assert!(deny_recovery(&db, owner.id, third).await.is_err());
    assert!(confirm_recovery(&db, owner.id, third).await.is_err());
    assert_eq!(verifiers().await?, Some(0));

    // A small number of signed requests per day.
    assert!(recovery_available(&db, owner.id).await.is_err());
    assert!(fresh(device.id, key.clone()).await.is_err());

    // Revoking the signing device cancels its pending recovery.
    db.query(
        "UPDATE account_recoveries SET created_at = time::now() - 2d WHERE status != 'confirmed'",
    )
    .await?
    .check()?;
    let revoked = fresh(android.id, android_key).await?;
    revoke_device(&db, android.id).await?;
    assert!(pending_recoveries(&db, owner.id).await?.is_empty());
    assert!(confirm_recovery(&db, owner.id, revoked).await.is_err());
    assert_eq!(verifiers().await?, Some(0));
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn only_a_live_revoke_claim_reports_the_last_approver(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = fixture_db("revoke_claim_fixture").await?;
    let owner = create_user(&db, "fixture owner").await?.id;
    let phone = device(&db, owner, Platform::Ios).await?;
    let laptop = device(&db, owner, Platform::Macos).await?;
    let request = revoke_request(&db, owner, &phone, phone.0.id).await?;
    revoke_device(&db, laptop.0.id).await?;
    let refusal = || {
        revoke_failure(
            &db,
            request.request_id,
            owner,
            phone.0.id,
            phone.0.id,
            IdentityError::AuthFailed,
        )
    };
    // A rolled-back transaction leaves the claim live: the guard refused it.
    assert!(matches!(refusal().await, IdentityError::LastApprover));
    // Consumed (a concurrent or replayed approval) or expired: plain failure.
    for state in [
        "consumed_at = time::now(), expires_at = time::now() + 5m",
        "consumed_at = NONE, expires_at = time::now() - 1s",
    ] {
        db.query(format!(
            "UPDATE account_actions SET {state} WHERE record::id(id) = $id"
        ))
        .bind(("id", request.request_id.to_string()))
        .await?
        .check()?;
        assert!(
            matches!(refusal().await, IdentityError::AuthFailed),
            "{state}"
        );
    }
    db.query("DELETE account_actions WHERE record::id(id) = $id")
        .bind(("id", request.request_id.to_string()))
        .await?
        .check()?;
    assert!(matches!(refusal().await, IdentityError::AuthFailed));
    assert!(matches!(
        approve_signed(&db, owner, &phone, &request).await,
        Err(IdentityError::AuthFailed)
    ));

    // A live claim from a signer that was revoked meanwhile is a plain
    // failure, even when its target is now the last approver.
    let owner = create_user(&db, "fixture owner").await?.id;
    let phone = device(&db, owner, Platform::Ios).await?;
    let laptop = device(&db, owner, Platform::Macos).await?;
    let request = revoke_request(&db, owner, &laptop, phone.0.id).await?;
    revoke_device(&db, laptop.0.id).await?;
    assert!(matches!(
        revoke_failure(
            &db,
            request.request_id,
            owner,
            laptop.0.id,
            phone.0.id,
            IdentityError::AuthFailed,
        )
        .await,
        IdentityError::AuthFailed
    ));
    assert!(matches!(
        approve_signed(&db, owner, &laptop, &request).await,
        Err(IdentityError::AuthFailed)
    ));
    Ok(())
}
