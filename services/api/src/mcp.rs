//! MCP: Jarvis' read-only tools over Streamable HTTP (ADR-031).
//!
//! A minimal, defensive MCP server so Claude Code (and the owner's own Claude
//! tooling) can *read* Jarvis' portfolio, status and memory. Every call is
//! owner-scoped (the [`Authed`] extractor requires a valid session token) and
//! read-only — no secrets, no Core, no mutations, no trading.
//!
//! The protocol shape has churned across MCP versions, so this is deliberately
//! lenient: it answers both the classic `initialize` and the newer
//! `server/discover` handshake, echoes the client's requested protocol version,
//! and returns union results that satisfy either. The stable core — `tools/call`
//! → `{content:[{type:"text"}], isError}` — is identical everywhere.

use std::sync::atomic::Ordering;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use rust_decimal::Decimal;
use serde_json::{json, Value};

use jarvis_portfolio as portfolio;

use crate::{render_ecosystem, AppState, Authed};

pub(crate) async fn mcp_endpoint(
    authed: Authed,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // DNS-rebinding guard: reject a browser Origin that isn't local. Non-browser
    // clients (Claude Code) send no Origin and are allowed.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        if !is_local_origin(origin) {
            return (StatusCode::FORBIDDEN, "bad origin").into_response();
        }
    }

    let id = body.get("id").cloned();
    // Notifications carry no id and expect no response body.
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let outcome: Result<Value, (i64, String)> = match method {
        "initialize" | "server/discover" => Ok(mcp_handshake(&body)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "resultType": "complete", "tools": mcp_tools() })),
        "tools/call" => {
            let name = body
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = body
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match mcp_call(&state, &authed, name, &args).await {
                Ok(text) => Ok(json!({
                    "resultType": "complete",
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                })),
                Err(ToolErr::Unknown) => Err((-32601, format!("onbekende tool: {name}"))),
                Err(ToolErr::Failed(msg)) => Ok(json!({
                    "resultType": "complete",
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true,
                })),
            }
        }
        other => Err((-32601, format!("methode niet gevonden: {other}"))),
    };

    let payload = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Json(payload).into_response()
}

/// A union handshake result that satisfies both `initialize` (classic) and
/// `server/discover`, echoing the client's requested protocol version.
fn mcp_handshake(body: &Value) -> Value {
    let requested = body
        .pointer("/params/protocolVersion")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("params")
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("2025-06-18");
    let info = json!({ "name": "jarvis", "version": env!("CARGO_PKG_VERSION") });
    json!({
        "resultType": "complete",
        "protocolVersion": requested,
        "supportedVersions": [requested],
        "capabilities": { "tools": {} },
        "serverInfo": info,
        "_meta": { "io.modelcontextprotocol/serverInfo": info },
        "instructions": "Read-only tools van Jarvis: portfolio, status en geheugen.",
    })
}

/// The read-only tool catalog. Definitions are static; execution is owner-scoped.
fn mcp_tools() -> Value {
    json!([
        {
            "name": "portfolio_summary",
            "description": "Jarvis' portfolio: posities, kostenbasis en allocatie (alleen-lezen).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "jarvis_status",
            "description": "Jarvis' ecosysteem: host, breinen, model-catalogus en maandbudget (alleen-lezen).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "recent_conversations",
            "description": "Recente gesprekstitels — Jarvis' geheugen, nieuwste eerst (alleen-lezen).",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "description": "max aantal (1-50)" } },
                "additionalProperties": false
            }
        }
    ])
}

enum ToolErr {
    Unknown,
    Failed(String),
}

/// Dispatch a read-only tool call, owner-scoped. Never mutates or exposes secrets.
async fn mcp_call(
    state: &AppState,
    authed: &Authed,
    name: &str,
    args: &Value,
) -> Result<String, ToolErr> {
    match name {
        "portfolio_summary" => mcp_portfolio(state, authed).await,
        "jarvis_status" => Ok(mcp_status(state)),
        "recent_conversations" => mcp_recent_conversations(state, authed, args).await,
        _ => Err(ToolErr::Unknown),
    }
}

async fn mcp_portfolio(state: &AppState, authed: &Authed) -> Result<String, ToolErr> {
    let holdings = portfolio::list_holdings(&state.db, authed.user.id)
        .await
        .map_err(|e| ToolErr::Failed(format!("portfolio niet leesbaar: {e}")))?;
    if holdings.is_empty() {
        return Ok("Geen posities in het portfolio.".to_string());
    }
    let total: Decimal = holdings.iter().map(|h| h.cost_basis()).sum();
    let hundred = Decimal::from(100);
    let mut s = format!(
        "Portfolio — {} posities, totale kostenbasis {}:\n",
        holdings.len(),
        total.normalize()
    );
    for h in &holdings {
        let cost = h.cost_basis();
        let weight = if total.is_zero() {
            Decimal::ZERO
        } else {
            (cost / total * hundred).round_dp(1)
        };
        s.push_str(&format!(
            "- {}: {} @ {} {} = {} ({}%)\n",
            h.symbol,
            h.quantity.normalize(),
            h.avg_cost.normalize(),
            h.currency,
            cost.normalize(),
            weight.normalize()
        ));
    }
    Ok(s)
}

fn mcp_status(state: &AppState) -> String {
    let eco = match state.registry.read() {
        Ok(reg) => render_ecosystem(&reg, state.agent_enabled, state.agent_sandbox.is_some()),
        Err(_) => "(ecosysteem tijdelijk niet leesbaar)".to_string(),
    };
    let spent = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget = state.budget_cents as f64 / 100.0;
    format!(
        "{eco}\nBudget: €{spent:.2} van €{budget:.2} gebruikt deze maand (€{:.2} over).",
        (budget - spent).max(0.0)
    )
}

async fn mcp_recent_conversations(
    state: &AppState,
    authed: &Authed,
    args: &Value,
) -> Result<String, ToolErr> {
    let limit = args
        .get("limit")
        .and_then(|l| l.as_i64())
        .unwrap_or(10)
        .clamp(1, 50);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, to_char(updated_at, 'YYYY-MM-DD HH24:MI') \
         FROM conversations WHERE user_id = $1 ORDER BY updated_at DESC LIMIT $2",
    )
    .bind(authed.user.id)
    .bind(limit)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ToolErr::Failed(format!("gesprekken niet leesbaar: {e}")))?;
    if rows.is_empty() {
        return Ok("Nog geen gesprekken.".to_string());
    }
    let mut s = String::from("Recente gesprekken:\n");
    for (title, updated) in rows {
        s.push_str(&format!("- {title} ({updated})\n"));
    }
    Ok(s)
}

/// Only local origins are allowed (DNS-rebinding protection); a client that sends
/// no Origin header (Claude Code) is allowed by the caller before this is reached.
fn is_local_origin(origin: &str) -> bool {
    origin.starts_with("http://localhost")
        || origin.starts_with("http://127.0.0.1")
        || origin.starts_with("https://localhost")
        || origin == "null"
}
