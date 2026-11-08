//! Memecoin PAPER trader for the owner. Writes only reduce activity (pause,
//! disable a strategy), except enabling a strategy again, which needs the
//! owner's device signature. Resuming is configuration-only. Money is
//! returned as decimal strings.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use jarvis_identity as identity;
use jarvis_onchain::config::StrategyConfig;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::audit::record_security_event;
use crate::error::{bad_request, unauthorized};
use crate::memecoin::{agent, announce, store, EnableChallenge, ENABLE_CHALLENGE_TTL};
use crate::rate_limit::allow_authenticated_device;
use crate::{validation, AppState, Authed};
use jarvis_client_core::realtime::Event;

const DEFAULT_LIMIT: usize = 50;
const DB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "paper trader data unavailable" })),
    )
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, store::MemecoinStoreError>>,
) -> Result<T, (StatusCode, Json<Value>)> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "memecoin paper read failed");
            Err(unavailable())
        }
        Err(_) => Err(unavailable()),
    }
}

fn ts(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

fn money(value: Decimal) -> String {
    value.normalize().to_string()
}

#[derive(Deserialize)]
pub(crate) struct LimitQuery {
    limit: Option<usize>,
}

fn limit(query: &LimitQuery) -> Result<usize, (StatusCode, Json<Value>)> {
    match query.limit {
        None => Ok(DEFAULT_LIMIT),
        Some(limit) if (1..=store::MAX_LIST).contains(&limit) => Ok(limit),
        Some(_) => Err(bad_request("limit must be 1 to 100")),
    }
}

pub(crate) async fn overview(_authed: Authed, State(state): State<AppState>) -> ApiResult {
    let control = bounded(store::control(&state.db)).await?;
    let handle = &state.memecoin;
    let mut body = json!({
        "mode": "paper",
        "enabled": handle.enabled(),
        "running": handle.running(),
        "paused": control.paused || handle.paused(),
        "paused_at": control.paused_at.map(ts),
        "last_error": handle.last_error(),
        "resume": "configuration only (JARVIS_MEMECOIN_PAPER_RESUME_PAUSED_AT)",
        "agent": handle.agent().map(|status| {
            let budget = status.budget;
            json!({
                "calling": status.calling,
                "calls_last_hour": budget.calls_last_hour,
                "calls_last_day": budget.calls_last_day,
                "max_calls_per_hour": status.settings.max_calls_per_hour,
                "max_calls_per_day": status.settings.max_calls_per_day,
                "last_call_at": budget.last_call_at.map(ts),
                "paused_until": budget.paused_until.map(ts),
                "backoff_until": budget.backoff_until.map(ts),
            })
        }),
    });
    if let Some(view) = handle.view() {
        // Deprecated: see `PaperView::strategy_version`.
        body["strategy_version"] = view.strategy_version.into();
        body["as_of"] = ts(view.at).into();
        body["stake_usd"] = money(view.stake_usd).into();
        body["equity_usd"] = money(view.equity_usd).into();
        body["pnl"] = json!({
            "realised_usd": money(view.pnl.realised_usd),
            "unrealised_usd": money(view.pnl.unrealised_usd),
            "failed_tx_fees_usd": money(view.pnl.failed_tx_fees_usd),
            "ai_cost_usd": money(view.pnl.ai_cost_usd),
            "net_usd": money(view.pnl.net_usd),
        });
        body["open_positions"] = view
            .open
            .iter()
            .map(|p| {
                json!({
                    "mint": p.mint,
                    "strategy_version": p.strategy_version,
                    "tokens": money(p.tokens),
                    "cost_usd": money(p.cost_usd),
                    "value_usd": p.value_usd.map(money),
                    "opened_at": ts(p.opened_at),
                    "exit_failures": p.exit_failures,
                    "stuck": p.stuck,
                })
            })
            .collect::<Vec<_>>()
            .into();
        let gate = view.gate;
        body["gate"] = json!({
            "closed_trades": gate.closed_trades,
            "min_closed_trades": jarvis_onchain::gate::MIN_CLOSED_TRADES,
            "duration_days": gate.duration.whole_days(),
            "min_duration_days": jarvis_onchain::gate::MIN_DURATION.whole_days(),
            "trading_days": gate.trading_days,
            "min_trading_days": jarvis_onchain::gate::MIN_TRADING_DAYS,
            "net_result_usd": money(gate.net_result_usd),
            "net_expectancy_usd": money(gate.net_expectancy_usd),
            "enough_trades": gate.enough_trades,
            "enough_time": gate.enough_time,
            "enough_days": gate.enough_days,
            "positive_expectancy": gate.positive_expectancy,
            "passed": gate.passed,
        });
        body["goal"] = json!({
            "multiple_target": money(view.goal_multiple),
            "multiple": view.goal.map(|g| money(g.multiple)),
            "fraction_to_goal": view.goal.map(|g| money(g.fraction_to_goal)),
            "remaining_usd": view.goal.map(|g| money(g.remaining_usd)),
            "reached": view.goal.map(|g| g.reached),
        });
    }
    Ok(Json(body))
}

pub(crate) async fn candidates(
    _authed: Authed,
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult {
    let rows = bounded(store::recent_candidates(&state.db, limit(&query)?)).await?;
    Ok(Json(json!({
        "candidates": rows.into_iter().map(|c| json!({
            "mint": c.mint,
            "observed_at": ts(c.observed_at),
            "strategy_version": c.strategy_version,
            "verdict": if c.reason.is_none() { "accept" } else { "reject" },
            "reason": c.reason,
            "price_usd": c.price_usd.map(money),
            "liquidity_usd": c.liquidity_usd.map(money),
        })).collect::<Vec<_>>(),
    })))
}

pub(crate) async fn trades(
    _authed: Authed,
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult {
    let rows = bounded(store::recent_closed_trips(&state.db, limit(&query)?)).await?;
    Ok(Json(json!({
        "trades": rows.into_iter().filter_map(|t| {
            let close = t.close?;
            Some(json!({
                "mint": t.mint,
                "strategy_version": t.strategy_version,
                "tokens": money(t.tokens),
                "cost_usd": money(t.cost_usd),
                "opened_at": ts(t.opened_at),
                "closed_at": ts(close.closed_at),
                "usd_out": money(close.usd_out),
                "exit_fees_usd": money(close.exit_fees_usd),
                "net_pnl_usd": money(close.net_pnl_usd),
                "exit_reason": close.exit_reason,
            }))
        }).collect::<Vec<_>>(),
    })))
}

/// Pause paper entries. Tightening only: it takes effect in memory at once,
/// then persists; exits keep running.
pub(crate) async fn pause(authed: Authed, State(state): State<AppState>) -> ApiResult {
    state.memecoin.pause_now();
    let device = authed.device.id.to_string();
    let persisted = bounded(store::pause(&state.db, &device)).await;
    record_security_event(
        &state,
        Some(authed.device.id),
        "memecoin.paper.pause",
        if persisted.is_ok() {
            "paused"
        } else {
            "paused_unpersisted"
        },
        None,
    )
    .await;
    persisted?;
    Ok(Json(json!({ "paused": true })))
}

/// Strategy changes per device and minute (disable, challenge, enable).
const STRATEGY_WRITES_PER_MIN: u32 = 30;
/// Versions listed per strategy, newest first.
const VERSIONS_LISTED: usize = 20;

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

fn not_found() -> (StatusCode, Json<Value>) {
    error(StatusCode::NOT_FOUND, "no such strategy")
}

fn strategy_id(id: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let valid = (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(bad_request("invalid strategy id"))
    }
}

fn throttle(state: &AppState, authed: &Authed) -> Result<(), (StatusCode, Json<Value>)> {
    if allow_authenticated_device(
        state,
        authed.device.id,
        "meme-strategy",
        STRATEGY_WRITES_PER_MIN,
    ) {
        Ok(())
    } else {
        Err(error(StatusCode::TOO_MANY_REQUESTS, "rate limited"))
    }
}

fn performance(p: &store::StrategyPerformance) -> Value {
    json!({
        "trades": p.closed.trades,
        "net_usd": money(p.net_usd),
        "costs_usd": money(p.costs_usd),
        "expectancy_usd": money(p.closed.expectancy_usd),
        "win_rate_bps": p.closed.win_rate_bps,
        "max_drawdown_usd": money(p.closed.max_drawdown_usd),
        "profit_factor": p.closed.profit_factor.map(money),
    })
}

async fn find_strategy(
    state: &AppState,
    id: &str,
) -> Result<store::Strategy, (StatusCode, Json<Value>)> {
    bounded(store::list_strategies(&state.db))
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(not_found)
}

/// The parameter JSON of `strategy`'s active version, recorded for it.
async fn active_params(
    state: &AppState,
    strategy: &store::Strategy,
) -> Result<String, (StatusCode, Json<Value>)> {
    match bounded(store::version_params(&state.db, strategy.active_version)).await? {
        Some((Some(owner), params)) if owner == strategy.id => Ok(params),
        _ => Err(unavailable()),
    }
}

fn strategy_row(
    strategy: &store::Strategy,
    family: Option<Value>,
    perf: &store::StrategyPerformance,
) -> Value {
    json!({
        "id": strategy.id,
        "family": family,
        "active_version": strategy.active_version,
        "enabled": strategy.enabled,
        "disabled_by_owner": strategy.disabled_by_owner,
        "changed_at": ts(strategy.changed_at),
        "changed_by": strategy.changed_by,
        "performance": performance(perf),
    })
}

fn family(params: &str) -> Option<Value> {
    serde_json::from_str::<StrategyConfig>(params)
        .ok()
        .and_then(|config| serde_json::to_value(config.family).ok())
}

pub(crate) async fn strategies(_authed: Authed, State(state): State<AppState>) -> ApiResult {
    let mut rows = Vec::new();
    for strategy in bounded(store::list_strategies(&state.db)).await? {
        let perf = bounded(store::strategy_performance(&state.db, &strategy.id)).await?;
        let params = bounded(store::version_params(&state.db, strategy.active_version)).await?;
        let family = params.and_then(|(_, params)| family(&params));
        rows.push(strategy_row(&strategy, family, &perf));
    }
    Ok(Json(json!({ "strategies": rows })))
}

/// One strategy with its newest versions: each version's config, the
/// per-field diff to its parent (computed by Core), the author's rationale
/// (display only) and the stored backtest; its paper results and the latest
/// gate snapshot of the active version.
pub(crate) async fn strategy(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult {
    strategy_id(&id)?;
    let strategy = find_strategy(&state, &id).await?;
    let versions = bounded(store::list_strategy_versions(
        &state.db,
        &id,
        VERSIONS_LISTED,
    ))
    .await?;
    let parse = |params: &str| serde_json::from_str::<StrategyConfig>(params).ok();
    let mut listed = Vec::with_capacity(versions.len());
    for version in &versions {
        let config = parse(&version.params);
        let parent = match version.parent_version {
            None => None,
            Some(parent) => match versions.iter().find(|v| v.version == parent) {
                Some(v) => parse(&v.params),
                None => bounded(store::version_params(&state.db, parent))
                    .await?
                    .and_then(|(_, params)| parse(&params)),
            },
        };
        let diff = config
            .as_ref()
            .zip(parent.as_ref())
            .map(|(config, parent)| agent::config_diff(parent, config));
        listed.push(json!({
            "version": version.version,
            "parent_version": version.parent_version,
            "author": match version.author {
                store::StrategyAuthor::Seed => "seed",
                store::StrategyAuthor::Claude => "claude",
            },
            "created_at": ts(version.created_at),
            "config": config,
            "diff": diff,
            "rationale": version.rationale,
            "backtest": version.backtest.as_deref().and_then(|b| serde_json::from_str::<Value>(b).ok()),
        }));
    }
    let perf = bounded(store::strategy_performance(&state.db, &id)).await?;
    let gate = bounded(store::latest_gate_snapshot(
        &state.db,
        strategy.active_version,
    ))
    .await?
    .map(|g| {
        let days = i64::try_from(g.duration_secs / 86_400).unwrap_or(i64::MAX);
        json!({
            "at": ts(g.at),
            "closed_trades": g.closed_trades,
            "min_closed_trades": jarvis_onchain::gate::MIN_CLOSED_TRADES,
            "duration_days": days,
            "min_duration_days": jarvis_onchain::gate::MIN_DURATION.whole_days(),
            "trading_days": g.trading_days,
            "min_trading_days": jarvis_onchain::gate::MIN_TRADING_DAYS,
            "net_result_usd": money(g.net_result_usd),
            "net_expectancy_usd": money(g.net_expectancy_usd),
        })
    });
    let active = versions
        .iter()
        .find(|v| v.version == strategy.active_version);
    let mut body = strategy_row(&strategy, active.and_then(|v| family(&v.params)), &perf);
    body["active_config_sha256"] = active
        .map(|v| hex::encode(Sha256::digest(v.params.as_bytes())))
        .into();
    body["versions"] = listed.into();
    body["gate"] = gate.into();
    Ok(Json(body))
}

/// Disable a strategy at once: no signature, the owner's session suffices.
/// Its open positions keep exiting. Audited.
pub(crate) async fn disable(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult {
    strategy_id(&id)?;
    throttle(&state, &authed)?;
    // In memory first, so entries stop within the running tick, even when
    // the database cannot be reached.
    state.memecoin.disable_now(&id);
    let device = authed.device.id.to_string();
    let changed = bounded(store::set_strategy_enabled(
        &state.db, &id, false, true, &device,
    ))
    .await;
    let outcome = match &changed {
        Ok(true) => "disabled",
        Ok(false) => "not_found",
        Err(_) => "disabled_unpersisted",
    };
    record_security_event(
        &state,
        Some(authed.device.id),
        "memecoin.strategy.disable",
        outcome,
        Some(&id),
    )
    .await;
    if !changed? {
        state.memecoin.clear_disabled(&id);
        return Err(not_found());
    }
    if let Ok(strategy) = find_strategy(&state, &id).await {
        announce(
            Some(&state.realtime),
            Event::MemeStrategyChanged {
                strategy_id: id.clone(),
                version: strategy.active_version,
                enabled: false,
            },
        );
    }
    Ok(Json(json!({ "strategy_id": id, "enabled": false })))
}

/// A single-use challenge for enabling a disabled strategy, bound to its
/// active version. Only a device that can approve may ask for one.
pub(crate) async fn enable_challenge(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult {
    strategy_id(&id)?;
    throttle(&state, &authed)?;
    if !identity::surreal::APPROVER_PLATFORMS.contains(&authed.device.platform.as_str()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "this device cannot approve a strategy enable",
        ));
    }
    let strategy = find_strategy(&state, &id).await?;
    if strategy.enabled {
        return Err(error(StatusCode::CONFLICT, "strategy is already enabled"));
    }
    let params = active_params(&state, &strategy).await?;
    let mut nonce = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut nonce);
    let now = OffsetDateTime::now_utc();
    let challenge = EnableChallenge {
        strategy_id: id.clone(),
        version: strategy.active_version,
        config_sha256: Sha256::digest(params.as_bytes()).into(),
        nonce,
        expires_at: now + ENABLE_CHALLENGE_TTL,
    };
    let body = json!({
        "strategy_id": id,
        "version": challenge.version,
        "config_sha256": hex::encode(challenge.config_sha256),
        // The exact bytes `config_sha256` hashes, so the device can hash
        // what it shows.
        "config_json": params,
        "nonce": hex::encode(nonce),
        "expires_at": ts(challenge.expires_at),
        "message": jarvis_client_core::MEME_STRATEGY_ENABLE_MESSAGE_V1,
    });
    let issued = state
        .memecoin
        .enable_challenges
        .lock()
        .map(|mut open| open.issue(authed.device.id, challenge, now))
        .unwrap_or(false);
    record_security_event(
        &state,
        Some(authed.device.id),
        "memecoin.strategy.enable_challenge",
        if issued { "issued" } else { "refused" },
        Some(&id),
    )
    .await;
    if !issued {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many open enable challenges",
        ));
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EnableReq {
    signature: String,
}

/// Enable a strategy with the owner's device signature over
/// `meme_strategy_enable_message` for the device's open challenge. The
/// challenge is used up by this call, whatever its outcome. Audited.
pub(crate) async fn enable(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<EnableReq>,
) -> ApiResult {
    strategy_id(&id)?;
    throttle(&state, &authed)?;
    if !validation::is_hex_of_len(&req.signature, validation::ED25519_SIGNATURE_HEX_LEN) {
        return Err(bad_request("invalid signature"));
    }
    let signature = hex::decode(&req.signature).map_err(|_| bad_request("invalid signature"))?;
    let now = OffsetDateTime::now_utc();
    let challenge = state
        .memecoin
        .enable_challenges
        .lock()
        .ok()
        .and_then(|mut open| open.take(authed.device.id, &id, now));
    let audit = |outcome: &'static str| {
        record_security_event(
            &state,
            Some(authed.device.id),
            "memecoin.strategy.enable",
            outcome,
            Some(&id),
        )
    };
    let Some(challenge) = challenge else {
        audit("no_challenge").await;
        return Err(error(
            StatusCode::CONFLICT,
            "no open enable challenge; request a new one",
        ));
    };
    let message = jarvis_client_core::meme_strategy_enable_message(
        &id,
        challenge.version,
        &challenge.config_sha256,
        &challenge.nonce,
        authed.device.id,
    )
    .map_err(|_| bad_request("invalid strategy id"))?;
    let verified = identity::verify_device_signature(
        &state.db,
        authed.user.id,
        authed.device.id,
        &message,
        &signature,
    )
    .await;
    if verified.is_err() {
        audit("denied").await;
        return Err(unauthorized());
    }
    // Still the version and parameters the device signed for.
    let device = authed.device.id.to_string();
    let enabled = async {
        let strategy = find_strategy(&state, &id).await?;
        let params = active_params(&state, &strategy).await?;
        let same = strategy.active_version == challenge.version
            && <[u8; 32]>::from(Sha256::digest(params.as_bytes())) == challenge.config_sha256;
        if !same {
            return Ok(false);
        }
        let enable = store::enable_strategy_version(&state.db, &id, challenge.version, &device);
        bounded(enable).await
    }
    .await;
    let enabled = match enabled {
        Ok(enabled) => enabled,
        Err(failure) => {
            audit("store_failed").await;
            return Err(failure);
        }
    };
    if !enabled {
        audit("changed").await;
        return Err(error(
            StatusCode::CONFLICT,
            "strategy changed; request a new challenge",
        ));
    }
    state.memecoin.clear_disabled(&id);
    audit("enabled").await;
    announce(
        Some(&state.realtime),
        Event::MemeStrategyChanged {
            strategy_id: id.clone(),
            version: challenge.version,
            enabled: true,
        },
    );
    Ok(Json(json!({
        "strategy_id": id,
        "version": challenge.version,
        "enabled": true,
    })))
}

/// The strategy agent's decisions, newest first. Rationales are display
/// only.
pub(crate) async fn decisions(
    _authed: Authed,
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> ApiResult {
    let rows = bounded(store::recent_decisions(&state.db, limit(&query)?)).await?;
    Ok(Json(json!({
        "decisions": rows.into_iter().map(|d| json!({
            "id": d.id,
            "at": ts(d.at),
            "call_id": d.call_id,
            "kind": d.kind,
            "strategy_version": d.strategy_version,
            "mint": d.mint,
            "size_bps": d.size_bps,
            "reason_code": d.reason_code,
            "rationale": d.rationale,
            "model": d.model,
            "input_tokens": d.input_tokens,
            "output_tokens": d.output_tokens,
        })).collect::<Vec<_>>(),
    })))
}
