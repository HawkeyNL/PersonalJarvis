//! System introspection: this month's LLM spend vs. budget (ADR-027), the
//! resource/agent registry (host + brains + model catalog), and self-development.
//! Self-improve is **advisory only** — Jarvis reads its own ecosystem and returns
//! concrete proposals but never acts; carrying one out goes through the approval
//! gate, and the Core + `Jarvis.md` stay owner-only, manual.

use std::sync::atomic::Ordering;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use jarvis_registry as registry;
use jarvis_selfdev as selfdev;
use jarvis_usage as usage;

use crate::error::bad_request;
use crate::metering::record_usage;
use crate::validation;
use crate::{AppState, Authed};

/// This month's LLM spend vs. the budget, with a per-backend breakdown (ADR-027).
pub(crate) async fn system_usage(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let spent_eur = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget_eur = state.budget_cents as f64 / 100.0;
    let reservation = state.budget_book.snapshot();
    let breakdown = usage::month_breakdown(&state.db).await.unwrap_or_default();
    let by_backend: Vec<Value> = breakdown
        .into_iter()
        .map(|(backend, eur)| json!({ "backend": backend, "spent_eur": eur }))
        .collect();
    Json(json!({
        "budget_eur": budget_eur,
        "spent_eur": spent_eur,
        "remaining_eur": (budget_eur - spent_eur).max(0.0),
        "over_budget": spent_eur >= budget_eur,
        "reserved_eur": reservation.reserved_cents as f64 / 100.0,
        "remaining_hard_eur": reservation.remaining_hard_cents as f64 / 100.0,
        "above_soft_budget": reservation.above_soft_limit,
        "by_backend": by_backend,
    }))
}

/// Jarvis' resource/agent registry — available brains + cost + the host it runs
/// on (ADR-027 stage 3). Cached from startup; POST `/refresh` re-probes.
pub(crate) async fn system_registry(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let value = state
        .registry
        .read()
        .map(|reg| serde_json::to_value(&*reg).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|_| json!({}));
    Json(value)
}

pub(crate) async fn system_registry_refresh(
    _authed: Authed,
    State(state): State<AppState>,
) -> Json<Value> {
    let fresh = registry::collect(&state.registry_input).await;
    if let Ok(mut reg) = state.registry.write() {
        *reg = fresh.clone();
    }
    Json(serde_json::to_value(&fresh).unwrap_or_else(|_| json!({})))
}

/// Owner-authenticated, non-secret view of the exact model allowlist.  Mutation
/// is intentionally root-operated for now; a bearer session alone must not
/// rewrite Home Node policy or activate paid models.
pub(crate) async fn system_model_policy(
    _authed: Authed,
    State(state): State<AppState>,
) -> Json<Value> {
    Json(json!({
        "version": state.model_policy.version,
        "models": state.model_policy.models,
        "mutation": "root-operated: sudo jarvis-models enable|disable",
    }))
}

#[derive(Deserialize)]
pub(crate) struct BudgetPreflightReq {
    provider: String,
    model: String,
    input_tokens_per_call: u32,
    output_tokens_per_call: u32,
    calls: u32,
}

/// Bounded owner-visible cost preflight for a planned long task.  It neither
/// executes work nor enables models; a disabled model cannot be probed into
/// becoming eligible through this endpoint.
pub(crate) async fn system_budget_preflight(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<BudgetPreflightReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.provider.len() > 64 || req.model.len() > 256 || req.calls == 0 || req.calls > 10_000 {
        return Err(bad_request("invalid budget preflight"));
    }
    if !state.model_policy.allows(&req.provider, &req.model) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "model is not owner-enabled" })),
        ));
    }
    let estimate = usage::estimate_task_cost_with_registry(
        &state.pricing_registry,
        &req.provider,
        &req.model,
        req.input_tokens_per_call,
        req.output_tokens_per_call,
        req.calls,
        state.eur_per_usd,
    );
    let budget = state.budget_book.snapshot();
    let high_cents = (estimate.high_eur * 100.0).ceil().max(0.0) as u64;
    let recommendation = if high_cents > budget.remaining_hard_cents {
        "do_not_start"
    } else if budget.above_soft_limit {
        "proceed_cost_consciously"
    } else {
        "proceed"
    };
    Ok(Json(json!({
        "provider": req.provider,
        "model": req.model,
        "calls": req.calls,
        "estimate": estimate,
        "remaining_hard_eur": budget.remaining_hard_cents as f64 / 100.0,
        "recommendation": recommendation,
        "note": "Estimate only; a long-running task requires a bounded reservation and checkpoints before execution.",
    })))
}

#[derive(Deserialize)]
pub(crate) struct SelfImproveReq {
    /// Optional area to focus the advice on (e.g. "goedkopere modellen").
    #[serde(default)]
    focus: Option<String>,
}

/// Jarvis proposes improvements to ITSELF (ADR-029 fase 4d) — **advisory only**.
/// It reads its own ecosystem (registry + budget + agent capabilities) and returns
/// concrete proposals; it never acts. Carrying one out goes through the approval
/// gate (4b/4c); the Core and `Jarvis.md` stay owner-only, manual. On request only.
pub(crate) async fn system_self_improve(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<SelfImproveReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if let Some(focus) = req.focus.as_deref() {
        if focus.len() > validation::MAX_FOCUS_LEN {
            return Err(bad_request("focus too long"));
        }
    }
    let ecosystem = match state.registry.read() {
        Ok(reg) => render_ecosystem(&reg, state.agent_enabled, state.agent_sandbox.is_some()),
        Err(_) => "(ecosysteem tijdelijk niet leesbaar)".to_string(),
    };
    let spent_eur = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget_eur = state.budget_cents as f64 / 100.0;
    match selfdev::propose(
        &state.llm,
        &state.jarvis_system,
        &ecosystem,
        budget_eur,
        spent_eur,
        req.focus.as_deref(),
    )
    .await
    {
        Ok(report) => {
            for reply in &report.calls {
                record_usage(&state, reply).await;
            }
            let proposals: Vec<Value> = report
                .proposals
                .iter()
                .map(|p| {
                    json!({
                        "title": p.title,
                        "category": p.category,
                        "rationale": p.rationale,
                        "cost": p.cost,
                        "requires_approval": p.requires_approval,
                        "steps": p.steps,
                    })
                })
                .collect();
            Ok(Json(json!({
                "summary": report.summary,
                "proposals": proposals,
                "note": "Jarvis stelt alleen voor — uitvoeren gaat via jouw goedkeuring (4b/4c); \
                         de Core en Jarvis.md blijven handmatig, alleen door jou.",
            })))
        }
        Err(e) => {
            tracing::warn!(error = %e, "self-improve failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer je brein-config (router/keys/Ollama)",
                })),
            ))
        }
    }
}

/// Render the registry into a compact text snapshot for the self-dev advisor.
/// Shared with the MCP `jarvis_status` tool.
pub(crate) fn render_ecosystem(
    reg: &registry::Registry,
    agent_enabled: bool,
    has_workspace: bool,
) -> String {
    let h = &reg.host;
    let mut s = format!(
        "Host: {} {}, {} ({} cores), {:.1} GB RAM, GPU: {}\nActief brein: {}\n",
        h.os, h.arch, h.cpu, h.cpu_cores, h.mem_total_gb, h.gpu, reg.active_brain
    );
    s.push_str("\nBreinen:\n");
    for b in &reg.brains {
        s.push_str(&format!(
            "- {} [{}] beschikbaar: {} — {}\n",
            b.label,
            enum_str(&b.cost),
            yesno(b.available),
            b.note
        ));
    }
    s.push_str("\nModel-catalogus:\n");
    for m in &reg.models {
        s.push_str(&format!(
            "- {} ({}, {}, {}) beschikbaar: {}\n",
            m.id,
            m.backend,
            enum_str(&m.class),
            enum_str(&m.cost),
            yesno(m.available)
        ));
    }
    s.push_str("\nTools op de host:\n");
    for t in &reg.software {
        let v = t
            .version
            .as_deref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "- {}: {}{}\n",
            t.name,
            if t.present { "aanwezig" } else { "afwezig" },
            v
        ));
    }
    s.push_str(&format!(
        "\nAgent-capabilities: agent {}, werkmap {}\n",
        if agent_enabled { "AAN" } else { "uit" },
        if has_workspace {
            "geconfigureerd"
        } else {
            "geen"
        }
    ));
    s
}

/// Serialize a small lowercase-tagged enum (ModelClass/ModelCost/CostTier) to its
/// string form for display.
fn enum_str<T: serde::Serialize>(t: &T) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn yesno(b: bool) -> &'static str {
    if b {
        "ja"
    } else {
        "nee"
    }
}
