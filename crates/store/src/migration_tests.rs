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
    assert_eq!(version.unwrap().version, 17);
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
    assert_eq!(from_eleven.unwrap().version, 17);
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
    assert_eq!(from_twelve_version.unwrap().version, 17);
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
    assert_eq!(migrated_version.unwrap().version, 17);
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
    assert_eq!(from_thirteen.unwrap().version, 17);
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
    assert_eq!(from_fourteen.unwrap().version, 17);
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
    db.use_db("from_fifteen").await?;
    apply_through_fifteen(&db).await?;
    // A v15 session has no password anchor and stays valid.
    let session = |id: &str, extra: &str| {
        format!("CREATE sessions SET id = '{id}', user_id = 'u', device_id = 'd', token_hash = <bytes>'{id}', created_at = time::now(), expires_at = time::now() + 7d{extra};")
    };
    db.query(session("legacy", "")).await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_fifteen = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE password_verified_at = NONE FROM sessions:legacy;")
        .await?
        .check()?;
    let from_fifteen_version: Option<SchemaVersion> = from_fifteen.take(0)?;
    assert_eq!(from_fifteen_version.unwrap().version, 17);
    let legacy_has_no_anchor: Vec<bool> = from_fifteen.take(1)?;
    assert_eq!(legacy_has_no_anchor, [true]);
    db.query(session(
        "anchored",
        ", password_verified_at = time::now(), password_revision = 'r'",
    ))
    .await?
    .check()?;
    // The anchor keeps its shape even if Core validation regressed.
    for bad in [
        session("bad_time", ", password_verified_at = 'yesterday'"),
        session(
            "bad_revision",
            &format!(", password_revision = '{}'", "r".repeat(65)),
        ),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.use_db("from_sixteen").await?;
    apply_through_sixteen(&db).await?;
    // A v16 strategy version and trips have none of the new fields.
    let version = |n: u32, extra: &str| {
        format!("CREATE memecoin_strategy_versions SET id = 'v{n}', version = {n}, params = '{{}}', created_at = time::now(){extra};")
    };
    db.query(version(1, "")).await?.check()?;
    db.query(trip("old", mint, "")).await?.check()?;
    db.query("UPDATE memecoin_paper_trips:old SET status = 'closed', slot = 'old', closed_at = time::now(), usd_out = '20', exit_fees_usd = '0.02', net_pnl_usd = '-5.03', exit_reason = 'time_stop';")
        .await?
        .check()?;
    db.query(trip("held", mint, "")).await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_sixteen = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE strategy_id = NONE AND family = NONE FROM memecoin_strategy_versions:v1;")
        .await?
        .check()?;
    let from_sixteen_version: Option<SchemaVersion> = from_sixteen.take(0)?;
    assert_eq!(from_sixteen_version.unwrap().version, 17);
    let legacy_has_no_lineage: Vec<bool> = from_sixteen.take(1)?;
    assert_eq!(legacy_has_no_lineage, [true]);
    // Legacy rows still pass every rule when they are written again.
    db.query("UPDATE memecoin_strategy_versions:v1 SET created_at = time::now(); UPDATE memecoin_paper_trips:old SET exit_failures = 0;")
        .await?
        .check()?;
    let lineage = ", strategy_id = 'momentum-a', family = 'a', author = 'claude'";
    db.query(version(
        2,
        &format!(
            "{lineage}, parent_version = 1, rationale = 'Tighter stop.', backtest = '{}'",
            r#"{"trades":3}"#
        ),
    ))
    .await?
    .check()?;
    let strategy = |id: &str, extra: &str| {
        format!("CREATE memecoin_strategies SET id = '{id}', active_version = 2, enabled = true, disabled_by_owner = false, changed_at = time::now(), changed_by = 'seed'{extra};")
    };
    db.query(strategy("momentum-a", "")).await?.check()?;
    let decision = |id: &str, extra: &str| {
        format!("CREATE memecoin_decisions SET id = '{id}', at = time::now(), call_id = 'call-1'{extra};")
    };
    db.query(decision("enter", &format!(", kind = 'enter', strategy_version = 2, mint = '{mint}', size_bps = 2500, reason_code = 'volume_spike', rationale = 'Volume rose.', model = 'claude-sonnet-4-5', input_tokens = 900, output_tokens = 120")))
        .await?
        .check()?;
    db.query(decision("down", ", kind = 'worker_unavailable'"))
        .await?
        .check()?;
    let observation = |id: &str, extra: &str| {
        format!("CREATE memecoin_observations SET id = '{id}', mint = '{mint}', at = time::now(), price_usd = '0.0001', liquidity_usd = '50000', daily = true{extra};")
    };
    db.query(observation("o1", "")).await?.check()?;
    let universe = |mint: &str, extra: &str| {
        format!("CREATE memecoin_universe SET id = '{mint}', mint = '{mint}', pool = '{other}', program = '{other}', migrated_at = time::now(){extra};")
    };
    db.query(universe(mint, "")).await?.check()?;
    db.query("UPDATE memecoin_paper_trips:held SET status = 'closed', slot = 'held', closed_at = time::now(), usd_out = '30', exit_fees_usd = '0.02', net_pnl_usd = '4.97', exit_reason = 'trailing_stop', peak_value_usd = '31.5';")
        .await?
        .check()?;
    // Each case breaks exactly one rule.
    for bad in [
        version(10, &lineage.replace("momentum-a", "Momentum_A")),
        version(11, &lineage.replace("'a'", "'d'")),
        // Lineage without a strategy, and a strategy without an author.
        version(12, ", family = 'a'"),
        version(18, ", parent_version = 1"),
        version(19, ", author = 'seed'"),
        version(20, ", rationale = 'r'"),
        version(21, ", backtest = '{}'"),
        version(22, &format!("{lineage}, backtest = '1'")),
        version(13, &lineage.replace(", author = 'claude'", "")),
        version(14, &format!("{lineage}, parent_version = 14")),
        version(15, &lineage.replace("'claude'", "'owner'")),
        version(16, &format!("{lineage}, rationale = '{}'", "r".repeat(601))),
        // 2050 characters, but 4098 bytes.
        version(
            17,
            &format!("{lineage}, backtest = '\"{}\"'", "é".repeat(2048)),
        ),
        // An owner's disable while enabled.
        strategy("b", "").replace("disabled_by_owner = false", "disabled_by_owner = true"),
        strategy("c", "").replace("'seed'", "''"),
        strategy("d", "").replace("active_version = 2", "active_version = 0"),
        strategy("Bad_Id", ""),
        decision("x1", ", kind = 'buy'"),
        decision("x2", &format!(", kind = 'enter', mint = '{mint}'")),
        decision("x3", ", kind = 'skip', size_bps = 100"),
        decision("x4", ", kind = 'exit'"),
        decision(
            "x5",
            &format!(", kind = 'enter', mint = '{mint}', size_bps = 10001"),
        ),
        decision("x6", ", kind = 'skip', reason_code = 'Not-Snake'"),
        decision(
            "x7",
            &format!(", kind = 'skip', rationale = '{}'", "r".repeat(301)),
        ),
        decision("x8", ", kind = 'hold'").replace("call-1", "call 1"),
        decision("x9", ", kind = 'hold', output_tokens = -1"),
        observation("o2", "").replace("'0.0001'", "'1e-4'"),
        observation("o3", "").replace(", daily = true", ""),
        // A second row for the same mint.
        universe(mint, "").replace(&format!("id = '{mint}'"), "id = 'u2'"),
        universe(other, "").replace(&format!("pool = '{other}'"), "pool = 'not-base58-0OIl'"),
        "UPDATE memecoin_paper_trips:old SET exit_reason = 'panic';".to_string(),
        "UPDATE memecoin_paper_trips:old SET peak_value_usd = '-1';".to_string(),
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
