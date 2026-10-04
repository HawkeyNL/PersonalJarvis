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

use jarvis_llm as llm;
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
        .snapshot()
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
    // `table:$param` is not valid SurrealQL; bind the record id instead.
    let user_id = authed.user.id.to_string();
    state.db.query(
        "UPSERT $record SET id = $id, user_id = $user_id, provider = $provider, model = $model, updated_at = time::now() RETURN NONE",
    ).bind(("record", surrealdb::RecordId::from_table_key("owner_brain_preferences", user_id.as_str())))
        .bind(json!({"id": user_id, "user_id": user_id, "provider": req.provider, "model": req.model})).await
        .map_err(|_| internal_error())?
        .check()
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
    brain_selection(
        &state.model_policy,
        &state.model_routing.snapshot(),
        provider,
        model,
    )
}

fn brain_selection(
    policy: &llm::LiveModelPolicy,
    routing: &llm::RoutingSnapshot,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<(), (StatusCode, Json<Value>)> {
    match (provider, model) {
        (None, None) => Ok(()),
        (Some(provider), Some(_)) if routing.refuses_provider(provider) => Err(paid_api_off()),
        (Some(provider), Some(model)) if policy.allows(provider, model) => Ok(()),
        _ => Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"model is not owner-enabled"})),
        )),
    }
}

/// A metered brain selected while the owner turned paid APIs off.
pub(crate) fn paid_api_off() -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "paid API is off",
            "hint": "choose a subscription or local model, or Auto",
        })),
    )
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
    let _mutation = state.model_control.mutation.try_lock().map_err(|_| {
        (
            StatusCode::CONFLICT,
            Json(json!({"error":"another model change is in progress"})),
        )
    })?;
    let (provider, model, enabled, expected_policy_sha256) = match &request.operation {
        jarvis_privileged::Operation::ModelSetEnabled {
            provider,
            model,
            enabled,
            expected_policy_sha256,
        } => (provider, model, *enabled, expected_policy_sha256),
        jarvis_privileged::Operation::ModelRoutingSet {
            routing,
            expected_routing_sha256,
        } => {
            return set_routing(
                &state,
                authed.device.id,
                socket,
                &request,
                routing,
                expected_routing_sha256,
            )
            .await;
        }
    };
    let (expected, hash) = state
        .model_control
        .read()
        .map_err(|_| model_activation_error())?;
    if enabled && !state.model_control.can_enable(provider, model) {
        return Err((
            StatusCode::CONFLICT,
            Json(
                json!({"error":"selected Hugging Face route is unavailable; refresh the trusted catalog and restart Core"}),
            ),
        ));
    }
    if hash != *expected_policy_sha256 || expected != state.model_policy.snapshot() {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"model policy changed; refresh before approving"})),
        ));
    }
    let result = forward_to_broker(socket, &request).await;
    match result {
        Ok(()) => {
            let activated = state
                .model_control
                .read()
                .map_err(|_| ())
                .and_then(|(verified, _)| {
                    state
                        .model_policy
                        .activate_verified_toggle(&expected, verified, provider, model, enabled)
                        .map_err(|_| ())
                });
            if activated.is_err() {
                state.model_policy.suspend();
                return Err(model_activation_error());
            }
            record_security_event(
                &state,
                Some(authed.device.id),
                "privileged_config",
                "forwarded",
                Some(request.operation.action()),
            )
            .await;
            Ok(Json(json!({"status":"active","restart_required":false})))
        }
        Err(()) => {
            // A lost reply is not proof that the broker did not commit. Never
            // keep an old grant live when protected readback is inconclusive.
            if !state
                .model_control
                .read()
                .is_ok_and(|(policy, _)| policy == expected)
            {
                state.model_policy.suspend();
                return Err(model_activation_error());
            }
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

/// Signed full replacement of `routing.json`. The approval is bound to the
/// exact file bytes; Core activates only the signed document it reads back,
/// and only if the live routing did not change meanwhile.
async fn set_routing(
    state: &AppState,
    device_id: Uuid,
    socket: &str,
    request: &jarvis_privileged::SignedRequest,
    routing: &llm::ModelRouting,
    expected_sha256: &str,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let live = state.model_routing.snapshot();
    let current = state.model_control.read_routing().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"model routing is unreadable; owner must repair it as root"})),
        )
    })?;
    if current.sha256 != expected_sha256 {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"model routing changed; refresh before approving"})),
        ));
    }
    if !routing.all_discovered(&state.model_policy.snapshot()) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"routing names a model that is not discovered"})),
        ));
    }
    let forwarded = forward_to_broker(socket, request).await;
    let readback = state.model_control.read_routing();
    let outcome = match forwarded {
        Ok(()) => activate_signed_routing(&state.model_routing, &live, routing, readback)
            .map_err(|()| routing_activation_error()),
        Err(()) => Err(refused_routing(
            &state.model_routing,
            expected_sha256,
            readback,
        )),
    };
    let event = if outcome.is_ok() {
        "forwarded"
    } else {
        "denied"
    };
    record_security_event(
        state,
        Some(device_id),
        "privileged_config",
        event,
        Some(request.operation.action()),
    )
    .await;
    outcome.map(|()| Json(json!({"status":"active","restart_required":false})))
}

/// Activate exactly the signed document read back from disk, if the live
/// routing is still what this request started from. Anything else (a
/// piggybacked change, an unreadable file, a concurrent swap) fails closed.
fn activate_signed_routing(
    live: &llm::LiveRouting,
    expected: &llm::RoutingSnapshot,
    signed: &llm::ModelRouting,
    readback: Result<crate::model_control::RoutingFile, &'static str>,
) -> Result<(), ()> {
    let verified = matches!(
        &readback,
        Ok(crate::model_control::RoutingFile { routing: Ok(Some(stored)), .. }) if stored == signed
    );
    let next = llm::RoutingSnapshot {
        routing: Some(signed.clone()),
        unavailable_reason: None,
    };
    if verified && live.swap(expected, next).is_ok() {
        return Ok(());
    }
    live.fail_closed("routing_activation_unverified");
    Err(())
}

/// A broker error or lost reply is not proof that nothing was written. Keep
/// the live routing only if the file is provably unchanged.
fn refused_routing(
    live: &llm::LiveRouting,
    expected_sha256: &str,
    readback: Result<crate::model_control::RoutingFile, &'static str>,
) -> (StatusCode, Json<Value>) {
    if readback.is_ok_and(|file| file.sha256 == expected_sha256) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"privileged operation denied"})),
        );
    }
    live.fail_closed("routing_activation_unverified");
    routing_activation_error()
}

fn routing_activation_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            json!({"error":"model routing could not be verified; paid APIs are off until the owner verifies routing and restarts Core"}),
        ),
    )
}

fn model_activation_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(
            json!({"error":"model activation could not be verified; owner must verify policy and restart Core"}),
        ),
    )
}

async fn forward_to_broker(
    socket: &str,
    request: &jarvis_privileged::SignedRequest,
) -> Result<(), ()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    // The socket path alone is not authority. Verify the kernel-reported
    // root peer before sending even an action-bound approval to the broker.
    if stream.peer_cred().map_err(|_| ())?.uid() != 0 {
        return Err(());
    }
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
        BufReader::new(read.take(1025)).read_line(&mut reply),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    (reply.len() <= 1024 && reply.trim() == r#"{"status":"applied"}"#)
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
    statistics.failures_by_category.truncate(32);
    // Not-yet-instrumented dimensions are `null`, never a measured zero.
    let by_agent: Option<Vec<Value>> = if usage::AGENT_USAGE_INSTRUMENTED {
        let mut rows = usage::month_agent_statistics(&state.db).await?;
        rows.truncate(MAX_USAGE_AGENTS);
        Some(
            rows.iter()
                .map(|row| {
                    let mut value = agent_usage_value(&row.totals, row.last_used.as_deref());
                    value["agent_id"] = json!(row.agent_id);
                    value
                })
                .collect(),
        )
    } else {
        None
    };
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
                "failures": failures(&row.totals),
                "fallbacks": fallbacks(&row.totals),
                "latency_p50_ms": row.totals.latency_p50_ms,
                "latency_p95_ms": row.totals.latency_p95_ms,
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
                "failures": failures(&row.totals),
                "fallbacks": fallbacks(&row.totals),
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
        "failures": failures(&statistics.totals),
        "fallbacks": fallbacks(&statistics.totals),
        "latency_p50_ms": statistics.totals.latency_p50_ms,
        "latency_p95_ms": statistics.totals.latency_p95_ms,
        "by_agent": by_agent,
        "failures_by_category": usage::FAILURES_INSTRUMENTED.then_some(statistics.failures_by_category),
        "pricing": {
            "source": state.pricing_registry.source,
            "updated_at": state.pricing_registry.updated_at,
        },
    }))
}

const MAX_USAGE_AGENTS: usize = 100;

/// `null` ("not measured") until Core records failed calls.
fn failures(totals: &usage::UsageTotals) -> Option<u64> {
    usage::FAILURES_INSTRUMENTED.then_some(totals.failures)
}

/// `null` ("not measured") until the router reports its fallbacks.
fn fallbacks(totals: &usage::UsageTotals) -> Option<u64> {
    usage::FALLBACKS_INSTRUMENTED.then_some(totals.fallbacks)
}

/// One agent's monthly usage, shared by `/v1/system/usage` and `/v1/agents`.
pub(crate) fn agent_usage_value(totals: &usage::UsageTotals, last_used: Option<&str>) -> Value {
    json!({
        "requests": totals.requests,
        "input_tokens": totals.input_tokens,
        "output_tokens": totals.output_tokens,
        "total_tokens": totals.total_tokens,
        "spent_eur": totals.cost_eur,
        "failures": failures(totals),
        "fallbacks": fallbacks(totals),
        "latency_p50_ms": totals.latency_p50_ms,
        "latency_p95_ms": totals.latency_p95_ms,
        "last_used": last_used,
    })
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
    let mut value = state
        .registry
        .read()
        .map(|reg| serde_json::to_value(&*reg).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|_| json!({}));
    // Only sampled counters are fresh; inventory remains the startup/explicit
    // refresh snapshot. Do not rerun discovery or shell probes for live polling.
    let live = tokio::task::spawn_blocking(registry::live_host)
        .await
        .ok()
        .flatten();
    if let Some(object) = value.as_object_mut() {
        object.insert("live_host".into(), json!(live));
    }
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
    authed: Authed,
    State(state): State<AppState>,
) -> Json<Value> {
    // Serialize discovery reconciliation with signed mutations. Reading the
    // catalog cannot grant access: only disabled additions/metadata are allowed.
    let _guard = state.model_control.mutation.lock().await;
    let disk = state.model_control.read();
    if let Ok((stored, _)) = &disk {
        let _ = state.model_policy.refresh_discovery(stored.clone());
    }
    let policy = state.model_policy.snapshot();
    let unavailable_reason = if state.privileged_broker_socket.is_none() {
        Some("broker_unavailable")
    } else {
        match &disk {
            Err(_) => Some("policy_unavailable"),
            Ok((stored, _)) if *stored != policy => Some("policy_reload_required"),
            Ok(_) => None,
        }
    };
    let verified = disk.ok().filter(|(disk, _)| *disk == policy);
    let mutable = verified.is_some() && state.privileged_broker_socket.is_some();
    let routing = state.model_routing.snapshot();
    let (routing_unavailable_reason, routing_sha256) =
        crate::model_control::routing_status(&routing, state.model_control.read_routing());
    Json(json!({
        "version": policy.version,
        "models": crate::model_control::priced_models(&policy, &state.pricing_registry),
        "mutation_unavailable_reason": unavailable_reason,
        "mutation": if mutable { "device-signed-model-toggle-v1" } else { "unavailable" },
        "policy_sha256": verified.map(|(_, hash)| hash),
        "routing": routing.routing,
        "routing_sha256": routing_sha256,
        "routing_unavailable_reason": routing_unavailable_reason,
        "routing_mutation": if state.privileged_broker_socket.is_some() && routing_sha256.is_some() {
            "device-signed-model-route-v1"
        } else {
            "unavailable"
        },
        "user_id": authed.user.id,
        "device_id": authed.device.id,
        "server_time": time::OffsetDateTime::now_utc().unix_timestamp(),
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

#[cfg(test)]
mod model_broker_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn unprivileged_socket_cannot_receive_a_signed_approval() {
        if unsafe { libc::geteuid() } == 0 {
            return; // The ordinary CI suite runs unprivileged.
        }
        let fixture = tempfile::tempdir().unwrap();
        let socket = fixture.path().join("fake-broker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let captured = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                peer.read_to_end(&mut bytes),
            )
            .await
            .unwrap()
            .unwrap();
            bytes
        });
        let now = time::OffsetDateTime::now_utc();
        let request = jarvis_privileged::SignedRequest {
            request_id: uuid::Uuid::new_v4(),
            nonce_hex: "11".repeat(32),
            user_id: uuid::Uuid::new_v4(),
            device_id: uuid::Uuid::new_v4(),
            issued_at: now,
            expires_at: now + time::Duration::seconds(60),
            operation: jarvis_privileged::Operation::ModelSetEnabled {
                provider: "openai-api".into(),
                model: "fixture".into(),
                enabled: true,
                expected_policy_sha256: "22".repeat(32),
            },
            signature_hex: "33".repeat(64),
        };
        assert!(forward_to_broker(socket.to_str().unwrap(), &request)
            .await
            .is_err());
        assert!(captured.await.unwrap().is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model_control::RoutingFile;

    fn routing(raw: &str) -> llm::ModelRouting {
        llm::ModelRouting::parse(raw.as_bytes()).unwrap()
    }

    #[test]
    fn signed_routing_activates_only_the_exact_readback() {
        let signed = routing(r#"{"version":1,"paid_api":"off"}"#);
        let readback = |raw: &str| Ok(RoutingFile::from_bytes(Some(raw.as_bytes())));
        let live = llm::LiveRouting::default();
        let start = live.snapshot();
        activate_signed_routing(
            &live,
            &start,
            &signed,
            readback(r#"{"version":1,"paid_api":"off"}"#),
        )
        .unwrap();
        assert_eq!(live.snapshot().routing, Some(signed.clone()));
        assert!(live.snapshot().paid_api_off());

        // Piggybacked change on disk, unreadable or invalid readback, or a
        // live routing that moved meanwhile: all fail closed.
        let piggybacked = r#"{"version":1,"paid_api":"off","tiers":{"hard":{"chain":[{"provider":"openai-api","model":"x"}]}}}"#;
        let cases: Vec<(llm::RoutingSnapshot, Result<RoutingFile, &'static str>)> = vec![
            (llm::RoutingSnapshot::default(), readback(piggybacked)),
            (llm::RoutingSnapshot::default(), Err("routing_unsafe")),
            (llm::RoutingSnapshot::default(), readback("{")),
            (
                llm::RoutingSnapshot::unavailable("routing_invalid"),
                readback(r#"{"version":1,"paid_api":"off"}"#),
            ),
        ];
        for (expected, disk) in cases {
            let live = llm::LiveRouting::default();
            assert!(activate_signed_routing(&live, &expected, &signed, disk).is_err());
            let snapshot = live.snapshot();
            assert_eq!(
                snapshot.unavailable_reason,
                Some("routing_activation_unverified")
            );
            assert!(snapshot.routing.is_none());
            assert!(snapshot.refuses_provider("openai-api"));
        }
    }

    #[test]
    fn signed_routing_repairs_an_invalid_file() {
        let live = llm::LiveRouting::new(llm::RoutingSnapshot::unavailable("routing_invalid"));
        let start = live.snapshot();
        let signed = routing(r#"{"version":1}"#);
        activate_signed_routing(
            &live,
            &start,
            &signed,
            Ok(RoutingFile::from_bytes(Some(b"{\n  \"version\": 1\n}\n"))),
        )
        .unwrap();
        assert_eq!(live.snapshot().unavailable_reason, None);
        assert!(!live.snapshot().paid_api_off());
    }

    #[test]
    fn refused_routing_keeps_live_state_only_when_the_file_is_unchanged() {
        let active = llm::RoutingSnapshot {
            routing: Some(routing(r#"{"version":1}"#)),
            unavailable_reason: None,
        };
        let before = RoutingFile::from_bytes(Some(br#"{"version":1}"#));
        let live = llm::LiveRouting::new(active.clone());
        let (status, _) = refused_routing(&live, &before.sha256, Ok(before.clone()));
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(live.snapshot(), active);
        for readback in [
            Ok(RoutingFile::from_bytes(Some(
                br#"{"version":1,"paid_api":"off"}"#,
            ))),
            Err("routing_unreadable"),
        ] {
            let live = llm::LiveRouting::new(active.clone());
            let (status, _) = refused_routing(&live, &before.sha256, readback);
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert!(live.snapshot().paid_api_off());
        }
    }

    #[test]
    fn paid_api_off_refuses_metered_brain_pins_with_a_bounded_error() {
        let policy = llm::LiveModelPolicy::new(llm::ModelAccessPolicy {
            version: 1,
            models: ["anthropic-api", "claude-cli"]
                .into_iter()
                .map(|provider| llm::ModelAccessEntry {
                    provider: provider.into(),
                    model: "fixture".into(),
                    enabled: true,
                    source: "test".into(),
                    route: None,
                })
                .collect(),
        });
        let allowed = llm::RoutingSnapshot::default();
        let off = llm::RoutingSnapshot {
            routing: llm::ModelRouting::parse(br#"{"version":1,"paid_api":"off"}"#).ok(),
            unavailable_reason: None,
        };
        let failed = llm::RoutingSnapshot::unavailable("routing_invalid");
        let pin =
            |routing, provider| brain_selection(&policy, routing, Some(provider), Some("fixture"));
        assert!(pin(&allowed, "anthropic-api").is_ok());
        for routing in [&off, &failed] {
            let (status, Json(body)) = pin(routing, "anthropic-api").unwrap_err();
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(body["error"], "paid API is off");
            assert!(pin(routing, "claude-cli").is_ok());
            assert!(brain_selection(&policy, routing, None, None).is_ok());
        }
        // The allowlist still applies when paid APIs are allowed.
        let (status, _) = pin(&allowed, "openai-api").unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
