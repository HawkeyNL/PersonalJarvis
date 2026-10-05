//! Self-development advisor (ADR-029 fase 4d). Jarvis inspects its OWN ecosystem
//! (host, brains, model catalog, budget, agent capabilities) and **proposes**
//! concrete improvements to itself.
//!
//! Hard boundaries (owner policy — the whole point of this layer):
//! - **Advisory only.** It never executes anything, edits no files, runs no tools.
//!   Carrying out a proposal goes through the approval-gated agentic layer (4b/4c).
//! - **Only on request.** There is no background loop; the API exposes this as an
//!   owner-triggered endpoint.
//! - **Nothing paid is auto-activated** (Jarvis.md §12). Paid changes are flagged
//!   `requires_approval` with a cost note; the owner decides.
//! - **The Core (`jarvis-core/**`) and Jarvis' own `Jarvis.md` are never agent-editable.**
//!   Proposals touching them are marked owner-only/manual.
//!
//! Pure LLM reasoning routed through the cost-aware router (ADR-027/028), so model
//! choice + budget still apply. The caller bills every reply via `calls`.

use std::sync::Arc;

use jarvis_llm::{ChatMessage, ChatReply, ChatRequest, LlmError, LlmProvider, RoutingMode, Tier};

const MAX_TOKENS: u32 = 1200;

const SYS: &str = "You are Jarvis and you reflect on how to improve YOURSELF, \
based on your own ecosystem (hardware, brains, models, budget, capabilities). \
You ONLY PROPOSE — you execute nothing, edit no files, activate nothing. \
Rules you never break: (1) activate nothing paid without permission — \
mark such proposals with requires_approval=true and state the cost; (2) stay within \
the monthly budget; (3) you NEVER touch the Core (jarvis-core/**) or your own Jarvis.md yourself — \
mark proposals about those as owner-only (done manually by the owner). \
Give 2 to 6 concrete proposals. Answer ONLY with JSON of the form: \
{\"summary\": \"short summary\", \"proposals\": [{\"title\": \"...\", \
\"category\": \"model|key|tool|code|capability|core\", \"rationale\": \"why\", \
\"cost\": \"gratis|metered|abonnement\", \"requires_approval\": true, \
\"steps\": [\"step 1\", \"step 2\"]}]} and nothing else.";

/// One improvement Jarvis proposes for itself. Advisory — never executed here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Proposal {
    pub title: String,
    /// `model` | `key` | `tool` | `code` | `capability` | `core`.
    pub category: String,
    pub rationale: String,
    /// `gratis` | `metered` | `abonnement`.
    pub cost: String,
    /// Whether carrying this out needs the owner's explicit go-ahead. Defaults to
    /// true when the model omits it — safe side.
    pub requires_approval: bool,
    pub steps: Vec<String>,
}

/// The advisor's output: a summary, the proposals, and every LLM reply (so the
/// caller can bill usage — ADR-027).
pub struct SelfDevReport {
    pub summary: String,
    pub proposals: Vec<Proposal>,
    pub calls: Vec<ChatReply>,
}

/// Ask Jarvis to propose improvements to itself, given a rendered snapshot of its
/// ecosystem and budget. `focus` narrows the scope (e.g. "cheaper models").
pub async fn propose(
    llm: &Arc<dyn LlmProvider>,
    persona: &str,
    ecosystem: &str,
    budget_eur: f64,
    spent_eur: f64,
    focus: Option<&str>,
) -> Result<SelfDevReport, LlmError> {
    let system = if persona.trim().is_empty() {
        SYS.to_string()
    } else {
        format!("{persona}\n\n[Rol nu] {SYS}")
    };
    let user = build_context(ecosystem, budget_eur, spent_eur, focus);
    let req = ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(user)],
        tier: Tier::Hard, // planning about oneself — the strong tier
        mode: RoutingMode::Deep,
        max_tokens: MAX_TOKENS,
        model: None,
        provider: None,
    };
    let reply = llm.chat(&req).await?;
    let (summary, proposals) = parse_report(&reply.text);
    Ok(SelfDevReport {
        summary,
        proposals,
        calls: vec![reply],
    })
}

fn build_context(ecosystem: &str, budget_eur: f64, spent_eur: f64, focus: Option<&str>) -> String {
    let remaining = (budget_eur - spent_eur).max(0.0);
    let mut s = String::from("Your current ecosystem:\n");
    s.push_str(ecosystem);
    s.push_str(&format!(
        "\n\nBudget: €{spent_eur:.2} of €{budget_eur:.2} used this month (€{remaining:.2} left).\n"
    ));
    if let Some(f) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        s.push_str(&format!("\nFocus of this request: {f}\n"));
    }
    s.push_str("\nWhich concrete improvements to yourself do you propose?");
    s
}

/// Parse `{summary, proposals:[...]}` from the reply, tolerant of surrounding
/// prose. On failure the raw text becomes the summary with no structured proposals
/// (the owner still sees the advice).
fn parse_report(text: &str) -> (String, Vec<Proposal>) {
    if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[a..=b]) {
                let summary = v
                    .get("summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let proposals = v
                    .get("proposals")
                    .and_then(|p| p.as_array())
                    .map(|arr| arr.iter().filter_map(parse_proposal).collect::<Vec<_>>())
                    .unwrap_or_default();
                if !summary.is_empty() || !proposals.is_empty() {
                    return (
                        if summary.is_empty() {
                            "Self-improvement proposals.".to_string()
                        } else {
                            summary
                        },
                        proposals,
                    );
                }
            }
        }
    }
    (text.trim().to_string(), Vec::new())
}

fn parse_proposal(v: &serde_json::Value) -> Option<Proposal> {
    let title = v.get("title").and_then(|t| t.as_str())?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let steps = v
        .get("steps")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(Proposal {
        title,
        category: v
            .get("category")
            .and_then(|c| c.as_str())
            .unwrap_or("capability")
            .trim()
            .to_string(),
        rationale: v
            .get("rationale")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        cost: v
            .get("cost")
            .and_then(|c| c.as_str())
            .unwrap_or("gratis")
            .trim()
            .to_string(),
        // Safe default: assume approval is needed unless the model says otherwise.
        requires_approval: v
            .get("requires_approval")
            .and_then(|b| b.as_bool())
            .unwrap_or(true),
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct Fake(String);
    #[async_trait]
    impl LlmProvider for Fake {
        fn label(&self) -> &str {
            "fake"
        }
        async fn chat(&self, _req: &ChatRequest) -> Result<ChatReply, LlmError> {
            Ok(ChatReply {
                text: self.0.clone(),
                model: "fake".into(),
                backend: Some("fake".into()),
                requested_route: None,
                actual_provider: None,
                fallback_count: 0,
                stop_reason: Some("end_turn".into()),
                usage: None,
            })
        }
    }

    #[test]
    fn parses_a_structured_report() {
        let (summary, props) = parse_report(
            r#"some text {"summary":"short","proposals":[
              {"title":"DeepSeek-reasoner only for Hard","category":"model",
               "rationale":"cheaper","cost":"metered","requires_approval":true,
               "steps":["a","b"]}]} some more"#,
        );
        assert_eq!(summary, "short");
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].category, "model");
        assert!(props[0].requires_approval);
        assert_eq!(props[0].steps, vec!["a", "b"]);
    }

    #[test]
    fn falls_back_to_raw_text_without_json() {
        let (summary, props) = parse_report("just text without json");
        assert_eq!(summary, "just text without json");
        assert!(props.is_empty());
    }

    #[test]
    fn requires_approval_defaults_true_and_cost_free() {
        let p = parse_proposal(&serde_json::json!({ "title": "x" })).unwrap();
        assert!(p.requires_approval);
        assert_eq!(p.cost, "gratis");
        assert_eq!(p.category, "capability");
    }

    #[tokio::test]
    async fn propose_returns_parsed_proposals_and_a_billable_call() {
        let llm: Arc<dyn LlmProvider> = Arc::new(Fake(
            r#"{"summary":"s","proposals":[{"title":"T","category":"tool","requires_approval":false}]}"#
                .to_string(),
        ));
        let report = propose(&llm, "persona", "ecosystem", 50.0, 10.0, Some("models"))
            .await
            .unwrap();
        assert_eq!(report.summary, "s");
        assert_eq!(report.proposals.len(), 1);
        assert!(!report.proposals[0].requires_approval);
        assert_eq!(report.calls.len(), 1);
    }
}
