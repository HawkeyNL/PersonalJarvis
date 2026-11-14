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
    assert_eq!(version.unwrap().version, 21);
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
    assert_eq!(from_eleven.unwrap().version, 21);
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
    assert_eq!(from_twelve_version.unwrap().version, 21);
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
    assert_eq!(migrated_version.unwrap().version, 21);
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
    assert_eq!(from_thirteen.unwrap().version, 21);
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
    assert_eq!(from_fourteen.unwrap().version, 21);
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
    assert_eq!(from_fifteen_version.unwrap().version, 21);
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
    assert_eq!(from_sixteen_version.unwrap().version, 21);
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
    db.use_db("from_seventeen").await?;
    apply_through_seventeen(&db).await?;
    db.query(strategy("momentum-a", "").replace("active_version = 2", "active_version = 1"))
        .await?
        .check()?;
    apply_baseline_schema(&db).await?;
    let mut from_seventeen = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE record::id(id) FROM memecoin_strategies;")
        .await?
        .check()?;
    let from_seventeen_version: Option<SchemaVersion> = from_seventeen.take(0)?;
    assert_eq!(from_seventeen_version.unwrap().version, 21);
    let kept: Vec<String> = from_seventeen.take(1)?;
    assert_eq!(kept, ["momentum-a"]);
    // The roadmap tables enforce their shape even if Core validation regressed.
    let (r, i, d) = (
        "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
        "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c",
        "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d",
    );
    let roadmap = |id: &str, extra: &str| {
        format!("CREATE roadmaps SET id = '{id}', title = 'Plan', description = NONE, phases = ['Now', 'Next'], revision = 1, created_at = time::now(), updated_at = time::now(){extra};")
    };
    db.query(roadmap(r, "")).await?.check()?;
    let item = |id: &str, extra: &str| {
        format!("CREATE roadmap_items SET id = '{id}', roadmap_id = '{r}', title = 'Item', status = 'planned', phase = 'Now', position = 0, depends_on = [], links = [], created_at = time::now(), updated_at = time::now(){extra};")
    };
    db.query(item(d, "")).await?.check()?;
    db.query(item(i, &format!(", depends_on = ['{d}'], notes = 'Line one\nline two', links = [{{ kind: 'pr', repo: 'HawkeyNL/PersonalJarvis', number: 214, state: 'open' }}, {{ kind: 'release', repo: 'a/b', tag: 'v0.0.78' }}, {{ kind: 'url', url: 'https://example.com/a?b=c' }}], auto_status = 'in_progress'")))
        .await?
        .check()?;
    let change = |id: &str, extra: &str| {
        format!("CREATE roadmap_changes SET id = '{id}', roadmap_id = '{r}', revision = 2, at = time::now(), author = 'owner', device_id = '{d}', action = 'item_updated', item_id = '{i}', title = 'Item', fields = ['status'], from_status = 'planned', to_status = 'ready'{extra};")
    };
    db.query(change(i, "")).await?.check()?;
    db.query(change(
        d,
        &format!(", author = 'jarvis', proposal_id = '{r}'"),
    ))
    .await?
    .check()?;
    db.query(change(r, ", author = 'github', device_id = NONE"))
        .await?
        .check()?;
    let proposal = |id: &str, extra: &str| {
        format!("CREATE roadmap_proposals SET id = '{id}', roadmap_id = '{r}', base_revision = 1, ops = ['{{\"op\":\"update_item\"}}'], rationale = 'Merged.', status = 'pending', created_at = time::now(), expires_at = time::now() + 14d{extra};")
    };
    db.query(proposal(r, "")).await?.check()?;
    db.query(proposal(
        i,
        &format!(", status = 'accepted', decided_at = time::now(), decided_by = '{d}'"),
    ))
    .await?
    .check()?;
    db.query(proposal(d, ", status = 'stale', decided_at = time::now()"))
        .await?
        .check()?;
    // Each case breaks exactly one rule.
    let x = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5e";
    for bad in [
        roadmap(x, "").replace("'Plan'", "''"),
        roadmap(x, &format!(", title = '{}'", "t".repeat(121))),
        roadmap(x, "").replace("['Now', 'Next']", "['Now', 'Now']"),
        roadmap(x, "").replace("['Now', 'Next']", "['']"),
        roadmap(
            x,
            &format!(
                ", phases = [{}]",
                (0..21)
                    .map(|n| format!("'p{n}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        roadmap(x, "").replace("revision = 1", "revision = 0"),
        item(x, "").replace(&format!("roadmap_id = '{r}'"), "roadmap_id = 'plan'"),
        item(x, "").replace("'planned'", "'done'"),
        item(x, ", auto_status = 'done'"),
        item(x, &format!(", title = '{}'", "t".repeat(201))),
        item(x, "").replace("position = 0", "position = 100001"),
        item(x, &format!(", notes = '{}'", "n".repeat(4001))),
        item(x, &format!(", depends_on = ['{d}', '{d}']")),
        item(x, ", depends_on = ['not-a-uuid']"),
        item(x, ", links = [{ kind: 'commit' }]"),
        item(x, ", links = [{ kind: 'pr', repo: 'owner', number: 1 }]"),
        item(
            x,
            ", links = [{ kind: 'pr', repo: '-owner/repo', number: 1 }]",
        ),
        item(x, ", links = [{ kind: 'pr', repo: 'a/b', number: 0 }]"),
        item(
            x,
            ", links = [{ kind: 'release', repo: 'a/b', tag: 'v 1' }]",
        ),
        item(x, ", links = [{ kind: 'url', url: 'http://example.com' }]"),
        item(
            x,
            ", links = [{ kind: 'url', url: 'https://exa mple.com' }]",
        ),
        item(
            x,
            ", links = [{ kind: 'pr', repo: 'a/b', number: 1, state: 'Open!' }]",
        ),
        item(
            x,
            &format!(
                ", links = [{}]",
                ["{ kind: 'url', url: 'https://a.b' }"; 11].join(", ")
            ),
        ),
        change(x, "").replace("'owner'", "'admin'"),
        // An owner change without a device, a GitHub change with one, and
        // a Jarvis change that is no accepted proposal.
        change(x, ", device_id = NONE"),
        change(x, ", author = 'github'"),
        change(x, ", author = 'jarvis'"),
        change(x, &format!(", proposal_id = '{r}'")),
        change(x, "").replace("'item_updated'", "'item_moved'"),
        change(x, ", item_id = NONE"),
        change(x, "").replace("'item_updated'", "'roadmap_updated'"),
        change(x, "").replace("['status']", "['owner']"),
        change(x, ", to_status = 'done'"),
        proposal(x, ", ops = []"),
        proposal(x, &format!(", ops = [{}]", ["'{}'"; 21].join(", "))),
        proposal(x, &format!(", ops = ['\"{}\"']", "é".repeat(12288))),
        proposal(x, &format!(", rationale = '{}'", "r".repeat(1001))),
        proposal(x, "").replace("'pending'", "'open'"),
        proposal(x, ", decided_at = time::now()"),
        proposal(x, ", status = 'rejected', decided_at = time::now()"),
        proposal(
            x,
            &format!(", status = 'stale', decided_at = time::now(), decided_by = '{d}'"),
        ),
        proposal(x, ", status = 'accepted', decided_at = time::now()"),
        proposal(x, "").replace("time::now() + 14d", "time::now() - 1d"),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.use_db("from_eighteen").await?;
    apply_through_eighteen(&db).await?;
    db.query(roadmap(r, "")).await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_eighteen = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE record::id(id) FROM roadmaps;")
        .await?
        .check()?;
    let from_eighteen_version: Option<SchemaVersion> = from_eighteen.take(0)?;
    assert_eq!(from_eighteen_version.unwrap().version, 21);
    let kept: Vec<String> = from_eighteen.take(1)?;
    assert_eq!(kept, [r]);
    // The job tables enforce their shape even if Core validation regressed.
    let job = |id: &str, extra: &str| {
        format!("CREATE scheduled_jobs SET id = '{id}', kind = 'dependency_review', cadence = 'weekly', weekday = 1, at_minute = 420, enabled = true, revision = 1, params = {{}}, created_at = time::now(), updated_at = time::now(){extra};")
    };
    // Each case breaks exactly one rule.
    for bad in [
        job(x, "").replace("'dependency_review'", "'research'"),
        job(x, ", cadence = 'daily'"),
        job(x, "").replace("'weekly'", "'monthly'"),
        job(x, "").replace("weekday = 1", "weekday = NONE"),
        job(x, "").replace("weekday = 1", "weekday = 8"),
        job(x, "").replace("at_minute = 420", "at_minute = 1440"),
        job(x, "").replace("revision = 1", "revision = 0"),
        job(x, ", params = { roadmap_id: 'plan' }"),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.query(job(
        r,
        &format!(", params = {{ roadmap_id: '{d}' }}, last_slot_at = time::now()"),
    ))
    .await?
    .check()?;
    let run = |id: &str, extra: &str| {
        format!("CREATE scheduled_job_runs SET id = '{id}', job_id = '{r}', kind = 'dependency_review', slot_at = d'2026-11-09T06:00:00Z', trigger = 'schedule', status = 'ok', priorities = [], started_at = time::now(), finished_at = time::now(){extra};")
    };
    db.query(run(r, ", slot_at = d'2026-11-16T06:00:00Z', error_code = 'repo_failed', facts = '{\"alerts\":[]}', summary = 'Line one\nline two', summary_source = 'claude', priorities = [{ ref: 'a1', rank: 1, reason: 'Fix first.' }, { ref: 'p12', rank: 2, reason: 'Merge.' }]"))
        .await?
        .check()?;
    db.query(run(
        i,
        ", status = 'running', finished_at = NONE, trigger = 'owner', slot_at = time::now()",
    ))
    .await?
    .check()?;
    db.query(run(
        d,
        ", slot_at = d'2026-11-02T06:00:00Z', status = 'missed'",
    ))
    .await?
    .check()?;
    for bad in [
        run(x, "").replace("'dependency_review'", "'research'"),
        run(x, "").replace(&format!("job_id = '{r}'"), "job_id = 'job'"),
        // The same slot of the same job twice.
        run(x, ", slot_at = d'2026-11-16T06:00:00Z'"),
        run(x, ", trigger = 'cron'"),
        run(x, ", status = 'done'"),
        run(x, ", status = 'running'"),
        run(x, ", finished_at = NONE"),
        run(x, ", error_code = 'Bad code'"),
        run(x, &format!(", error_code = '{}'", "e".repeat(49))),
        run(x, ", facts = 'x'"),
        run(x, &format!(", facts = '\"{}\"'", "é".repeat(32768))),
        run(x, ", summary = '', summary_source = 'claude'"),
        run(
            x,
            &format!(
                ", summary = '{}', summary_source = 'claude'",
                "s".repeat(2001)
            ),
        ),
        run(x, ", summary = 'Done.'"),
        run(x, ", summary_source = 'claude'"),
        run(x, ", summary = 'Done.', summary_source = 'model'"),
        run(x, ", priorities = [{ ref: 'x1', rank: 1, reason: 'r' }]"),
        run(x, ", priorities = [{ ref: 'a0', rank: 1, reason: 'r' }]"),
        run(x, ", priorities = [{ ref: 'a1', rank: 0, reason: 'r' }]"),
        run(x, ", priorities = [{ ref: 'a1', rank: 21, reason: 'r' }]"),
        run(x, ", priorities = [{ ref: 'a1', rank: 1, reason: '' }]"),
        run(
            x,
            &format!(
                ", priorities = [{{ ref: 'a1', rank: 1, reason: '{}' }}]",
                "r".repeat(301)
            ),
        ),
        run(
            x,
            &format!(
                ", priorities = [{}]",
                ["{ ref: 'a1', rank: 1, reason: 'r' }"; 21].join(", ")
            ),
        ),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.use_db("from_nineteen").await?;
    apply_through_nineteen(&db).await?;
    // A message stored before 0020 has no attachments and stays valid.
    let message = |id: &str, extra: &str| {
        format!("CREATE chat_messages SET id = '{id}', conversation_id = '{r}', user_id = '{d}', role = 'user', content = 'Hello', created_at = time::now(){extra};")
    };
    db.query(message(r, "")).await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_nineteen = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE attachments = NONE FROM chat_messages;")
        .await?
        .check()?;
    let from_nineteen_version: Option<SchemaVersion> = from_nineteen.take(0)?;
    assert_eq!(from_nineteen_version.unwrap().version, 21);
    let legacy_has_no_attachments: Vec<bool> = from_nineteen.take(1)?;
    assert_eq!(legacy_has_no_attachments, [true]);
    db.query(message(i, &format!(", attachments = ['{r}', '{d}']")))
        .await?
        .check()?;
    // The photo table enforces its shape even if Core validation regressed.
    let photo = |id: &str, extra: &str| {
        format!("CREATE chat_photos SET id = '{id}', user_id = '{d}', conversation_id = NONE, saved = false, width = 1600, height = 1, bytes = 1048576, created_at = time::now(){extra};")
    };
    db.query(photo(r, "")).await?.check()?;
    db.query(photo(
        i,
        &format!(", conversation_id = '{r}', saved = true"),
    ))
    .await?
    .check()?;
    // Each case breaks exactly one rule.
    for bad in [
        photo("not-a-uuid", ""),
        photo(x, "").replace("width = 1600", "width = 1601"),
        photo(x, "").replace("width = 1600", "width = 0"),
        photo(x, "").replace("height = 1", "height = 0"),
        photo(x, "").replace("bytes = 1048576", "bytes = 1048577"),
        photo(x, "").replace("bytes = 1048576", "bytes = 0"),
        photo(x, "").replace("saved = false", "saved = NONE"),
        // The same id twice.
        photo(r, ""),
        message(x, &format!(", attachments = ['{r}', '{r}']")),
        message(x, ", attachments = ['photo']"),
        message(
            x,
            &format!(
                ", attachments = ['{r}', '{d}', '{i}', '{x}', '{}']",
                uuid::Uuid::nil()
            ),
        ),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    db.use_db("from_twenty").await?;
    apply_through_twenty(&db).await?;
    // A stored job makes 0021 rebuild the kind index over an existing row,
    // which once failed to commit on RocksDB.
    db.query(job(r, "")).await?.check()?;
    apply_baseline_schema(&db).await?;
    let mut from_twenty = db
        .query("SELECT version FROM schema_version:baseline; SELECT VALUE origin = NONE AND every = NONE AND run_count = NONE AND params.title = NONE FROM scheduled_jobs;")
        .await?
        .check()?;
    let from_twenty_version: Option<SchemaVersion> = from_twenty.take(0)?;
    assert_eq!(from_twenty_version.unwrap().version, 21);
    // A job stored before 0021 keeps its shape, stays valid when written and
    // may now have a second of its kind.
    let legacy: Vec<bool> = from_twenty.take(1)?;
    assert_eq!(legacy, [true]);
    db.query("UPDATE scheduled_jobs SET at_minute = 0;")
        .await?
        .check()?;
    db.query(job(i, "")).await?.check()?;
    let new_job = |id: &str, extra: &str| {
        format!("CREATE scheduled_jobs SET id = '{id}', kind = 'prompt', cadence = 'monthly', month_day = 31, every = 6, start_date = '2026-11-13', max_runs = 2, run_count = 0, origin = 'owner', owner_id = '{d}', at_minute = 540, enabled = true, revision = 1, params = {{ title: 'News', instruction: 'Summarize the AI news', notify: true }}, failure_count = 0, created_at = time::now(), updated_at = time::now(){extra};")
    };
    let fresh_id = || uuid::Uuid::now_v7().to_string();
    for good in [
        new_job(&fresh_id(), ""),
        new_job(&fresh_id(), ", kind = 'reminder', cadence = 'once', month_day = NONE, every = NONE, max_runs = NONE, params = { title: 'Call', text: 'Call the bank.' }"),
        new_job(&fresh_id(), ", kind = 'disk_check', cadence = 'daily', month_day = NONE, every = 1, start_date = NONE, max_runs = NONE, params = { threshold_percent: 80 }"),
        new_job(&fresh_id(), &format!(", origin = 'jarvis', expires_at = time::now() + 30d, conversation_id = '{r}'")),
        new_job(&fresh_id(), ", enabled = false, failure_count = 3, paused_reason = 'paused_after_failures'"),
    ] {
        db.query(&good).await?.check()?;
    }
    db.query(run(
        &fresh_id(),
        ", kind = 'prompt', summary = 'Answer.', summary_source = 'web'",
    ))
    .await?
    .check()?;
    // Each case breaks exactly one rule.
    for bad in [
        new_job(x, ", kind = 'shell'"),
        new_job(x, ", month_day = NONE"),
        new_job(x, ", month_day = 0"),
        new_job(x, ", month_day = 32"),
        new_job(x, ", cadence = 'daily'"),
        new_job(x, ", every = 0"),
        new_job(x, ", every = 366"),
        new_job(x, ", start_date = NONE"),
        new_job(x, ", start_date = '13-11-2026'"),
        new_job(x, ", cadence = 'once', month_day = NONE, every = 1"),
        new_job(x, ", cadence = 'once', month_day = NONE, max_runs = NONE"),
        new_job(x, ", cadence = 'once', month_day = NONE, every = 1, max_runs = NONE, start_date = NONE"),
        new_job(x, ", max_runs = 0"),
        new_job(x, ", max_runs = 101"),
        new_job(x, ", run_count = -1"),
        new_job(x, ", origin = 'system'"),
        new_job(x, ", origin = 'jarvis'"),
        new_job(x, ", origin = 'jarvis', expires_at = time::now() + 30d, kind = 'reminder', params = { title: 'Call' }"),
        new_job(x, ", owner_id = 'owner'"),
        new_job(x, ", conversation_id = 'chat'"),
        new_job(x, ", failure_count = -1"),
        new_job(x, ", paused_reason = 'paused_after_failures'"),
        new_job(x, ", enabled = false, paused_reason = 'tired'"),
        new_job(x, ", params = { title: '', instruction: 'Search' }"),
        new_job(x, &format!(", params = {{ title: '{}', instruction: 'Search' }}", "t".repeat(61))),
        new_job(x, &format!(", params = {{ title: 'News', instruction: '{}' }}", "i".repeat(501))),
        new_job(x, ", params = { title: 'Call', text: '' }"),
        new_job(x, ", params = { threshold_percent: 49 }"),
        new_job(x, ", params = { threshold_percent: 100 }"),
        run(x, ", kind = 'shell', slot_at = d'2026-11-23T06:00:00Z'"),
        run(x, ", slot_at = d'2026-11-23T06:00:00Z', summary = 'Answer.', summary_source = 'search'"),
    ] {
        assert!(db.query(&bad).await?.check().is_err(), "{bad}");
    }
    // A fresh database goes straight to the current schema.
    db.use_db("fresh").await?;
    apply_baseline_schema(&db).await?;
    let fresh: Option<SchemaVersion> = db
        .query("SELECT version FROM schema_version:baseline")
        .await?
        .check()?
        .take(0)?;
    assert_eq!(fresh.unwrap().version, 21);
    db.query(photo(r, "")).await?.check()?;
    db.query("UPDATE schema_version:baseline SET version = 99;")
        .await?
        .check()?;
    assert!(matches!(
        apply_baseline_schema(&db).await,
        Err(StoreError::UnsupportedSchema(99))
    ));
    Ok(())
}
