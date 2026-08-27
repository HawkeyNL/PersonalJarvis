//! LLM cost metering (ADR-027). Records each metered reply's spend and keeps the
//! monthly-spend counter fresh so the router's budget gate stays correct. Shared
//! by the chat and self-dev paths. Billing must never break a request, so DB
//! errors are logged, not surfaced; free backends (plan/Ollama) are skipped.

use std::sync::atomic::Ordering;

use jarvis_llm as llm;
use jarvis_usage as usage;

use crate::AppState;

/// Record a reply's cost and refresh the monthly spend counter (ADR-027). Free
/// backends (plan/Ollama) cost nothing and are skipped. Billing must never break
/// a chat, so DB errors are logged, not surfaced.
pub(crate) async fn record_usage(state: &AppState, reply: &llm::ChatReply) {
    let (Some(u), Some(backend)) = (&reply.usage, reply.backend.as_deref()) else {
        return;
    };
    let cost = usage::cost_eur(
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
    if !usage::is_metered(backend) {
        return; // plan/local: nothing to bill or count
    }
    let entry = usage::UsageEntry {
        backend: backend.to_string(),
        model: reply.model.clone(),
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
}
