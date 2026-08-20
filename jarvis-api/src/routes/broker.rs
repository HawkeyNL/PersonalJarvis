//! IBKR broker bridge — strictly read-only. Reachability/auth status and
//! positions are surfaced from the Client Portal Gateway; nothing here can place
//! or modify an order. Trading stays a human, out-of-band decision by design.

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};

use jarvis_ibkr as ibkr;

use crate::{AppState, Authed};

pub(crate) fn ibkr_down() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "IBKR gateway not connected",
            "hint": "start de Client Portal Gateway en log in (paper of live)",
        })),
    )
}

/// IBKR gateway reachability + auth status (read-only).
pub(crate) async fn ibkr_status(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let client = match ibkr::IbkrClient::new(&state.ibkr_gateway_url) {
        Ok(c) => c,
        Err(_) => return Json(json!({ "reachable": false, "authenticated": false })),
    };
    match client.auth_status().await {
        Ok(s) => Json(json!({
            "reachable": true,
            "authenticated": s.authenticated,
            "connected": s.connected,
        })),
        Err(_) => Json(json!({
            "reachable": false,
            "authenticated": false,
            "hint": "start de IBKR Client Portal Gateway en log in",
        })),
    }
}

/// IBKR positions for an account (read-only). `?account=` selects the account;
/// otherwise the first account in the session is used.
pub(crate) async fn ibkr_positions(
    _authed: Authed,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client = ibkr::IbkrClient::new(&state.ibkr_gateway_url).map_err(|_| ibkr_down())?;
    let status = client.auth_status().await.map_err(|_| ibkr_down())?;
    if !status.authenticated {
        return Err(ibkr_down());
    }
    let account = match params.get("account") {
        Some(a) => a.clone(),
        None => client
            .accounts()
            .await
            .map_err(|_| ibkr_down())?
            .into_iter()
            .next()
            .map(|a| a.account_id)
            .ok_or_else(ibkr_down)?,
    };
    let positions = client.positions(&account).await.map_err(|_| ibkr_down())?;
    let positions = serde_json::to_value(&positions).unwrap_or_else(|_| json!([]));
    Ok(Json(json!({ "account": account, "positions": positions })))
}
