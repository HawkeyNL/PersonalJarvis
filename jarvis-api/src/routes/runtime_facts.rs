//! Live, non-secret facts about Jarvis' own runtime setup, appended to the
//! persona on the chat and orchestration paths so the model can answer "which
//! models do you use?" truthfully. Built per request from in-process
//! snapshots. Only the fields rendered below leave the Home Node: never
//! tokens, account emails or ids, paths, hostnames, conids or symbols.
//! A source that cannot be read is left out; it never fails the chat.

use std::time::Duration;

use jarvis_llm::{
    claude_worker_protocol::{self, MAX_SYSTEM_BYTES},
    codex_chat_protocol, RoutingSnapshot, Tier,
};
use rust_decimal::Decimal;
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};

use crate::ibkr::store;
use crate::AppState;

/// Hard cap for the whole block (preface included).
const MAX_BYTES: usize = 2 * 1024;
/// One owner-configured value (a model id, an agent name) cannot crowd out
/// the other lines.
const MAX_LINE_BYTES: usize = 400;
/// Room kept below the worker's system cap for the orchestrator's role text.
const ROLE_HEADROOM: usize = 1024;
const DB_TIMEOUT: Duration = Duration::from_secs(2);
const PREFACE: &str = "Runtime facts: live facts about your own Jarvis setup, read by Core \
for this request. You cannot change them; the owner changes them in Jarvis Core Admin. \
Anything about your setup that is not listed here is unknown to you.";

pub(super) struct Portfolio {
    pub as_of: OffsetDateTime,
    pub positions: usize,
    pub skipped: usize,
    pub total_eur: Decimal,
}

pub(super) enum Ibkr {
    NotLinked,
    /// Linked, but the portfolio could not be read right now.
    Linked,
    NoSyncYet,
    Synced(Portfolio),
}

pub(super) struct Facts<'a> {
    pub version: &'a str,
    pub routing: &'a RoutingSnapshot,
    pub allows: &'a dyn Fn(&str, &str) -> bool,
    pub claude_worker: bool,
    pub codex_worker: bool,
    /// `None` when no agent bundle is loaded.
    pub agents: Option<Vec<String>>,
    pub ibkr: Ibkr,
}

/// The persona followed by the runtime facts of this moment.
pub(super) async fn system_prompt(state: &AppState) -> String {
    let claude_worker = socket_exists(claude_worker_protocol::SOCKET).await;
    let codex_worker = socket_exists(codex_chat_protocol::SOCKET).await;
    let ibkr = ibkr(state).await;
    let routing = state.model_routing.snapshot();
    let allows = |provider: &str, model: &str| state.model_policy.allows(provider, model);
    let facts = Facts {
        version: env!("CARGO_PKG_VERSION"),
        routing: &routing,
        allows: &allows,
        claude_worker,
        codex_worker,
        agents: state.agent_registry.as_deref().map(|loader| {
            let snapshot = loader.snapshot();
            snapshot
                .registry
                .agents()
                .iter()
                .map(|agent| agent.name.clone())
                .collect()
        }),
        ibkr,
    };
    compose(&state.jarvis_system, &facts)
}

/// A socket-activated worker's socket exists only while the owner has it
/// enabled. This says nothing about the account login, which only root sees.
async fn socket_exists(path: &str) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

async fn ibkr(state: &AppState) -> Ibkr {
    if state.ibkr.is_none() {
        return Ibkr::NotLinked;
    }
    // ponytail: reuses the full read (at most 1000 rows) for three numbers;
    // add a summary query if chat latency ever shows it.
    match tokio::time::timeout(DB_TIMEOUT, store::load_portfolio(&state.db)).await {
        Ok(Ok(Some(portfolio))) => Ibkr::Synced(Portfolio {
            as_of: portfolio.as_of,
            positions: portfolio.positions.len(),
            skipped: portfolio.skipped_count,
            total_eur: portfolio.net_liquidation,
        }),
        Ok(Ok(None)) => Ibkr::NoSyncYet,
        _ => Ibkr::Linked,
    }
}

/// Persona plus facts, keeping the system text within the subscription
/// worker's cap. A persona that leaves no room gets no facts.
fn compose(persona: &str, facts: &Facts) -> String {
    let room = MAX_SYSTEM_BYTES
        .saturating_sub(ROLE_HEADROOM)
        .saturating_sub(persona.len() + 2);
    let block = render(facts, room.min(MAX_BYTES));
    if block.is_empty() {
        persona.to_string()
    } else {
        format!("{persona}\n\n{block}")
    }
}

/// Plain-text block of at most `max` bytes. Lines are kept in a fixed order
/// and the first line that does not fit ends the block, so truncation is
/// deterministic. Empty when not even the preface and one fact fit.
fn render(facts: &Facts, max: usize) -> String {
    let mut out = PREFACE.to_string();
    let mut kept = 0;
    for line in lines(facts) {
        let line = clip(&line);
        if out.len() + 1 + line.len() > max {
            break;
        }
        out.push('\n');
        out.push_str(&line);
        kept += 1;
    }
    if kept == 0 {
        String::new()
    } else {
        out
    }
}

fn lines(facts: &Facts) -> Vec<String> {
    let routing = facts.routing;
    let mut lines = vec![
        format!("Jarvis Core version: {}.", facts.version),
        format!(
            "Paid APIs: {}.",
            if routing.paid_api_off() { "off" } else { "on" }
        ),
    ];
    for (name, tier) in [
        ("cheap", Tier::Cheap),
        ("default", Tier::Default),
        ("hard", Tier::Hard),
    ] {
        let chain = match routing.tier(tier) {
            None => "built-in order".to_string(),
            Some(route) => {
                let models: Vec<String> = route
                    .runtime_chain()
                    .filter(|entry| {
                        (facts.allows)(&entry.provider, &entry.model)
                            && !routing.refuses_provider(&entry.provider)
                    })
                    .map(|entry| format!("{}:{}", entry.provider, entry.model))
                    .collect();
                if models.is_empty() {
                    "no allowed model".to_string()
                } else {
                    models.join(" > ")
                }
            }
        };
        lines.push(format!("Model chain for tier {name}: {chain}."));
    }
    let enabled = |on: bool| if on { "enabled" } else { "not enabled" };
    lines.push(format!(
        "Subscription workers: claude-cli {}, codex-cli {} (account login status is not visible to Core).",
        enabled(facts.claude_worker),
        enabled(facts.codex_worker)
    ));
    match &facts.ibkr {
        Ibkr::NotLinked => lines.push("IBKR: not linked.".into()),
        Ibkr::Linked => lines.push("IBKR: linked.".into()),
        Ibkr::NoSyncYet => lines.push("IBKR: linked; portfolio not synced yet.".into()),
        Ibkr::Synced(portfolio) => {
            let as_of = portfolio
                .as_of
                .to_offset(UtcOffset::UTC)
                .replace_nanosecond(0)
                .ok()
                .and_then(|at| at.format(&Rfc3339).ok())
                .unwrap_or_else(|| "unknown".into());
            let skipped = if portfolio.skipped > 0 {
                format!(", {} non-EUR positions not tracked", portfolio.skipped)
            } else {
                String::new()
            };
            lines.push(format!(
                "IBKR: linked; portfolio last synced {as_of}, {} positions{skipped}, total value EUR {}.",
                portfolio.positions,
                portfolio.total_eur.round_dp(2)
            ));
        }
    }
    lines.push(match &facts.agents {
        None => "Loaded agents: none (no agent bundle loaded).".into(),
        Some(names) if names.is_empty() => "Loaded agents: none.".into(),
        Some(names) => format!("Loaded agents ({}): {}.", names.len(), names.join(", ")),
    });
    lines
}

/// One line, no control characters, at most [`MAX_LINE_BYTES`].
fn clip(line: &str) -> String {
    let mut out = String::new();
    for ch in line.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if out.len() + ch.len_utf8() > MAX_LINE_BYTES - '…'.len_utf8() {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_llm::{ModelRouting, PaidApi, RouteEntry, TierRoute, TierRoutes};

    fn entry(provider: &str, model: &str) -> RouteEntry {
        RouteEntry {
            provider: provider.into(),
            model: model.into(),
        }
    }

    fn routing(paid_api: PaidApi) -> RoutingSnapshot {
        RoutingSnapshot {
            routing: Some(ModelRouting {
                version: 1,
                paid_api,
                tiers: TierRoutes {
                    cheap: None,
                    default: Some(TierRoute {
                        chain: vec![
                            entry("claude-cli", "claude-sonnet-fixture"),
                            entry("codex-cli", "gpt-fixture"),
                            entry("ollama", "disallowed-fixture"),
                        ],
                        metered_after_subscription: false,
                    }),
                    hard: Some(TierRoute {
                        chain: vec![
                            entry("anthropic-api", "claude-opus-fixture"),
                            entry("claude-cli", "claude-opus-fixture"),
                        ],
                        metered_after_subscription: false,
                    }),
                },
                research_web_search: Default::default(),
            }),
            unavailable_reason: None,
        }
    }

    fn allows(_: &str, model: &str) -> bool {
        model != "disallowed-fixture"
    }

    fn facts<'a>(routing: &'a RoutingSnapshot, ibkr: Ibkr) -> Facts<'a> {
        Facts {
            version: "9.8.7",
            routing,
            allows: &allows,
            claude_worker: true,
            codex_worker: false,
            agents: Some(vec!["Research".into(), "Trading".into()]),
            ibkr,
        }
    }

    fn synced() -> Ibkr {
        Ibkr::Synced(Portfolio {
            as_of: OffsetDateTime::from_unix_timestamp(1_793_000_000).unwrap(),
            positions: 12,
            skipped: 0,
            total_eur: Decimal::new(1_234_567_891, 4),
        })
    }

    #[test]
    fn renders_allowed_chains_workers_agents_and_portfolio_summary() {
        let routing = routing(PaidApi::Allowed);
        let block = render(&facts(&routing, synced()), MAX_BYTES);
        assert!(block.starts_with(PREFACE));
        for expected in [
            "Jarvis Core version: 9.8.7.",
            "Paid APIs: on.",
            "Model chain for tier cheap: built-in order.",
            "Model chain for tier default: claude-cli:claude-sonnet-fixture > codex-cli:gpt-fixture.",
            "Model chain for tier hard: anthropic-api:claude-opus-fixture > claude-cli:claude-opus-fixture.",
            "Subscription workers: claude-cli enabled, codex-cli not enabled",
            "IBKR: linked; portfolio last synced 2026-10-26T",
            "12 positions, total value EUR 123456.79.",
            "Loaded agents (2): Research, Trading.",
        ] {
            assert!(block.contains(expected), "missing {expected:?} in {block}");
        }
        assert!(!block.contains("disallowed-fixture"));
        assert!(!block.contains('#'), "no Markdown headings");
        assert!(block.len() <= 1536, "target size: {}", block.len());
    }

    #[test]
    fn paid_api_off_drops_metered_entries_and_failed_reads_are_omitted() {
        let routing = routing(PaidApi::Off);
        let block = render(&facts(&routing, Ibkr::Linked), MAX_BYTES);
        assert!(block.contains("Paid APIs: off."));
        assert!(block.contains("Model chain for tier hard: claude-cli:claude-opus-fixture."));
        assert!(block.contains("IBKR: linked.\n"));
        assert!(!block.contains("portfolio"));
        let unusable = RoutingSnapshot::unavailable("routing_unavailable");
        let mut facts = facts(&unusable, Ibkr::NotLinked);
        facts.agents = None;
        let block = render(&facts, MAX_BYTES);
        assert!(block.contains("Paid APIs: off."));
        assert!(block.contains("IBKR: not linked."));
        assert!(block.contains("Loaded agents: none (no agent bundle loaded)."));
    }

    #[test]
    fn never_contains_paths_emails_conids_or_symbols() {
        let routing = routing(PaidApi::Allowed);
        let block = render(&facts(&routing, synced()), MAX_BYTES);
        for forbidden in ["/run/", "/etc/", "/var/", ".sock", "@", "conid", "AAPL"] {
            assert!(!block.contains(forbidden), "{forbidden:?} in {block}");
        }
    }

    #[test]
    fn truncation_is_deterministic_and_capped() {
        let routing = routing(PaidApi::Allowed);
        let mut many = facts(&routing, synced());
        many.agents = Some((0..200).map(|n| format!("Agent\n{n:03}")).collect());
        let full = render(&many, MAX_BYTES);
        assert!(full.len() <= MAX_BYTES);
        let agents = full.lines().last().unwrap();
        assert!(agents.starts_with("Loaded agents (200): Agent 000"));
        assert!(agents.ends_with('…') && agents.len() <= MAX_LINE_BYTES);
        // A smaller cap keeps a prefix of the same lines, never a partial line.
        let small = render(&many, PREFACE.len() + 60);
        assert_eq!(small, render(&many, PREFACE.len() + 60));
        assert!(full.starts_with(&small) && small.len() <= PREFACE.len() + 60);
        assert_eq!(small.lines().count(), 3);
        assert!(render(&many, PREFACE.len() + 10).is_empty());
    }

    #[test]
    fn composed_system_stays_within_the_worker_cap() {
        let routing = routing(PaidApi::Allowed);
        let facts = facts(&routing, synced());
        let composed = compose("Persona.", &facts);
        assert!(composed.starts_with("Persona.\n\nRuntime facts:"));
        let large = "p".repeat(MAX_SYSTEM_BYTES - ROLE_HEADROOM - 400);
        let composed = compose(&large, &facts);
        assert!(composed.len() <= MAX_SYSTEM_BYTES - ROLE_HEADROOM);
        assert!(composed.contains("Jarvis Core version"));
        let full = "p".repeat(MAX_SYSTEM_BYTES);
        assert_eq!(compose(&full, &facts), full);
    }
}
