//! The `dependency_review` job: what Dependabot knows about the allowlisted
//! repositories, put in order by one Claude call or, failing that, by Core,
//! and optionally proposed to a roadmap (see `docs/SCHEDULED_JOBS.md`).

use std::cmp::Reverse;
use std::collections::BTreeSet;

use serde::Deserialize;

use jarvis_llm::claude_worker_protocol::{WorkerTask, MAX_TASK_PROMPT_BYTES};

use jarvis_store::Database;

use super::store::Finish;
use super::{
    Job, Priority, RunStatus, SummarySource, Work, DB_TIMEOUT, MAX_FACTS_BYTES, MAX_PRIORITIES,
    MAX_REASON, MAX_SUMMARY,
};
use crate::memecoin::agent::Brain;
use crate::roadmaps::github::{self, DependencyAlert, DependencyFacts, Severity};
use crate::roadmaps::store::Proposal;
use crate::roadmaps::{
    Link, LinkKind, NewItem, ProposalOp, RoadmapError, Snapshot, MAX_ITEM_TITLE, MAX_PROPOSAL_OPS,
};

/// Read the facts with the GitHub poller's token and allowlist (without
/// either, the run fails as `not_configured`), put them in order and, when
/// the job names a roadmap, propose the upgrades to it.
pub(super) async fn run(work: &Work, db: &Database, job: &Job) -> Finish {
    let Some(token) = work.token.clone().filter(|_| work.github.configured()) else {
        return Finish::failed("not_configured");
    };
    let repos = work.github.repos();
    let mut facts = github::dependency_facts(token, repos).await;
    let mut finish = record(&mut facts);
    if finish.status != RunStatus::Failed {
        let (priorities, summary) = review(work.brain.as_ref(), &facts).await;
        finish.priorities = priorities;
        finish.summary = Some(summary);
        if let Some(roadmap_id) = &job.params.roadmap_id {
            if let Err(error_code) = propose(db, roadmap_id, &facts).await {
                finish.status = RunStatus::Partial;
                finish.error_code.get_or_insert(error_code);
            }
        }
    }
    finish
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, RoadmapError>>,
) -> Result<T, RoadmapError> {
    tokio::time::timeout(DB_TIMEOUT, future)
        .await
        .unwrap_or(Err(RoadmapError::Invalid("timed out")))
}

/// Propose one item per fixable alert to the roadmap, at most one proposal
/// per run; `Ok(None)` when every upgrade is already on it or proposed.
async fn propose(
    db: &Database,
    roadmap_id: &str,
    facts: &DependencyFacts,
) -> Result<Option<String>, &'static str> {
    let snapshot = bounded(crate::roadmaps::store::get(db, roadmap_id))
        .await
        .map_err(|_| "roadmap_proposal_failed")?
        .filter(|snapshot| snapshot.roadmap.archived_at.is_none())
        .ok_or("roadmap_unavailable")?;
    let pending = bounded(crate::roadmaps::store::list_proposals(db, roadmap_id))
        .await
        .map_err(|_| "roadmap_proposal_failed")?;
    submit(db, &snapshot, &pending, facts).await
}

/// Record the proposal on `snapshot`'s revision; a roadmap that changed
/// since is refused (the next run proposes again).
async fn submit(
    db: &Database,
    snapshot: &Snapshot,
    pending: &[Proposal],
    facts: &DependencyFacts,
) -> Result<Option<String>, &'static str> {
    let ops = proposal_ops(snapshot, pending, facts);
    if ops.is_empty() {
        return Ok(None);
    }
    let created = crate::roadmaps::store::create_proposal(
        db,
        &snapshot.roadmap.id,
        snapshot.roadmap.revision,
        &ops,
        Some("Upgrades with a fixed version, from the scheduled dependency review."),
    );
    match bounded(created).await {
        Ok(id) => Ok(Some(id)),
        Err(RoadmapError::Stale | RoadmapError::Conflict) => Err("roadmap_changed"),
        Err(RoadmapError::Limit(_)) => Err("roadmap_limit"),
        Err(RoadmapError::NotFound | RoadmapError::Archived) => Err("roadmap_unavailable"),
        Err(error) => {
            tracing::warn!(%error, "dependency review: roadmap proposal failed");
            Err("roadmap_proposal_failed")
        }
    }
}

fn link_key(link: &Link) -> Option<(String, u64)> {
    Some((link.repo.as_deref()?.to_ascii_lowercase(), link.number?))
}

/// One new item per fixable alert, most severe first: "Upgrade {package} to
/// {version} ({repo})", linked to its Dependabot pull request when there is
/// one. An upgrade whose title or pull request is already on the roadmap or
/// in a pending proposal (or earlier in this one) is left out; at most 20.
fn proposal_ops(
    snapshot: &Snapshot,
    pending: &[Proposal],
    facts: &DependencyFacts,
) -> Vec<ProposalOp> {
    let proposed = pending
        .iter()
        .flat_map(|proposal| &proposal.ops)
        .filter_map(|op| match op {
            ProposalOp::CreateItem { item } => Some((&item.title, &item.links)),
            ProposalOp::UpdateItem { .. } => None,
        });
    let mut titles = BTreeSet::new();
    let mut links = BTreeSet::new();
    for (title, item_links) in snapshot
        .items
        .iter()
        .map(|item| (&item.title, &item.links))
        .chain(proposed)
    {
        titles.insert(title.to_lowercase());
        links.extend(item_links.iter().filter_map(link_key));
    }
    let mut ops = Vec::new();
    for alert in &facts.alerts {
        let Some(version) = &alert.patched else {
            continue;
        };
        let title = format!("Upgrade {} to {version} ({})", alert.package, alert.repo);
        let link = alert
            .pr
            .as_deref()
            .and_then(|reference| {
                facts
                    .pull_requests
                    .iter()
                    .find(|pull| pull.reference == reference)
            })
            .map(|pull| Link {
                kind: LinkKind::Pr,
                repo: Some(pull.repo.clone()),
                number: Some(pull.number),
                tag: None,
                url: None,
                state: None,
            });
        let key = link.as_ref().and_then(link_key);
        if title.chars().count() > MAX_ITEM_TITLE
            || !titles.insert(title.to_lowercase())
            || key.as_ref().is_some_and(|key| links.contains(key))
        {
            continue;
        }
        links.extend(key);
        ops.push(ProposalOp::CreateItem {
            item: NewItem {
                title,
                links: link.into_iter().collect(),
                ..NewItem::default()
            },
        });
        if ops.len() == MAX_PROPOSAL_OPS {
            break;
        }
    }
    ops
}

/// `ok` when every repository was read, `partial` when some repository's
/// alerts were (with the first failure's code), `failed` when none were.
/// The facts are cut to what is stored.
fn record(facts: &mut DependencyFacts) -> Finish {
    let (status, error_code) = match facts.errors.first() {
        None => (RunStatus::Ok, None),
        Some(error) if facts.alerts_read > 0 => (RunStatus::Partial, Some(error.error_code)),
        Some(error) => (RunStatus::Failed, Some(error.error_code)),
    };
    Finish {
        status,
        error_code,
        facts: facts.fit(MAX_FACTS_BYTES),
        summary: None,
        priorities: Vec::new(),
    }
}

/// The priorities and summary: from one Claude call over at most 16 KiB of
/// the facts, or, when there is nothing to order, the call fails or its
/// answer breaks a rule, from Core's own order and template.
async fn review(
    brain: &dyn Brain,
    facts: &DependencyFacts,
) -> (Vec<Priority>, (String, SummarySource)) {
    if facts.alerts.is_empty() && facts.pull_requests.is_empty() {
        return fallback(facts);
    }
    let mut seen = facts.clone();
    let Some(input) = seen
        .fit(MAX_TASK_PROMPT_BYTES)
        .and_then(|json| serde_json::from_str(&json).ok())
    else {
        return fallback(facts);
    };
    let answer = match brain.run(WorkerTask::DependencyReview, &input).await {
        Ok(reply) => parse(&reply.text, &seen),
        Err(_) => Err("Claude unavailable"),
    };
    match answer {
        Ok((priorities, summary)) => (priorities, (summary, SummarySource::Claude)),
        Err(reason) => {
            tracing::warn!(reason, "dependency review: Claude's answer not used");
            fallback(facts)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    priorities: Vec<AnswerPriority>,
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerPriority {
    #[serde(rename = "ref")]
    reference: String,
    rank: u8,
    reason: String,
}

/// Non-blank, at most `max` characters, nothing hidden; line breaks only
/// where `multiline`.
fn text(value: &str, max: usize, multiline: bool) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max
        && if multiline {
            value.split('\n').all(crate::validation::no_hidden_chars)
        } else {
            crate::validation::no_hidden_chars(value)
        }
}

/// Claude's answer, held to the rules as a whole: exactly the asked JSON,
/// at most 20 priorities ranked 1, 2, 3 … without gaps, each naming a fact
/// the call was given at most once, bounded one-line reasons and a bounded
/// summary. Anything else is refused.
fn parse(text_: &str, seen: &DependencyFacts) -> Result<(Vec<Priority>, String), &'static str> {
    let answer: Answer =
        serde_json::from_str(text_.trim()).map_err(|_| "not the JSON object asked for")?;
    if answer.priorities.len() > MAX_PRIORITIES {
        return Err("too many priorities");
    }
    let mut priorities = answer.priorities;
    priorities.sort_by_key(|priority| priority.rank);
    let known = |reference: &str| {
        seen.alerts.iter().any(|alert| alert.reference == reference)
            || seen
                .pull_requests
                .iter()
                .any(|pull| pull.reference == reference)
    };
    for (i, priority) in priorities.iter().enumerate() {
        if usize::from(priority.rank) != i + 1 {
            return Err("ranks are not 1, 2, 3 without gaps");
        }
        if !known(&priority.reference)
            || priorities[..i]
                .iter()
                .any(|earlier| earlier.reference == priority.reference)
        {
            return Err("a ref is unknown or repeated");
        }
        if !text(&priority.reason, MAX_REASON, false) {
            return Err("a reason is empty, too long or not one line");
        }
    }
    if !text(&answer.summary, MAX_SUMMARY, true) {
        return Err("the summary is empty or too long");
    }
    let priorities = priorities
        .into_iter()
        .map(|priority| Priority {
            reference: priority.reference,
            rank: priority.rank,
            reason: priority.reason,
        })
        .collect();
    Ok((priorities, answer.summary))
}

/// Core's own order: alerts with a fixed version first, then by severity,
/// each with a templated reason, and a templated summary.
fn fallback(facts: &DependencyFacts) -> (Vec<Priority>, (String, SummarySource)) {
    let mut alerts: Vec<&DependencyAlert> = facts.alerts.iter().collect();
    // Stable: within a group the facts' order (repo, number) stays.
    alerts.sort_by_key(|alert| (Reverse(alert.patched.is_some()), Reverse(alert.severity)));
    let priorities = alerts
        .into_iter()
        .take(MAX_PRIORITIES)
        .zip(1..)
        .map(|(alert, rank)| Priority {
            reference: alert.reference.clone(),
            rank,
            reason: reason(alert, facts),
        })
        .collect();
    (priorities, (summary(facts), SummarySource::Deterministic))
}

fn reason(alert: &DependencyAlert, facts: &DependencyFacts) -> String {
    let severity = alert.severity.as_str();
    let pull = alert.pr.as_deref().and_then(|reference| {
        facts
            .pull_requests
            .iter()
            .find(|pull| pull.reference == reference)
    });
    match (&alert.patched, pull) {
        (Some(version), Some(pull)) => format!(
            "{severity} severity; fixed in {version}; Dependabot pull request #{} is open",
            pull.number
        ),
        (Some(version), None) => format!("{severity} severity; fixed in {version}"),
        (None, _) => format!("{severity} severity; no fixed version yet"),
    }
}

fn summary(facts: &DependencyFacts) -> String {
    let count = |severity| {
        facts
            .alerts
            .iter()
            .filter(|alert| alert.severity == severity)
            .count()
    };
    let fixable = facts
        .alerts
        .iter()
        .filter(|alert| alert.patched.is_some())
        .count();
    let mut summary = format!(
        "Open Dependabot alerts: {} ({} critical, {} high, {} medium, {} low), {fixable} \
         with a fixed version. Open Dependabot pull requests: {}.",
        facts.alerts.len(),
        count(Severity::Critical),
        count(Severity::High),
        count(Severity::Medium),
        count(Severity::Low),
        facts.pull_requests.len(),
    );
    if !facts.errors.is_empty() {
        summary.push_str(&format!(" Repositories not read: {}.", facts.errors.len()));
    }
    if facts.truncated {
        summary.push_str(" Some alerts or pull requests were left out.");
    }
    summary
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use jarvis_llm::{ChatReply, LlmError};
    use serde_json::{json, Value};

    use super::*;
    use crate::roadmaps::github::{DependabotPull, RepoError};

    /// Answers with `reply` (or fails without one) and keeps every input.
    struct FakeBrain {
        reply: Option<String>,
        inputs: Mutex<Vec<(WorkerTask, Value)>>,
    }

    impl FakeBrain {
        fn new(reply: Option<&str>) -> Self {
            Self {
                reply: reply.map(str::to_owned),
                inputs: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Brain for FakeBrain {
        async fn run(&self, task: WorkerTask, input: &Value) -> Result<ChatReply, LlmError> {
            self.inputs.lock().unwrap().push((task, input.clone()));
            let text = self.reply.clone().ok_or(LlmError::Empty)?;
            Ok(ChatReply {
                text,
                model: "claude-test".into(),
                backend: Some("claude-cli".into()),
                requested_route: None,
                actual_provider: None,
                stop_reason: None,
                usage: None,
                fallback_count: 0,
            })
        }
    }

    fn alert(n: usize, severity: Severity, patched: Option<&str>) -> DependencyAlert {
        DependencyAlert {
            reference: format!("a{n}"),
            repo: "Owner/Repo".into(),
            number: n as u64,
            package: format!("crate-{n}"),
            ecosystem: "rust".into(),
            manifest: Some("Cargo.lock".into()),
            severity,
            ghsa: Some("GHSA-abcd-efgh-ij23".into()),
            cve: None,
            vulnerable: Some("< 1.0.0".into()),
            patched: patched.map(str::to_owned),
            pr: None,
        }
    }

    /// Ordered as the reader orders them: most severe first, fixable first.
    fn facts() -> DependencyFacts {
        let mut first = alert(1, Severity::Critical, Some("1.0.1"));
        first.pr = Some("p1".into());
        DependencyFacts {
            alerts: vec![
                first,
                alert(2, Severity::High, None),
                alert(3, Severity::Medium, Some("2.0.0")),
                alert(4, Severity::Low, Some("0.3.1")),
            ],
            pull_requests: vec![DependabotPull {
                reference: "p1".into(),
                repo: "Owner/Repo".into(),
                number: 234,
                package: Some("crate-1".into()),
                from: Some("1.0.0".into()),
                to: Some("1.0.1".into()),
                ..DependabotPull::default()
            }],
            alerts_read: 1,
            ..DependencyFacts::default()
        }
    }

    fn answer(priorities: Value, summary: &str) -> String {
        json!({"priorities": priorities, "summary": summary}).to_string()
    }

    #[tokio::test]
    async fn claude_orders_the_facts_once_within_the_rules() {
        let reply = answer(
            json!([
                {"ref": "p1", "rank": 2, "reason": "Merge the open update."},
                {"ref": "a1", "rank": 1, "reason": "Critical and fixed in 1.0.1."}
            ]),
            "Merge #234 first.\nThen look at crate-2.",
        );
        let brain = FakeBrain::new(Some(&reply));
        let (priorities, (summary, source)) = review(&brain, &facts()).await;
        assert_eq!(source, SummarySource::Claude);
        assert_eq!(summary, "Merge #234 first.\nThen look at crate-2.");
        assert_eq!(
            priorities
                .iter()
                .map(|p| (p.reference.as_str(), p.rank))
                .collect::<Vec<_>>(),
            [("a1", 1), ("p1", 2)]
        );
        // One call, the fixed task, the facts as data.
        let inputs = brain.inputs.lock().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].0, WorkerTask::DependencyReview);
        assert_eq!(inputs[0].1["alerts"][0]["ref"], "a1");
    }

    #[tokio::test]
    async fn an_answer_that_breaks_a_rule_falls_back_to_core_order() {
        let ok = json!({"ref": "a1", "rank": 1, "reason": "Fix it."});
        let many: Vec<Value> = (1..=21)
            .map(|rank| json!({"ref": format!("a{}", (rank - 1) % 4 + 1), "rank": rank, "reason": "r"}))
            .collect();
        for reply in [
            format!("Here you go: {}", answer(json!([ok]), "Done.")),
            json!({"priorities": [ok], "summary": "Done.", "tools": []}).to_string(),
            answer(
                json!([{"ref": "a1", "rank": 1, "reason": "r", "url": "x"}]),
                "Done.",
            ),
            answer(json!([{"ref": "a9", "rank": 1, "reason": "r"}]), "Done."),
            answer(json!([{"ref": "x1", "rank": 1, "reason": "r"}]), "Done."),
            answer(
                json!([ok, {"ref": "a1", "rank": 2, "reason": "r"}]),
                "Done.",
            ),
            answer(json!([{"ref": "a1", "rank": 2, "reason": "r"}]), "Done."),
            answer(
                json!([ok, {"ref": "a2", "rank": 1, "reason": "r"}]),
                "Done.",
            ),
            answer(
                json!([{"ref": "a1", "rank": 1, "reason": "r".repeat(301)}]),
                "Done.",
            ),
            answer(
                json!([{"ref": "a1", "rank": 1, "reason": "one\ntwo"}]),
                "Done.",
            ),
            answer(json!([{"ref": "a1", "rank": 1, "reason": " "}]), "Done."),
            answer(json!([ok]), ""),
            answer(json!([ok]), &"s".repeat(2001)),
            answer(json!([ok]), "Hidden\u{202E}text"),
            answer(json!(many), "Done."),
            "not json".to_owned(),
        ] {
            let brain = FakeBrain::new(Some(&reply));
            let (_, (_, source)) = review(&brain, &facts()).await;
            assert_eq!(source, SummarySource::Deterministic, "{reply}");
        }
        // No answer at all.
        let (_, (_, source)) = review(&FakeBrain::new(None), &facts()).await;
        assert_eq!(source, SummarySource::Deterministic);
    }

    #[tokio::test]
    async fn claude_sees_at_most_16_kib_and_only_refs_it_saw_count() {
        let mut big = facts();
        big.alerts = (1..=200)
            .map(|n| {
                let severity = if n <= 100 {
                    Severity::High
                } else {
                    Severity::Low
                };
                alert(n, severity, Some("1.0.1"))
            })
            .collect();
        let brain = FakeBrain::new(Some(&answer(
            json!([{"ref": "a200", "rank": 1, "reason": "Least severe."}]),
            "Done.",
        )));
        let (_, (_, source)) = review(&brain, &big).await;
        // a200 was cut from the input, so the answer is refused.
        assert_eq!(source, SummarySource::Deterministic);
        let inputs = brain.inputs.lock().unwrap();
        let input = &inputs[0].1;
        assert!(input.to_string().len() <= MAX_TASK_PROMPT_BYTES);
        assert_eq!(input["truncated"], true);
        let sent = input["alerts"].as_array().unwrap();
        assert!(sent.len() < 200);
        assert_eq!(sent[0]["ref"], "a1");
    }

    #[tokio::test]
    async fn the_fallback_puts_fixable_alerts_first_then_severity() {
        let brain = FakeBrain::new(None);
        let mut facts = facts();
        facts.errors.push(RepoError {
            repo: "Owner/Other".into(),
            error_code: "github_unavailable",
        });
        let (priorities, (summary, source)) = review(&brain, &facts).await;
        assert_eq!(source, SummarySource::Deterministic);
        assert_eq!(
            priorities
                .iter()
                .map(|p| (p.reference.as_str(), p.rank, p.reason.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "a1",
                    1,
                    "critical severity; fixed in 1.0.1; Dependabot pull request #234 is open"
                ),
                ("a3", 2, "medium severity; fixed in 2.0.0"),
                ("a4", 3, "low severity; fixed in 0.3.1"),
                ("a2", 4, "high severity; no fixed version yet"),
            ]
        );
        assert_eq!(
            summary,
            "Open Dependabot alerts: 4 (1 critical, 1 high, 1 medium, 1 low), 3 with a fixed \
             version. Open Dependabot pull requests: 1. Repositories not read: 1."
        );
        // Nothing to order: no call.
        let empty = DependencyFacts::default();
        let (priorities, (summary, _)) = review(&brain, &empty).await;
        assert!(priorities.is_empty());
        assert!(summary.starts_with("Open Dependabot alerts: 0 "));
        assert_eq!(brain.inputs.lock().unwrap().len(), 1);
        // The fallback always passes the store's rules.
        let finish = Finish {
            summary: Some((summary, SummarySource::Deterministic)),
            priorities,
            ..Finish::failed("x")
        };
        assert!(finish.check().is_ok());
    }

    #[tokio::test]
    async fn without_a_token_or_allowlist_the_run_fails_as_not_configured() {
        let work = Work {
            github: Arc::default(),
            token: None,
            brain: Arc::new(FakeBrain::new(None)),
        };
        let job =
            serde_json::from_value::<super::super::NewJob>(json!({"kind": "dependency_review"}))
                .unwrap()
                .into_job("j".into(), time::OffsetDateTime::now_utc(), None)
                .unwrap();
        assert_eq!(
            run(&work, &Database::init(), &job).await,
            Finish::failed("not_configured")
        );
    }

    #[test]
    fn repository_failures_make_a_run_partial_or_failed() {
        let failed = |repo: &str| RepoError {
            repo: repo.into(),
            error_code: "token_needs_dependabot_alerts_read",
        };
        let mut facts = DependencyFacts {
            alerts_read: 2,
            ..DependencyFacts::default()
        };
        let ok = record(&mut facts);
        assert_eq!((ok.status, ok.error_code), (RunStatus::Ok, None));
        assert_eq!(
            ok.facts.as_deref(),
            Some(r#"{"alerts":[],"pull_requests":[],"errors":[],"truncated":false}"#)
        );
        // One repository failed: the other's alerts were read.
        facts.errors.push(failed("a/b"));
        facts.alerts_read = 1;
        let partial = record(&mut facts);
        assert_eq!(
            (partial.status, partial.error_code),
            (
                RunStatus::Partial,
                Some("token_needs_dependabot_alerts_read")
            )
        );
        // Alerts read (none open) but its pull request list failed: partial.
        facts.errors[0].error_code = "github_access_refused";
        assert_eq!(record(&mut facts).status, RunStatus::Partial);
        facts.errors.push(failed("a/c"));
        facts.alerts_read = 0;
        assert_eq!(record(&mut facts).status, RunStatus::Failed);
    }

    fn snapshot(items: Vec<crate::roadmaps::Item>) -> Snapshot {
        let now = time::OffsetDateTime::now_utc();
        Snapshot {
            roadmap: crate::roadmaps::Roadmap {
                id: uuid::Uuid::now_v7().to_string(),
                title: "Plan".into(),
                description: None,
                phases: Vec::new(),
                revision: 3,
                created_at: now,
                updated_at: now,
                archived_at: None,
            },
            items,
        }
    }

    fn item(title: &str, links: Vec<Link>) -> crate::roadmaps::Item {
        crate::roadmaps::Item {
            id: uuid::Uuid::now_v7().to_string(),
            title: title.into(),
            status: crate::roadmaps::Status::Planned,
            auto_status: None,
            phase: None,
            position: 0,
            notes: None,
            depends_on: Vec::new(),
            links,
            updated_at: time::OffsetDateTime::now_utc(),
        }
    }

    fn pr_link(repo: &str, number: u64) -> Link {
        Link {
            kind: LinkKind::Pr,
            repo: Some(repo.into()),
            number: Some(number),
            tag: None,
            url: None,
            state: None,
        }
    }

    fn titles(ops: &[ProposalOp]) -> Vec<&str> {
        ops.iter()
            .map(|op| match op {
                ProposalOp::CreateItem { item } => item.title.as_str(),
                ProposalOp::UpdateItem { .. } => panic!("an update"),
            })
            .collect()
    }

    #[test]
    fn proposals_hold_fixable_upgrades_once_and_at_most_twenty() {
        let facts = facts();
        let ops = proposal_ops(&snapshot(Vec::new()), &[], &facts);
        assert_eq!(
            titles(&ops),
            [
                "Upgrade crate-1 to 1.0.1 (Owner/Repo)",
                "Upgrade crate-3 to 2.0.0 (Owner/Repo)",
                "Upgrade crate-4 to 0.3.1 (Owner/Repo)",
            ]
        );
        // Linked to the Dependabot pull request when there is one.
        let ProposalOp::CreateItem { item: first } = &ops[0] else {
            panic!("a create");
        };
        assert_eq!(first.links, [pr_link("Owner/Repo", 234)]);
        // Already on the roadmap by title (any case) or by pull request, or
        // in a pending proposal: left out.
        let on_roadmap = snapshot(vec![
            item_on("upgrade CRATE-3 to 2.0.0 (owner/repo)"),
            item("Ship crate-1", vec![pr_link("owner/repo", 234)]),
        ]);
        let pending = Proposal {
            id: uuid::Uuid::now_v7().to_string(),
            base_revision: 3,
            ops: vec![ops[2].clone()],
            rationale: None,
            created_at: time::OffsetDateTime::now_utc(),
            expires_at: time::OffsetDateTime::now_utc(),
        };
        assert!(proposal_ops(&on_roadmap, &[pending], &facts).is_empty());
        // Twenty at most, and the same upgrade twice only once.
        let mut many = facts.clone();
        many.alerts = (1..=30)
            .map(|n| alert(n, Severity::High, Some("1.0.0")))
            .collect();
        many.alerts.push(alert(1, Severity::Low, Some("1.0.0")));
        let ops = proposal_ops(&snapshot(Vec::new()), &[], &many);
        assert_eq!(ops.len(), MAX_PROPOSAL_OPS);
        assert_eq!(
            titles(&ops).iter().collect::<BTreeSet<_>>().len(),
            MAX_PROPOSAL_OPS
        );
        // A title over the roadmap's bound is left out.
        let mut long = facts.clone();
        long.alerts[0].package = "p".repeat(190);
        assert_eq!(proposal_ops(&snapshot(Vec::new()), &[], &long).len(), 2);
    }

    fn item_on(title: &str) -> crate::roadmaps::Item {
        item(title, Vec::new())
    }

    async fn database() -> Result<Database, Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("jobs_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_run_proposes_once_dedupes_and_refuses_a_changed_roadmap(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::roadmaps::{store, NewRoadmap, RoadmapPatch};
        let db = database().await?;
        let hub = crate::realtime::Hub::default();
        let device = uuid::Uuid::now_v7().to_string();
        let plan = NewRoadmap {
            title: "Plan".into(),
            description: None,
            phases: Vec::new(),
        };
        let roadmap = store::create(&db, &plan, &device, &hub).await?;
        let id = roadmap.roadmap.id.clone();

        // A roadmap that changed since it was read refuses the proposal.
        let stale = store::get(&db, &id).await?.ok_or("roadmap")?;
        store::update(
            &db,
            &id,
            1,
            &RoadmapPatch {
                title: Some("Plan B".into()),
                ..RoadmapPatch::default()
            },
            &device,
            &hub,
        )
        .await?;
        assert_eq!(
            submit(&db, &stale, &[], &facts()).await,
            Err("roadmap_changed")
        );
        assert!(store::list_proposals(&db, &id).await?.is_empty());

        // One proposal; the next run finds it pending and proposes nothing.
        let proposal = propose(&db, &id, &facts()).await?.ok_or("proposal")?;
        let pending = store::list_proposals(&db, &id).await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, proposal);
        assert_eq!(pending[0].ops.len(), 3);
        assert_eq!(propose(&db, &id, &facts()).await, Ok(None));
        // Accepted, the items dedupe the next run as well.
        store::accept_proposal(&db, &id, &proposal, &device, &Default::default(), &hub).await?;
        assert_eq!(propose(&db, &id, &facts()).await, Ok(None));
        let unknown = uuid::Uuid::now_v7().to_string();
        assert_eq!(
            propose(&db, &unknown, &facts()).await,
            Err("roadmap_unavailable")
        );
        Ok(())
    }
}
