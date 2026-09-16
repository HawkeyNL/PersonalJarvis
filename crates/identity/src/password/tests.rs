use super::*;

const FIXTURE: &str = "fixture password not a real credential";

fn password(value: &str) -> AccountPassword {
    AccountPassword::new(value.to_owned()).unwrap()
}

#[test]
fn input_is_bounded_and_debug_is_redacted() {
    for invalid in [
        "short".to_owned(),
        "a".repeat(MAX_PASSWORD_BYTES + 1),
        format!("{FIXTURE}\n"),
        format!("{FIXTURE}\0"),
    ] {
        assert_eq!(
            AccountPassword::new(invalid).unwrap_err(),
            PasswordError::InvalidInput
        );
    }
    assert_eq!(
        format!("{:?}", password(FIXTURE)),
        "AccountPassword([REDACTED])"
    );
    assert!(AccountPassword::new("é".repeat(15)).is_ok());
    assert!(AccountPassword::new("é".repeat(14)).is_err());
}

#[tokio::test]
async fn salted_hash_roundtrip_and_wrong_password() {
    let service = PasswordService::default();
    let one = service.hash(password(FIXTURE)).await.unwrap();
    let two = service.hash(password(FIXTURE)).await.unwrap();
    assert_ne!(one.as_storage_str(), two.as_storage_str());
    assert!(!one.as_storage_str().contains(FIXTURE));
    assert_eq!(format!("{one:?}"), "StoredPassword([REDACTED])");
    service.verify(password(FIXTURE), one).await.unwrap();
    assert_eq!(
        service
            .verify(password(&format!("{FIXTURE} ")), two)
            .await
            .unwrap_err(),
        PasswordError::AuthenticationFailed
    );
}

#[tokio::test]
async fn stored_parameters_cannot_inflate_cost_or_downgrade_hash() {
    let stored = PasswordService::default()
        .hash(password(FIXTURE))
        .await
        .unwrap();
    for value in [
        stored.as_storage_str().replace("m=65536", "m=4294967295"),
        stored.as_storage_str().replace("t=3", "t=4294967295"),
        stored.as_storage_str().replace("m=65536", "m=8"),
        stored.as_storage_str().replace("argon2id", "argon2i"),
        stored.as_storage_str().replace("v=19", "v=16"),
        "x".repeat(MAX_PHC_BYTES + 1),
        "malformed".to_owned(),
    ] {
        assert_eq!(
            StoredPassword::from_storage(value).unwrap_err(),
            PasswordError::InvalidVerifier
        );
    }
}

#[tokio::test]
async fn service_clones_share_admission_limit() {
    let service = PasswordService::default();
    let clone = service.clone();
    let permits = service.slots.clone().acquire_many_owned(2).await.unwrap();
    assert_eq!(
        clone.hash(password(FIXTURE)).await.unwrap_err(),
        PasswordError::Busy
    );
    drop(permits);
    assert_eq!(service.slots.available_permits(), 2);
}

#[tokio::test]
async fn cancelled_waiter_does_not_release_running_work_slot() {
    let service = PasswordService::default();
    let clone = service.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        clone
            .work(move || {
                let _ = started_tx.send(());
                finish_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(service.slots.available_permits(), 1);
    finish_tx.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while service.slots.available_permits() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
