//! Memecoin PAPER trader, read-only for the owner. The only write is the
//! pause, which can only reduce activity; resuming is configuration-only.
//! Money is returned as decimal strings.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::audit::record_security_event;
use crate::error::bad_request;
use crate::memecoin::store;
use crate::{AppState, Authed};

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
    });
    if let Some(view) = handle.view() {
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
