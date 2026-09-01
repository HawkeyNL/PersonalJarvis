//! System introspection: this month's LLM spend vs. budget (ADR-027), the
//! resource/agent registry (host + brains + model catalog), and self-development.
//! Self-improve is **advisory only** — Jarvis reads its own ecosystem and returns
//! concrete proposals but never acts; carrying one out goes through the approval
//! gate, and the Core + `Jarvis.md` stay owner-only, manual.

use std::sync::atomic::Ordering;

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use jarvis_registry as registry;
use jarvis_selfdev as selfdev;
use jarvis_usage as usage;

use crate::audit::record_security_event;
use crate::error::bad_request;
use crate::metering::record_usage;
use crate::validation;
use crate::{AppState, Authed};

#[derive(Debug, Deserialize)]
pub(crate) struct BrainPreferenceReq {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BrainPreferenceRow {
    provider: Option<String>,
    model: Option<String>,
}

/// Owner-visible default conversational brain. `null/null` means Auto. This
/// is deliberately application state, not a protected persona/model-policy
/// mutation: selection remains constrained by the root-owned allowlist.
pub(crate) async fn system_brain(authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let preference = brain_preference(&state.db, authed.user.id).await;
    let enabled: Vec<Value> = state
        .model_policy
        .models
        .iter()
        .filter(|entry| entry.enabled)
        .map(|entry| json!({"provider": entry.provider, "model": entry.model, "source": entry.source}))
        .collect();
    Json(json!({"default": preference, "enabled_models": enabled}))
}

pub(crate) async fn system_brain_set(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<BrainPreferenceReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_brain_selection(&state, req.provider.as_deref(), req.model.as_deref())?;
    state.db.query(
        "UPSERT owner_brain_preferences:$id SET id = $id, user_id = $user_id, provider = $provider, model = $model, updated_at = time::now() RETURN NONE",
    ).bind(json!({"id": authed.user.id.to_string(), "user_id": authed.user.id.to_string(), "provider": req.provider, "model": req.model})).await
        .map_err(|_| internal_error())?;
    record_security_event(
        &state,
        Some(authed.device.id),
        "owner_brain_preference",
        "changed",
        Some(if req.provider.is_some() {
            "explicit"
        } else {
            "auto"
        }),
    )
    .await;
    Ok(Json(
        json!({"status":"updated", "default": brain_preference(&state.db, authed.user.id).await}),
    ))
}

pub(crate) fn validate_brain_selection(
    state: &AppState,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    match (provider, model) {
        (None, None) => Ok(()),
        (Some(provider), Some(model)) if state.model_policy.allows(provider, model) => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"model is not owner-enabled"})),
        )),
    }
}

pub(crate) async fn brain_preference(db: &jarvis_store::Database, user_id: Uuid) -> Value {
    let row: Option<BrainPreferenceRow> = db
        .query(
            "SELECT provider, model FROM owner_brain_preferences WHERE user_id = $user_id LIMIT 1",
        )
        .bind(json!({"user_id": user_id.to_string()}))
        .await
        .ok()
        .and_then(|mut response| response.take(0).ok())
        .flatten();
    match row {
        Some(BrainPreferenceRow {
            provider: Some(provider),
            model: Some(model),
        }) => json!({"mode":"pinned","provider":provider,"model":model}),
        _ => json!({"mode":"auto"}),
    }
}

fn internal_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"internal error"})),
    )
}

/// Forward one typed, signed owner operation to the *local* root broker. The
/// bearer session only identifies the caller; it cannot authorize anything:
/// the broker independently validates the Ed25519 signature, owner device,
/// exact canonical payload, expiry and one-time request ID before mutation.
pub(crate) async fn system_privileged_config(
    authed: Authed,
    State(state): State<AppState>,
    Json(request): Json<jarvis_privileged::SignedRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if request.user_id != authed.user.id || request.device_id != authed.device.id {
        record_security_event(
            &state,
            Some(authed.device.id),
            "privileged_config",
            "denied",
            Some("principal mismatch"),
        )
        .await;
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"privileged operation denied"})),
        ));
    }
    request
        .message()
        .map_err(|_| bad_request("invalid privileged approval"))?;
    request
        .reject_if_expired(time::OffsetDateTime::now_utc())
        .map_err(|_| {
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"privileged approval expired"})),
            )
        })?;
    let Some(socket) = state.privileged_broker_socket.as_deref() else {
        record_security_event(
            &state,
            Some(authed.device.id),
            "privileged_config",
            "denied",
            Some("broker unavailable"),
        )
        .await;
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"privileged configuration unavailable"})),
        ));
    };
    let result = forward_to_broker(socket, &request).await;
    match result {
        Ok(()) => {
            record_security_event(
                &state,
                Some(authed.device.id),
                "privileged_config",
                "forwarded",
                Some(request.operation.action()),
            )
            .await;
            Ok(Json(json!({"status":"accepted","restart_required":true})))
        }
        Err(()) => {
            record_security_event(
                &state,
                Some(authed.device.id),
                "privileged_config",
                "denied",
                Some(request.operation.action()),
            )
            .await;
            Err((
                StatusCode::FORBIDDEN,
                Json(json!({"error":"privileged operation denied"})),
            ))
        }
    }
}

async fn forward_to_broker(
    socket: &str,
    request: &jarvis_privileged::SignedRequest,
) -> Result<(), ()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let (read, mut write) = stream.into_split();
    let encoded = serde_json::to_vec(&json!({"request": request})).map_err(|_| ())?;
    if encoded.len() > 16 * 1024 {
        return Err(());
    }
    write.write_all(&encoded).await.map_err(|_| ())?;
    write.write_all(b"\n").await.map_err(|_| ())?;
    let mut reply = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        BufReader::new(read).read_line(&mut reply),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    (reply.trim() == r#"{"status":"applied"}"#)
        .then_some(())
        .ok_or(())
}

/// This month's LLM spend vs. the budget, with a per-backend breakdown (ADR-027).
pub(crate) async fn usage_value(state: &AppState) -> Result<Value, jarvis_store::StoreError> {
    let spent_eur = state.spent_cents.load(Ordering::Relaxed) as f64 / 100.0;
    let budget_eur = state.budget_cents as f64 / 100.0;
    let reservation = state.budget_book.snapshot();
    let mut statistics = usage::month_statistics(&state.db).await?;
    statistics.by_model.truncate(250);
    let by_backend: Vec<Value> = statistics
        .by_backend
        .into_iter()
        .map(|row| {
            json!({
                "backend": row.backend,
                "spent_eur": row.totals.cost_eur,
                "requests": row.totals.requests,
                "input_tokens": row.totals.input_tokens,
                "output_tokens": row.totals.output_tokens,
                "cache_read_tokens": row.totals.cache_read_tokens,
                "cache_write_tokens": row.totals.cache_write_tokens,
                "total_tokens": row.totals.total_tokens,
            })
        })
        .collect();
    let by_model: Vec<Value> = statistics
        .by_model
        .into_iter()
        .map(|row| {
            json!({
                "backend": row.backend,
                "model": row.model,
                "spent_eur": row.totals.cost_eur,
                "requests": row.totals.requests,
                "input_tokens": row.totals.input_tokens,
                "output_tokens": row.totals.output_tokens,
                "cache_read_tokens": row.totals.cache_read_tokens,
                "cache_write_tokens": row.totals.cache_write_tokens,
                "total_tokens": row.totals.total_tokens,
            })
        })
        .collect();
    let daily: Vec<Value> = statistics
        .daily
        .into_iter()
        .map(|row| {
            json!({
                "day": row.day,
                "spent_eur": row.totals.cost_eur,
                "requests": row.totals.requests,
                "input_tokens": row.totals.input_tokens,
                "output_tokens": row.totals.output_tokens,
                "cache_read_tokens": row.totals.cache_read_tokens,
                "cache_write_tokens": row.totals.cache_write_tokens,
                "total_tokens": row.totals.total_tokens,
            })
        })
        .collect();
    Ok(json!({
        "period": "current_calendar_month",
        "budget_eur": budget_eur,
        "spent_eur": spent_eur,
        "remaining_eur": (budget_eur - spent_eur).max(0.0),
        "over_budget": spent_eur >= budget_eur,
        "reserved_eur": reservation.reserved_cents as f64 / 100.0,
        "remaining_hard_eur": reservation.remaining_hard_cents as f64 / 100.0,
        "above_soft_budget": reservation.above_soft_limit,
        "requests": statistics.totals.requests,
        "input_tokens": statistics.totals.input_tokens,
        "output_tokens": statistics.totals.output_tokens,
        "cache_read_tokens": statistics.totals.cache_read_tokens,
        "cache_write_tokens": statistics.totals.cache_write_tokens,
        "total_tokens": statistics.totals.total_tokens,
        "by_backend": by_backend,
        "by_model": by_model,
        "daily": daily,
        "pricing": {
            "source": state.pricing_registry.source,
            "updated_at": state.pricing_registry.updated_at,
        },
    }))
}

pub(crate) async fn system_usage(
    _authed: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    usage_value(&state)
        .await
        .map(Json)
        .map_err(|_| internal_error())
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
