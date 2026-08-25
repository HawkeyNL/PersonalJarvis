//! Portfolio holdings: the user's manually-tracked positions with cost basis and
//! allocation weights. All handlers are owner-scoped via [`Authed`] and never
//! touch the live broker — that is read-only and lives in [`super::broker`].

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use jarvis_portfolio as portfolio;

use crate::error::{bad_request, portfolio_err};
use crate::validation;
use crate::{AppState, Authed};

/// List the authenticated user's holdings with cost basis and allocation.
pub(crate) async fn get_holdings(
    authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let holdings = portfolio::list_holdings(&state.db, authed.user.id)
        .await
        .map_err(portfolio_err)?;
    let total: Decimal = holdings.iter().map(|h| h.cost_basis()).sum();
    let hundred = Decimal::from(100);
    let items: Vec<Value> = holdings
        .iter()
        .map(|h| {
            let cost = h.cost_basis();
            let weight = if total.is_zero() {
                Decimal::ZERO
            } else {
                (cost / total) * hundred
            };
            json!({
                "id": h.id,
                "symbol": h.symbol,
                "quantity": h.quantity.normalize().to_string(),
                "avg_cost": h.avg_cost.normalize().to_string(),
                "currency": h.currency,
                "cost_basis": cost.normalize().to_string(),
                "weight_pct": weight.round_dp(1).normalize().to_string(),
            })
        })
        .collect();
    Ok(Json(json!({
        "holdings": items,
        "total_cost": total.normalize().to_string(),
    })))
}

#[derive(Deserialize)]
pub(crate) struct AddHoldingReq {
    symbol: String,
    quantity: Decimal,
    avg_cost: Decimal,
    currency: Option<String>,
}

/// Add a holding for the authenticated user.
pub(crate) async fn add_holding(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<AddHoldingReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let symbol = req.symbol.trim().to_uppercase();
    if symbol.is_empty() {
        return Err(bad_request("symbol is required"));
    }
    if symbol.len() > validation::MAX_SYMBOL_LEN {
        return Err(bad_request("symbol too long"));
    }
    if req.quantity <= Decimal::ZERO {
        return Err(bad_request("quantity must be positive"));
    }
    if req.avg_cost < Decimal::ZERO {
        return Err(bad_request("avg_cost must be non-negative"));
    }
    let currency = req.currency.as_deref().unwrap_or("EUR");
    if !validation::bounded_text(currency, validation::MAX_CURRENCY_LEN) {
        return Err(bad_request("invalid currency"));
    }
    let holding = portfolio::add_holding(
        &state.db,
        authed.user.id,
        &symbol,
        req.quantity,
        req.avg_cost,
        currency,
    )
    .await
    .map_err(portfolio_err)?;
    Ok(Json(json!({ "id": holding.id, "symbol": holding.symbol })))
}

/// Delete one of the authenticated user's holdings.
pub(crate) async fn remove_holding(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if portfolio::delete_holding(&state.db, authed.user.id, id)
        .await
        .map_err(portfolio_err)?
    {
        Ok(Json(json!({ "status": "deleted" })))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "holding not found" })),
        ))
    }
}
