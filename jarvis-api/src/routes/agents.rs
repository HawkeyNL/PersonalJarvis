//! Owner-visible, read-only view of the installed private agents and their
//! usage this month. Only presentation metadata leaves Core: instructions,
//! requested capabilities and denied actions stay inside the protected bundle.

use axum::{extract::State, Json};
use serde_json::{json, Value};

use jarvis_core::AgentDefinition;
use jarvis_usage as usage;

use crate::routes::system::agent_usage_value;
use crate::{AppState, Authed};

const MAX_DESCRIPTION_CHARS: usize = 1_000;
const MAX_TOOLS: usize = 64;
const MAX_TOOL_NAME_LEN: usize = 128;

pub(crate) async fn list(_authed: Authed, State(state): State<AppState>) -> Json<Value> {
    let Some(registry) = state.agent_registry.as_deref() else {
        return Json(json!({
            "bundle_id": null,
            "agent_count": 0,
            "agents": [],
            "unavailable_reason": "agent_bundle_unavailable",
            "usage_period": "current_calendar_month",
        }));
    };
    // Until Core records which agent made a call, per-agent usage would be
    // all zeros: report it as not measured instead.
    let usage = if !usage::AGENT_USAGE_INSTRUMENTED {
        Err("agent_usage_not_instrumented")
    } else {
        usage::month_agent_statistics(&state.db)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "agent usage aggregates unavailable");
                "usage_query_failed"
            })
    };
    Json(agents_value(
        registry.bundle_id(),
        registry.agents(),
        usage.as_deref().map_err(|reason| *reason),
    ))
}

/// Explicit field allowlist: a new `AgentDefinition` field is never exposed
/// unless it is added here. Without usage rows every agent's `usage` is
/// `null` and `usage_unavailable_reason` says why.
fn agents_value(
    bundle_id: &str,
    agents: &[AgentDefinition],
    usage: Result<&[usage::AgentUsage], &'static str>,
) -> Value {
    let agents: Vec<Value> = agents
        .iter()
        .map(|agent| {
            let usage = usage.ok().map(|rows| {
                match rows.iter().find(|row| row.agent_id == agent.id) {
                    Some(row) => agent_usage_value(&row.totals, row.last_used.as_deref()),
                    None => agent_usage_value(&usage::UsageTotals::default(), None),
                }
            });
            let tools: Vec<&str> = agent
                .allowed_tools
                .iter()
                .map(String::as_str)
                .filter(|tool| !tool.is_empty() && tool.len() <= MAX_TOOL_NAME_LEN)
                .take(MAX_TOOLS)
                .collect();
            json!({
                "id": agent.id,
                "name": agent.name,
                "group": agent.group,
                "description": agent.description.chars().take(MAX_DESCRIPTION_CHARS).collect::<String>(),
                "model_policy": agent.model_policy,
                "allowed_tools": tools,
                "limits": agent.limits,
                "usage": usage,
            })
        })
        .collect();
    json!({
        "bundle_id": bundle_id,
        "agent_count": agents.len(),
        "agents": agents,
        "unavailable_reason": null,
        "usage_period": "current_calendar_month",
        "usage_unavailable_reason": usage.err(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> AgentDefinition {
        AgentDefinition {
            id: id.into(),
            name: "Researcher".into(),
            group: Some("Knowledge".into()),
            description: "Finds sources.".into(),
            model_policy: "research".into(),
            instructions: "SECRET-INSTRUCTIONS never leave Core".into(),
            requested_capabilities: vec![jarvis_policy::Capability::TradingExecute],
            allowed_tools: vec!["web.search".into(), "x".repeat(MAX_TOOL_NAME_LEN + 1)],
            denied_actions: vec!["SECRET-DENIED-ACTION".into()],
            limits: jarvis_core::AgentLimits {
                max_runtime_seconds: 600,
                max_context_chars: 50_000,
                max_output_chars: 10_000,
                max_parallel_runs: 1,
            },
        }
    }

    #[test]
    fn only_allowlisted_agent_fields_are_exposed() {
        let value = agents_value("bundle-1", &[agent("researcher")], Ok(&[]));
        let text = value.to_string();
        for secret in [
            "SECRET-INSTRUCTIONS",
            "SECRET-DENIED-ACTION",
            "instructions",
            "requested_capabilities",
            "denied_actions",
            "TradingExecute",
        ] {
            assert!(!text.contains(secret), "{secret}");
        }
        let entry = &value["agents"][0];
        let mut keys: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "allowed_tools",
                "description",
                "group",
                "id",
                "limits",
                "model_policy",
                "name",
                "usage"
            ]
        );
        assert_eq!(entry["allowed_tools"], json!(["web.search"]));
        assert_eq!(entry["limits"]["max_runtime_seconds"], 600);
        assert_eq!(value["agent_count"], 1);
        assert_eq!(value["bundle_id"], "bundle-1");
    }

    #[test]
    fn usage_is_matched_per_agent_and_zero_when_unused() {
        let used = usage::AgentUsage {
            agent_id: "researcher".into(),
            last_used: Some("2026-10-03T09:00:00Z".into()),
            totals: usage::UsageTotals {
                requests: 3,
                total_tokens: 900,
                cost_eur: 0.12,
                ..Default::default()
            },
        };
        let value = agents_value(
            "bundle-1",
            &[agent("researcher"), agent("trader")],
            Ok(&[used]),
        );
        assert_eq!(value["agents"][0]["usage"]["requests"], 3);
        assert_eq!(value["agents"][0]["usage"]["spent_eur"], 0.12);
        assert_eq!(
            value["agents"][0]["usage"]["last_used"],
            "2026-10-03T09:00:00Z"
        );
        // Not measured yet: null, never a zero.
        assert_eq!(value["agents"][0]["usage"]["failures"], Value::Null);
        assert_eq!(value["agents"][0]["usage"]["fallbacks"], Value::Null);
        assert_eq!(value["agents"][1]["usage"]["requests"], 0);
        assert_eq!(value["agents"][1]["usage"]["last_used"], Value::Null);
        assert_eq!(value["usage_unavailable_reason"], Value::Null);

        let degraded = agents_value(
            "bundle-1",
            &[agent("researcher")],
            Err("agent_usage_not_instrumented"),
        );
        assert_eq!(degraded["agents"][0]["usage"], Value::Null);
        assert_eq!(
            degraded["usage_unavailable_reason"],
            "agent_usage_not_instrumented"
        );
    }
}
