//! LLM usage and cost metering (ADR-027). Provider-reported token counts are
//! recorded for paid and zero-cost backends; only paid backends accrue spend.
//! Metering must never break a request, so database errors are logged rather
//! than surfaced to the assistant caller.

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};

use jarvis_llm as llm;
use jarvis_usage as usage;

use crate::{routes::system::usage_value, AppState};

/// Record a reply's token use and cost, then refresh the monthly spend counter.
/// Billing/telemetry must never break a chat, so errors are logged, not surfaced.
pub(crate) async fn record_usage(state: &AppState, reply: &llm::ChatReply) {
    record_usage_with_metadata(state, reply, usage::UsageMetadata::default()).await;
}

pub(crate) async fn record_usage_with_metadata(
    state: &AppState,
    reply: &llm::ChatReply,
    metadata: usage::UsageMetadata,
) {
    let (Some(u), Some(backend)) = (&reply.usage, reply.backend.as_deref()) else {
        return;
    };
    let cost = usage::cost_eur_with_registry(
        &state.pricing_registry,
        backend,
        &reply.model,
        u.input_tokens,
        u.output_tokens,
        u.cache_read_tokens,
        state.eur_per_usd,
    );
    tracing::info!(
        %backend, model = %reply.model, input = u.input_tokens, output = u.output_tokens,
        cache_read = u.cache_read_tokens, cost_eur = cost, "assistant chat usage",
    );
    let entry = usage::UsageEntry {
        request_id: metadata.request_id,
        backend: backend.to_string(),
        model: reply.model.clone(),
        routing_mode: metadata.routing_mode,
        quality_tier: metadata.quality_tier,
        agent_id: metadata.agent_id,
        latency_ms: metadata.latency_ms,
        status: metadata.status,
        failure_category: metadata.failure_category,
        fallback_count: metadata.fallback_count,
        input_tokens: u.input_tokens as i32,
        output_tokens: u.output_tokens as i32,
        cache_read_tokens: u.cache_read_tokens as i32,
        cache_write_tokens: u.cache_write_tokens as i32,
        cost_eur: cost,
    };
    if let Err(e) = usage::record(&state.db, &entry).await {
        tracing::warn!(error = %e, "failed to record llm usage");
    }
    // Re-read the month total so the gate stays correct across a month rollover.
    match usage::month_total_eur(&state.db).await {
        Ok(total) => {
            let cents = (total * 100.0).round() as u64;
            state.spent_cents.store(cents, Ordering::Relaxed);
            state.budget_book.reconcile_actual(cents);
        }
        Err(e) => tracing::warn!(error = %e, "failed to refresh monthly spend"),
    }
    refresh_usage_snapshot(state).await;
}

/// Refresh the bounded non-secret aggregate consumed by the local admin GUI.
/// Failure is observability-only and must never break an assistant response.
pub async fn refresh_usage_snapshot(state: &AppState) {
    let Some(path) = state.usage_snapshot_path.as_deref() else {
        return;
    };
    let Ok(mut value) = usage_value(state).await else {
        tracing::warn!("failed to query non-secret usage aggregates");
        return;
    };
    value["generated_at_unix"] = serde_json::json!(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs());
    let Ok(bytes) = serde_json::to_vec(&value) else {
        return;
    };
    if bytes.len() > 512 * 1024 {
        tracing::warn!("usage summary exceeded its safe size bound");
        return;
    }
    let Some(parent) = path.parent() else {
        return;
    };
    let temporary = parent.join(format!(".usage-summary.{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        tracing::warn!(%error, "failed to refresh non-secret usage summary");
    }
}
