use super::*;
use std::env;
use surrealdb::opt::auth::Root;

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
async fn account_migration_preserves_legacy_state_and_snapshot_recovers_old_schema(
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
    let snapshot = tempfile::tempdir()?;
    let export = snapshot.path().join("fixture.surql");
    db.export(&export).await?;
    apply_baseline_schema(&db).await?;
    apply_baseline_schema(&db).await?; // Idempotent startup, no duplicate data.
    let mut result = db.query("SELECT version FROM schema_version:baseline; SELECT VALUE display_name FROM users:fixture;").await?.check()?;
    let version: Option<SchemaVersion> = result.take(0)?;
    let names: Vec<String> = result.take(1)?;
    assert_eq!(version.unwrap().version, 8);
    assert_eq!(names, ["Migration fixture"]);
    // Restore into an empty namespace/database, never IMPORT over a newer
    // live schema (which could leave new authentication tables behind).
    db.use_db("restored").await?;
    db.import(&export).await?;
    let mut result = db.query("SELECT version FROM schema_version:baseline; SELECT VALUE display_name FROM users:fixture;").await?.check()?;
    let version: Option<SchemaVersion> = result.take(0)?;
    let names: Vec<String> = result.take(1)?;
    assert_eq!(version.unwrap().version, 6);
    assert_eq!(names, ["Migration fixture"]);
    apply_through_six(&db).await?; // Previous schema still starts cleanly.
    apply_through_seven(&db).await?;
    apply_baseline_schema(&db).await?; // Also support 7 -> 8 explicitly.
    db.query("UPDATE schema_version:baseline SET version = 99;")
        .await?
        .check()?;
    assert!(matches!(
        apply_baseline_schema(&db).await,
        Err(StoreError::UnsupportedSchema(99))
    ));
    Ok(())
}
