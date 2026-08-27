//! Plan→execute orchestrator (ADR-028 fase 3) — "Orchestrator First" (Jarvis.md §6).
//!
//! A strong model **plans**, cheap models **execute** the steps, and a synthesis
//! step **composes + checks** the result. This is the cost shape of the vision:
//! expensive intelligence only to think, cheap intelligence to do the legwork.
//!
//! PURE LLM orchestration — NO side effects, tools, shell, or file writes. Every
//! step is a text/reasoning call routed through the cost-aware router (ADR-027/028),
//! so model choice + budget still apply. Real execution (running commands, editing
//! files) is the agentic layer (ADR-027 stage 4), gated by an allowlist + approval;
//! it is deliberately out of scope here.

use std::sync::Arc;

use jarvis_llm::{ChatMessage, ChatReply, ChatRequest, LlmError, LlmProvider, RoutingMode, Tier};

/// Cap on plan size — keeps latency/cost bounded and the plan focused.
const MAX_STEPS: usize = 6;
const MAX_TOKENS: u32 = 1024;

const PLAN_SYS: &str = "Je bent Jarvis' planner. Verdeel de taak van de gebruiker in 2 tot 6 \
concrete, uitvoerbare stappen die elk los af te handelen zijn. Denk eerst, plan zuinig. \
Antwoord UITSLUITEND met JSON in de vorm {\"steps\": [\"stap 1\", \"stap 2\"]} en niets anders.";

const EXEC_SYS: &str = "Je voert één stap uit van een groter plan van Jarvis. Gebruik de \
gegeven context en eerdere resultaten. Doe alleen déze stap, bondig en concreet. \
Voer geen echte acties uit (geen commando's/bestanden) — lever het denk-/schrijfwerk als tekst.";

const SYNTH_SYS: &str = "Je bent Jarvis. Combineer de deelresultaten tot één helder, \
volledig eindantwoord voor de gebruiker in het Nederlands. Controleer of het plan is \
gevolgd; noem het eerlijk als er iets ontbreekt of onzeker is.";

/// One executed step and the model that did it.
#[derive(Debug, Clone)]
pub struct StepResult {
    pub step: String,
    pub output: String,
    pub model: String,
}

/// The full run: the plan, each step's result, the composed answer, and every
/// underlying LLM reply (so the caller can bill usage — ADR-027).
pub struct Orchestration {
    pub plan: Vec<String>,
    pub steps: Vec<StepResult>,
    pub answer: String,
    pub calls: Vec<ChatReply>,
}

/// Plan a task, execute each step on a cheap model, then synthesize + check.
/// `persona` is prepended as the system identity (from protected Home Node configuration).
pub async fn plan_and_execute(
    llm: &Arc<dyn LlmProvider>,
    task: &str,
    persona: &str,
) -> Result<Orchestration, LlmError> {
    let mut calls = Vec::new();

    // 1. PLAN — the strong tier (router → heavy model, e.g. Opus on the plan).
    let plan_reply = chat_once(llm, Tier::Hard, PLAN_SYS, task, persona).await?;
    let mut plan = extract_steps(&plan_reply.text);
    plan.truncate(MAX_STEPS);
    if plan.is_empty() {
        plan.push(task.to_string()); // degrade to a single step
    }
    calls.push(plan_reply);

    // 2. EXECUTE — cheap tier per step, threading prior results as context.
    let mut steps: Vec<StepResult> = Vec::with_capacity(plan.len());
    for (i, step) in plan.iter().enumerate() {
        let ctx = exec_context(task, &plan, &steps, i, step);
        let reply = chat_once(llm, Tier::Cheap, EXEC_SYS, &ctx, persona).await?;
        steps.push(StepResult {
            step: step.clone(),
            output: reply.text.clone(),
            model: reply.model.clone(),
        });
        calls.push(reply);
    }

    // 3. SYNTHESIZE + check — the balanced tier.
    let synth_reply = chat_once(
        llm,
        Tier::Default,
        SYNTH_SYS,
        &synth_context(task, &steps),
        persona,
    )
    .await?;
    let answer = synth_reply.text.clone();
    calls.push(synth_reply);

    Ok(Orchestration {
        plan,
        steps,
        answer,
        calls,
    })
}

/// One routed call: persona + a role instruction as the system prompt, the
/// content as the user turn, at the given tier (the router picks the model).
async fn chat_once(
    llm: &Arc<dyn LlmProvider>,
    tier: Tier,
    role_sys: &str,
    content: &str,
    persona: &str,
) -> Result<ChatReply, LlmError> {
    let system = if persona.trim().is_empty() {
        role_sys.to_string()
    } else {
        format!("{persona}\n\n[Rol nu] {role_sys}")
    };
    let req = ChatRequest {
        system: Some(system),
        messages: vec![ChatMessage::user(content)],
        tier,
        mode: match tier {
            Tier::Cheap => RoutingMode::Fast,
            Tier::Hard => RoutingMode::Deep,
            Tier::Default => RoutingMode::Auto,
        },
        max_tokens: MAX_TOKENS,
        model: None,
    };
    llm.chat(&req).await
}

fn exec_context(task: &str, plan: &[String], done: &[StepResult], i: usize, step: &str) -> String {
    let mut s = format!("Oorspronkelijke taak:\n{task}\n\nVolledig plan:\n");
    for (n, p) in plan.iter().enumerate() {
        s.push_str(&format!("{}. {p}\n", n + 1));
    }
    if !done.is_empty() {
        s.push_str("\nResultaten tot nu toe:\n");
        for prev in done {
            s.push_str(&format!("- {}: {}\n", prev.step, prev.output));
        }
    }
    s.push_str(&format!("\nVoer nu stap {} uit: {step}", i + 1));
    s
}

fn synth_context(task: &str, steps: &[StepResult]) -> String {
    let mut s = format!("Oorspronkelijke taak:\n{task}\n\nUitgevoerde stappen:\n");
    for (n, st) in steps.iter().enumerate() {
        s.push_str(&format!("{}. {}\n   → {}\n", n + 1, st.step, st.output));
    }
    s.push_str("\nSchrijf nu het eindantwoord.");
    s
}

/// Pull the plan's steps from the planner's reply. Prefers JSON `{"steps": [...]}`;
/// falls back to numbered/bulleted lines. Empty ⇒ the caller degrades to one step.
pub fn extract_steps(text: &str) -> Vec<String> {
    // 1. JSON object anywhere in the text.
    if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[a..=b]) {
                if let Some(arr) = v.get("steps").and_then(|s| s.as_array()) {
                    let steps: Vec<String> = arr
                        .iter()
                        .filter_map(|s| s.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !steps.is_empty() {
                        return steps;
                    }
                }
            }
        }
    }
    // 2. Numbered ("1." / "1)") or bulleted ("-" / "*") lines.
    text.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with(|c: char| c.is_ascii_digit()) || l.starts_with('-') || l.starts_with('*')
        })
        .map(|l| {
            l.trim_start_matches(|c: char| {
                c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | ' ' | '#')
            })
            .trim()
            .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[test]
    fn extract_steps_prefers_json() {
        let t = "Zeker! ```json\n{\"steps\": [\"onderzoek\", \"schrijf\", \"controleer\"]}\n```";
        assert_eq!(extract_steps(t), ["onderzoek", "schrijf", "controleer"]);
    }

    #[test]
    fn extract_steps_falls_back_to_numbered_lines() {
        let t = "Plan:\n1. Verzamel data\n2) Analyseer\n- Rapporteer\nklaar";
        assert_eq!(
            extract_steps(t),
            ["Verzamel data", "Analyseer", "Rapporteer"]
        );
    }

    #[test]
    fn extract_steps_empty_when_no_structure() {
        assert!(extract_steps("gewoon wat proza zonder stappen").is_empty());
    }

    /// A provider that answers by tier: Hard → a plan, Cheap → a step result,
    /// Default → the synthesis. Records the tiers it saw.
    struct Scripted {
        seen: Mutex<Vec<Tier>>,
    }
    #[async_trait]
    impl LlmProvider for Scripted {
        fn label(&self) -> &str {
            "scripted"
        }
        async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
            self.seen.lock().unwrap().push(req.tier);
            let (text, model) = match req.tier {
                Tier::Hard => (r#"{"steps": ["a", "b"]}"#.to_string(), "opus"),
                Tier::Cheap => ("gedaan".to_string(), "haiku"),
                Tier::Default => ("eindantwoord".to_string(), "sonnet"),
            };
            Ok(ChatReply {
                text,
                model: model.into(),
                backend: Some("test".into()),
                stop_reason: None,
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn plans_on_strong_executes_on_cheap_synthesizes_on_default() {
        let llm: Arc<dyn LlmProvider> = Arc::new(Scripted {
            seen: Mutex::new(Vec::new()),
        });
        let r = plan_and_execute(&llm, "bouw iets", "Je bent Jarvis.")
            .await
            .unwrap();
        assert_eq!(r.plan, ["a", "b"]);
        assert_eq!(r.steps.len(), 2);
        assert!(r.steps.iter().all(|s| s.model == "haiku")); // cheap executes
        assert_eq!(r.answer, "eindantwoord");
        // plan(Hard) + 2×execute(Cheap) + synth(Default) = 4 calls.
        assert_eq!(r.calls.len(), 4);
    }
}
