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
    assert_eq!(version.unwrap().version, 15);
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
    db.use_db("from_eleven").await?;
    apply_through_eleven(&db).await?;
    apply_baseline_schema(&db).await?;
    let from_eleven: Option<SchemaVersion> = db
        .query("SELECT version FROM schema_version:baseline")
        .await?
        .check()?
        .take(0)?;
    assert_eq!(from_eleven.unwrap().version, 15);
    db.use_db("from_twelve").await?;
    apply_through_twelve(&db).await?;
    // A v12 pairing row has none of the new optional fields and stays valid.
    db.query("CREATE device_pairing_requests SET id = 'legacy', user_id = 'fixture', candidate_name = 'old', candidate_platform = 'ios', candidate_public_key = <bytes>'k', candidate_fingerprint = 'f', nonce = <bytes>'n', status = 'pending', created_at = time::now(), expires_at = time::now() + 5m;")
        .await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_twelve = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE candidate_ip = NONE FROM device_pairing_requests:legacy;")
        .await?
        .check()?;
    let from_twelve_version: Option<SchemaVersion> = from_twelve.take(0)?;
    assert_eq!(from_twelve_version.unwrap().version, 15);
    let legacy_has_no_ip: Vec<bool> = from_twelve.take(1)?;
    assert_eq!(legacy_has_no_ip, [true]);
    // The database bounds the new fields even if Core validation regressed.
    let too_long = db
        .query("UPDATE device_pairing_requests:legacy SET candidate_ip = $ip;")
        .bind(("ip", "1".repeat(46)))
        .await?
        .check();
    assert!(too_long.is_err());
    db.query("UPDATE device_pairing_requests:legacy SET candidate_ip = '192.0.2.1', candidate_user_agent = 'ua', candidate_app_version = '0.1.14', candidate_model = 'Pixel 9';")
        .await?
        .check()?;
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
    assert_eq!(migrated_version.unwrap().version, 15);
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
    // One push target per device; malformed kinds, tokens and environments
    // are refused by the schema itself.
    let (token, other) = ("ab".repeat(32), "cd".repeat(32));
    let create = |device: &str, kind: &str, target: &str, env: &str| {
        format!("CREATE device_push_targets SET id = '{device}', device_id = '{device}', kind = '{kind}', target = '{target}', apns_env = '{env}', updated_at = time::now();")
    };
    db.query(create("d", "apns", &token, "sandbox"))
        .await?
        .check()?;
    for bad in [
        // A second row for the same device.
        create("e", "apns", &other, "sandbox").replace("device_id = 'e'", "device_id = 'd'"),
        // The same token on a second device.
        create("e", "apns", &token, "sandbox"),
        create("e", "fcm", &other, "sandbox"),
        create("e", "apns", &"ab".repeat(16), "sandbox"),
        create("e", "apns", &"ab".repeat(101), "sandbox"),
        create("e", "apns", &other, "development"),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.query(create("e", "apns", &other, "production"))
        .await?
        .check()?;
    db.use_db("from_thirteen").await?;
    apply_through_thirteen(&db).await?;
    apply_baseline_schema(&db).await?;
    let from_thirteen: Option<SchemaVersion> = db
        .query("SELECT version FROM schema_version:baseline")
        .await?
        .check()?
        .take(0)?;
    assert_eq!(from_thirteen.unwrap().version, 15);
    // The paper tables enforce their shape even if Core validation regressed.
    let (mint, other) = (
        "So11111111111111111111111111111111111111112",
        "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263",
    );
    let trip = |id: &str, mint: &str, extra: &str| {
        format!("CREATE memecoin_paper_trips SET id = '{id}', slot = '{mint}', mint = '{mint}', strategy_version = 1, status = 'open', tokens = '10.5', cost_usd = '25.01', entry_liquidity_usd = '50000', exit_failures = 0, opened_at = time::now(){extra};")
    };
    db.query(trip("a", mint, "")).await?.check()?;
    // Each case breaks exactly one rule.
    for bad in [
        // A second open trip for the same mint.
        trip("b", mint, ""),
        // Closed fields on an open trip.
        trip("c", other, ", usd_out = '1'"),
        // A float instead of a decimal string.
        trip("d", other, "").replace("tokens = '10.5'", "tokens = '1e5'"),
        // Not a base58 mint.
        trip("e", "not-a-mint-0OIl-not-a-mint-0OIl-x", ""),
        // A stuck trip with exit fields, and a negative failure count.
        trip("g", other, ", exit_reason = 'stop_loss'").replace("status = 'open'", "status = 'stuck'"),
        trip("h", other, "").replace("exit_failures = 0", "exit_failures = -1"),
        // A closed trip without its exit fields.
        trip("f", other, "").replace("status = 'open'", "status = 'closed'").replace(&format!("slot = '{other}'"), "slot = 'f'"),
        "CREATE memecoin_candidates SET id = 'y', mint = 'So11111111111111111111111111111111111111112', observed_at = time::now(), strategy_version = 1, reason = 'because';".to_string(),
        // A pause without its time.
        "CREATE memecoin_paper_control SET id = 'paper', paused = true, paused_by = 'd';".to_string(),
        "CREATE memecoin_paper_costs SET id = 'z', kind = 'refund', usd = '1', at = time::now(), strategy_version = 1;".to_string(),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    // A stuck position still holds its mint.
    db.query(trip("s", other, "").replace("status = 'open'", "status = 'stuck'"))
        .await?
        .check()?;
    assert!(db.query(trip("s2", other, "")).await?.check().is_err());
    db.query("UPDATE memecoin_paper_trips:a SET status = 'closed', slot = 'a', closed_at = time::now(), usd_out = '30', exit_fees_usd = '0.02', net_pnl_usd = '-0.03', exit_reason = 'take_profit';")
        .await?
        .check()?;
    // Closed, so the mint is free for a new position.
    db.query(trip("b", mint, "")).await?.check()?;
    db.query("CREATE memecoin_candidates SET id = 'r', mint = 'So11111111111111111111111111111111111111112', observed_at = time::now(), strategy_version = 1, reason = 'lp_not_secured', price_usd = '0.0001', liquidity_usd = NONE;")
        .await?
        .check()?;
    db.use_db("from_fourteen").await?;
    apply_through_fourteen(&db).await?;
    apply_baseline_schema(&db).await?;
    let from_fourteen: Option<SchemaVersion> = db
        .query("SELECT version FROM schema_version:baseline")
        .await?
        .check()?
        .take(0)?;
    assert_eq!(from_fourteen.unwrap().version, 15);
    // The IBKR read model enforces its shape even if Core validation regressed.
    let position = |id: &str, extra: &str| {
        format!("CREATE ibkr_positions SET id = '{id}', conid = {id}, symbol = 'VWCE', asset_class = 'STK', currency = 'EUR', quantity = '12.5', avg_cost = '101.25', last_price = '120', market_value = '1500', unrealized_pnl = '-3.5'{extra};")
    };
    db.query(position("1", "")).await?.check()?;
    db.query("UPSERT ibkr_portfolio:current SET id = 'current', as_of = time::now(), base_currency = 'EUR', net_liquidation = '2500.5', cash = '-1000', skipped_count = 2, skipped_positions = [{ symbol: 'AAPL', currency: 'USD' }, { symbol: 'NOCUR' }];")
        .await?
        .check()?;
    db.query("UPSERT ibkr_portfolio_history:⟨2026-11-01⟩ SET id = '2026-11-01', t = time::now(), value = '2500.5';")
        .await?
        .check()?;
    // Each case breaks exactly one rule.
    for bad in [
        // The same contract twice.
        position("1", "").replace("id = '1'", "id = '9'"),
        position("2", ", name = ''"),
        position("3", "").replace("asset_class = 'STK'", "asset_class = 'stk'"),
        position("4", "").replace("currency = 'EUR'", "currency = 'EURO'"),
        // A float instead of a decimal string.
        position("5", "").replace("quantity = '12.5'", "quantity = '1e5'"),
        position("6", "").replace("conid = 6", "conid = 0"),
        "UPSERT ibkr_portfolio:current SET base_currency = 'USD';".to_string(),
        "UPSERT ibkr_portfolio:current SET cash = '1,5';".to_string(),
        "UPSERT ibkr_portfolio:current SET skipped_count = -1;".to_string(),
        "UPSERT ibkr_portfolio:current SET skipped_positions = [{ symbol: 'AAPL', currency: 'usd' }];".to_string(),
        "UPSERT ibkr_portfolio:current SET skipped_positions = [{ symbol: '' }];".to_string(),
        format!("UPSERT ibkr_portfolio:current SET skipped_positions = [{}];", ["{ symbol: 'X' }"; 51].join(", ")),
        "CREATE ibkr_portfolio_history SET id = 'x', t = time::now(), value = 'NaN';".to_string(),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.query("UPDATE schema_version:baseline SET version = 99;")
        .await?
        .check()?;
    assert!(matches!(
        apply_baseline_schema(&db).await,
        Err(StoreError::UnsupportedSchema(99))
    ));
    Ok(())
}
