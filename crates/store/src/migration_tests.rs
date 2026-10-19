use super::*;
use std::env;
use surrealdb::opt::auth::Root;

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn account_migration_preserves_legacy_state_and_rejects_unknown_schema(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!(
        "migration_fixture_{}",
        uuid::Uuid::now_v7().simple()
    ))
    .use_db("before")
    .await?;
    apply_through_six(&db).await?;
    db.query("CREATE users:fixture SET id = 'fixture', display_name = 'Migration fixture', status = 'active', created_at = time::now(), updated_at = time::now();")
        .await?.check()?;
    apply_baseline_schema(&db).await?;
    apply_baseline_schema(&db).await?; // Idempotent startup, no duplicate data.
    let mut result = db.query("SELECT version FROM schema_version:baseline; SELECT VALUE display_name FROM users:fixture;").await?.check()?;
    let version: Option<SchemaVersion> = result.take(0)?;
    let names: Vec<String> = result.take(1)?;
    assert_eq!(version.unwrap().version, 11);
    assert_eq!(names, ["Migration fixture"]);
    // Cold on-disk rollback is tested against RocksDB by the deployment
    // fixture; WebSocket connections intentionally do not implement exports.
    db.use_db("from_seven").await?;
    apply_through_seven(&db).await?;
    apply_baseline_schema(&db).await?; // Also support 7 -> 10 explicitly.
    db.use_db("from_eight").await?;
    apply_through_eight(&db).await?;
    apply_baseline_schema(&db).await?;
    db.use_db("from_nine").await?;
    apply_through_nine(&db).await?;
    apply_baseline_schema(&db).await?;
    db.use_db("from_ten").await?;
    apply_through_ten(&db).await?;
    // A pending v10 account action stays valid across the migration.
    db.query("CREATE account_actions SET id = 'legacy', user_id = 'fixture', device_id = 'd', action = 'password-set', target = 'fixture', nonce = <bytes>'n', verifier = NONE, created_at = time::now(), expires_at = time::now() + 5m, consumed_at = NONE;")
        .await?.check()?;
    assert!(db
        .query("CREATE account_actions SET id = 'early', user_id = 'fixture', device_id = 'e', action = 'password-reset', target = 'fixture', nonce = <bytes>'n', verifier = NONE, created_at = time::now(), expires_at = time::now() + 5m, consumed_at = NONE;")
        .await?
        .check()
        .is_err());
    apply_baseline_schema(&db).await?;
    let mut migrated = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE action FROM account_actions:legacy;")
        .await?
        .check()?;
    let migrated_version: Option<SchemaVersion> = migrated.take(0)?;
    assert_eq!(migrated_version.unwrap().version, 11);
    let legacy: Vec<String> = migrated.take(1)?;
    assert_eq!(legacy, ["password-set"]);
    db.query("CREATE account_actions SET id = 'reset', user_id = 'fixture', device_id = 'e', action = 'password-reset', target = 'fixture', nonce = <bytes>'n', verifier = NONE, created_at = time::now(), expires_at = time::now() + 5m, consumed_at = NONE;")
        .await?.check()?;
    // The database itself allows one pending recovery per owner.
    let pending = "CREATE account_recoveries SET id = $id, user_id = 'fixture', device_id = 'd', slot = 'fixture', verifier = NONE, status = 'pending', created_at = time::now(), expires_at = time::now() + 15m, resolved_at = NONE;";
    db.query(pending).bind(("id", "first")).await?.check()?;
    assert!(db
        .query(pending)
        .bind(("id", "second"))
        .await?
        .check()
        .is_err());
    db.query("UPDATE schema_version:baseline SET version = 99;")
        .await?
        .check()?;
    assert!(matches!(
        apply_baseline_schema(&db).await,
        Err(StoreError::UnsupportedSchema(99))
    ));
    Ok(())
}
