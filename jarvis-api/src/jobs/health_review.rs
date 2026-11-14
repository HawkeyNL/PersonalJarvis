//! The `health_review` job, the morning check (see `docs/SCHEDULED_JOBS.md`):
//! Core reads its own signals and a static rule table turns them into
//! findings with stable ids. Only fixed codes, allowlisted names, counts and
//! times are read or kept: never message text, audit detail, device ids,
//! addresses, model output or anything of the host beyond the Health page's
//! disks and a fixed list of units. No model is called.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_llm::claude_worker_protocol;
use jarvis_store::Database;
use jarvis_usage::UsageFailures;

use super::store::{self, Finish, RunOutcome};
use super::{Cadence, Job, Kind, RunStatus, SummarySource, Trigger, DB_TIMEOUT, MAX_SUMMARY};
use crate::roadmaps::github::PollState;
use crate::AppState;

/// The IBKR status asks the broker (3 s) and the database (10 s) at once.
const IBKR_TIMEOUT: Duration = Duration::from_secs(12);
/// Most findings a run keeps, most urgent and severe first.
const MAX_FINDINGS: usize = 50;
const DAY_HOURS: u32 = 24;
const WEEK_HOURS: u32 = 7 * 24;
/// Units whose `failed` state is a `service_failed` finding; the updater and
/// the backup have findings of their own.
const SERVICES: [&str; 6] = [
    "jarvis-core.service",
    "jarvis-surrealdb.service",
    "jarvis-config-broker.service",
    "jarvis-codex-broker.service",
    "jarvis-opensandbox.service",
    "jarvis-private-agent-updater.service",
];
/// A timer in one of these states does not run.
const TIMER_OFF: [&str; 3] = ["inactive", "failed", "not_found"];
/// Run error codes the owner can act on; any `roadmap_*` code too.
const ACTIONABLE_CODES: [&str; 5] = [
    "token_needs_dependabot_alerts_read",
    "github_token_refused",
    "not_configured",
    "research_unavailable",
    "disk_unavailable",
];
/// Security audit events the check counts; their outcomes are matched below.
const AUDIT_EVENTS: [&str; 11] = [
    "auth.login",
    "device.pairing_request",
    "auth.bootstrap",
    "audit.unauthenticated",
    "unlock.deny",
    "unlock.approve",
    "account.password_change",
    "account.recovery.request",
    "local.device.mutation",
    "local.account.recovery.confirm",
    "local.account.recovery.deny",
];
/// IBKR status codes that are findings, and how severe; `sync_disabled` and
/// `first_sync_pending` are not.
const IBKR: [(&str, Severity, bool); 15] = [
    ("config_refused", Severity::High, false),
    ("broker_not_installed", Severity::Medium, false),
    ("credentials_missing", Severity::High, false),
    ("broker_not_enabled", Severity::Medium, false),
    ("broker_failed", Severity::High, false),
    ("broker_unreachable", Severity::High, false),
    ("broker_restart_needed", Severity::Medium, false),
    ("auth_rejected", Severity::High, true),
    ("token_expired", Severity::High, true),
    ("session_competing", Severity::Medium, false),
    ("rate_limited", Severity::Low, false),
    ("account_unsupported", Severity::Medium, false),
    ("sync_failing", Severity::High, false),
    ("sync_stale", Severity::Medium, false),
    ("core_store_unavailable", Severity::High, false),
];
/// Model failure categories that count towards `llm_unavailable`.
const UNAVAILABLE: [&str; 3] = ["unavailable", "model_unavailable", "timeout"];

/// Read the signals of the last day (a week for a scheduled weekly run) and
/// record the findings. `ok` when every source was read, `partial` with
/// `source_unavailable` when some were, `failed` with `signals_unavailable`
/// when none were.
pub(super) async fn run(state: &AppState, job: &Job, trigger: Trigger) -> Finish {
    let hours = if job.cadence == Cadence::Weekly && trigger == Trigger::Schedule {
        WEEK_HOURS
    } else {
        DAY_HOURS
    };
    finish(&collect(state, hours, trigger == Trigger::Schedule).await)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// One thing to fix. `id` is stable for the same problem: a code and, for
/// some, an allowlisted subject (`[a-z0-9_.:-]`).
#[derive(Debug, Clone, PartialEq, Serialize)]
struct Finding {
    id: String,
    severity: Severity,
    urgent: bool,
    /// From a fixed template; in the summary, not in the facts.
    #[serde(skip)]
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u64>,
    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_at: Option<String>,
}

impl Finding {
    fn new(id: impl Into<String>, severity: Severity, urgent: bool, title: String) -> Self {
        Self {
            id: id.into(),
            severity,
            urgent,
            title,
            count: None,
            last_at: None,
        }
    }

    fn counted(mut self, count: u64, last_at: Option<String>) -> Self {
        self.count = Some(count);
        self.last_at = last_at;
        self
    }
}

/// Security audit rows of one event and outcome: never `detail` or
/// `device_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct AuditCount {
    event: String,
    outcome: String,
    count: u64,
    #[serde(with = "time::serde::rfc3339")]
    last_at: OffsetDateTime,
}

/// What one check read. A source that could not be read is empty here and
/// `false` in `sources`.
#[derive(Debug, Clone)]
struct Signals {
    window_hours: u32,
    sources: BTreeMap<&'static str, bool>,
    jobs: Vec<Job>,
    runs: Vec<RunOutcome>,
    /// Label and used percentage.
    disks: Vec<(&'static str, f64)>,
    /// Unit and state, from a fixed vocabulary.
    units: HashMap<&'static str, &'static str>,
    agent_registry_degraded: bool,
    /// Code and fixed summary of each IBKR problem.
    ibkr: Vec<(&'static str, &'static str)>,
    claude_worker: bool,
    research: bool,
    llm_failures: Vec<UsageFailures>,
    push_failures: u64,
    audit: Vec<AuditCount>,
    github: PollState,
    memecoin_enabled: bool,
    memecoin_running: bool,
    memecoin_failing: bool,
    spent_cents: u64,
    budget_cents: u64,
}

/// A database read within [`DB_TIMEOUT`]; `None` when it failed.
async fn bounded<T, E: std::fmt::Display>(
    source: &'static str,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Option<T> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!(source, %error, "morning check: source unavailable");
            None
        }
        Err(_) => {
            tracing::warn!(source, "morning check: source timed out");
            None
        }
    }
}

/// Only a scheduled run takes the push failure count; the owner's run reads
/// it and leaves it for the next scheduled one.
async fn collect(state: &AppState, hours: u32, scheduled: bool) -> Signals {
    let since = OffsetDateTime::now_utc() - time::Duration::hours(hours.into());
    let db = &state.db;
    let (jobs, runs, disks, units, ibkr, llm_failures, audit, claude_worker) = tokio::join!(
        bounded("jobs", store::list(db)),
        bounded("runs", store::runs_since(db, since)),
        crate::routes::services::used_percent(),
        crate::routes::services::review_unit_states(crate::routes::services::SYSTEMCTL),
        tokio::time::timeout(IBKR_TIMEOUT, crate::ibkr::status::collect(state)),
        bounded("llm_usage", jarvis_usage::failures_since(db, hours)),
        bounded("security_audit", audit_counts(db, since)),
        tokio::fs::try_exists(claude_worker_protocol::SOCKET),
    );
    let ibkr = ibkr.ok().map(|collected| {
        crate::ibkr::status::problems(&collected)
            .into_iter()
            .map(|problem| (problem.code, problem.summary))
            .collect::<Vec<_>>()
    });
    let sources = BTreeMap::from([
        ("jobs", jobs.is_some()),
        ("runs", runs.is_some()),
        ("disks", !disks.is_empty()),
        ("services", units.is_some()),
        ("ibkr", ibkr.is_some()),
        ("llm_usage", llm_failures.is_some()),
        ("security_audit", audit.is_some()),
    ]);
    Signals {
        window_hours: hours,
        sources,
        jobs: jobs.unwrap_or_default(),
        runs: runs.unwrap_or_default(),
        disks,
        units: units.unwrap_or_default(),
        agent_registry_degraded: state
            .agent_registry
            .as_deref()
            .is_none_or(|loader| loader.is_degraded()),
        ibkr: ibkr.unwrap_or_default(),
        claude_worker: claude_worker.unwrap_or(false),
        research: state.llm.can_research() && state.model_routing.snapshot().research_allowed(),
        llm_failures: llm_failures.unwrap_or_default(),
        push_failures: state.notifier.delivery_failures(scheduled),
        audit: audit.unwrap_or_default(),
        github: state.github.status().state,
        memecoin_enabled: state.memecoin.enabled(),
        memecoin_running: state.memecoin.running(),
        memecoin_failing: state.memecoin.last_error().is_some(),
        spent_cents: state.spent_cents.load(std::sync::atomic::Ordering::Relaxed),
        budget_cents: state.budget_cents,
    }
}

/// Counts of the allowlisted security events since `since`.
async fn audit_counts(
    db: &Database,
    since: OffsetDateTime,
) -> Result<Vec<AuditCount>, surrealdb::Error> {
    let since = since.format(&Rfc3339).unwrap_or_default();
    db.query(
        "SELECT event, outcome, count() AS count, time::max(ts) AS last_at FROM security_audit \
         WHERE ts > <datetime>$since AND event IN $events GROUP BY event, outcome",
    )
    .bind(json!({"since": since, "events": AUDIT_EVENTS}))
    .await?
    .check()?
    .take(0)
}

/// The id part and the display name of a kind.
fn kind_name(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::DependencyReview => ("dependency_review", "Dependency review"),
        Kind::Prompt => ("prompt", "Research job"),
        Kind::Reminder => ("reminder", "Reminder"),
        Kind::DiskCheck => ("disk_check", "Disk check"),
        Kind::HealthReview => ("health_review", "Morning check"),
    }
}

fn rfc3339(at: OffsetDateTime) -> Option<String> {
    at.format(&Rfc3339).ok()
}

/// A model backend id as a finding subject; anything else is left out.
fn subject(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// The rule table: every finding the signals show, most urgent and severe
/// first, at most [`MAX_FINDINGS`].
fn findings(s: &Signals) -> Vec<Finding> {
    use Severity::{Critical, High, Low, Medium};
    let mut found = Vec::new();

    for (label, used) in &s.disks {
        let percent = used.floor().clamp(0.0, 100.0) as u8;
        let title = format!("Disk {label} is {percent}% full");
        if *used >= 97.0 {
            found.push(Finding::new(
                format!("disk_full:{label}"),
                Critical,
                true,
                title,
            ));
        } else if *used >= 90.0 {
            found.push(Finding::new(
                format!("disk_full:{label}"),
                High,
                false,
                title,
            ));
        }
    }

    let unit = |name: &str| s.units.get(name).copied();
    for name in SERVICES {
        if unit(name) == Some("failed") {
            found.push(Finding::new(
                format!("service_failed:{name}"),
                High,
                false,
                format!("Service {name} has failed"),
            ));
        }
    }
    let off = |name: &str| unit(name).is_some_and(|state| TIMER_OFF.contains(&state));
    if unit("jarvis-updater.service") == Some("failed") {
        found.push(Finding::new(
            "updater_failed",
            High,
            false,
            "The Core updater has failed".into(),
        ));
    }
    if off("jarvis-updater.timer") {
        found.push(Finding::new(
            "updater_timer_off",
            Medium,
            false,
            "The Core update timer does not run".into(),
        ));
    }
    if unit("jarvis-backup.service") == Some("failed") {
        found.push(Finding::new(
            "backup_failed",
            Critical,
            true,
            "The last backup failed".into(),
        ));
    }
    if off("jarvis-backup.timer") {
        found.push(Finding::new(
            "backup_not_enabled",
            Low,
            false,
            "Backups do not run on a timer".into(),
        ));
    }
    if s.agent_registry_degraded {
        found.push(Finding::new(
            "agent_registry_degraded",
            Medium,
            false,
            "The agent bundle did not load".into(),
        ));
    }
    match s.github {
        PollState::AuthFailed => found.push(Finding::new(
            "github_auth_failed",
            High,
            false,
            "GitHub refused the roadmap token".into(),
        )),
        PollState::Error | PollState::RateLimited => found.push(Finding::new(
            "github_poll_error",
            Low,
            false,
            "The GitHub poller cannot read GitHub".into(),
        )),
        PollState::NotConfigured | PollState::Starting | PollState::Ok => {}
    }

    for job in s.jobs.iter().filter(|job| job.paused_reason.is_some()) {
        found.push(Finding::new(
            format!("job_paused:{}", job.id),
            Medium,
            false,
            format!("{} paused after failed runs", kind_name(job.kind).1),
        ));
    }
    type Name = (&'static str, &'static str);
    let mut failing: BTreeMap<(Name, &str), (u64, OffsetDateTime)> = BTreeMap::new();
    let mut missed: Option<(u64, OffsetDateTime)> = None;
    for run in &s.runs {
        let add = |entry: &mut (u64, OffsetDateTime)| {
            entry.0 += 1;
            entry.1 = entry.1.max(run.started_at);
        };
        match (run.status, run.error_code.as_deref()) {
            (RunStatus::Failed | RunStatus::Partial, Some(code))
                if ACTIONABLE_CODES.contains(&code) || code.starts_with("roadmap_") =>
            {
                add(failing
                    .entry((kind_name(run.kind), code))
                    .or_insert((0, run.started_at)));
            }
            (RunStatus::Missed, _) => add(missed.get_or_insert((0, run.started_at))),
            _ => {}
        }
    }
    for (((kind, name), code), (count, last)) in failing {
        if code == "research_unavailable" && count < 2 {
            continue;
        }
        found.push(
            Finding::new(
                format!("job_failing:{kind}:{code}"),
                Medium,
                false,
                format!("{name} runs fail with {code}"),
            )
            .counted(count, rfc3339(last)),
        );
    }
    if let Some((count, last)) = missed {
        found.push(
            Finding::new(
                "jobs_missed",
                Low,
                false,
                format!("{count} scheduled runs were missed"),
            )
            .counted(count, rfc3339(last)),
        );
    }

    for (code, summary) in &s.ibkr {
        if let Some((_, severity, urgent)) = IBKR.iter().find(|entry| entry.0 == *code) {
            found.push(Finding::new(
                format!("ibkr:{code}"),
                *severity,
                *urgent,
                (*summary).to_owned(),
            ));
        }
    }

    let enabled = |kinds: &[Kind]| {
        s.jobs
            .iter()
            .any(|job| job.enabled && kinds.contains(&job.kind))
    };
    if !s.research && enabled(&[Kind::Prompt]) {
        found.push(Finding::new(
            "research_without_worker",
            Medium,
            false,
            "Research jobs cannot run: Research is off".into(),
        ));
    }
    if !s.claude_worker && enabled(&[Kind::Prompt, Kind::DependencyReview]) {
        found.push(Finding::new(
            "claude_worker_off",
            Medium,
            false,
            "The Claude worker is off while jobs need it".into(),
        ));
    }

    let mut unavailable: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
    for failure in s.llm_failures.iter().filter(|f| subject(&f.backend)) {
        let backend = failure.backend.as_str();
        if failure.category == "auth" {
            found.push(
                Finding::new(
                    format!("llm_auth:{backend}"),
                    High,
                    true,
                    format!("Model backend {backend} refused its credentials"),
                )
                .counted(failure.requests, Some(failure.last_at.clone())),
            );
        } else if UNAVAILABLE.contains(&failure.category.as_str()) {
            let entry = unavailable.entry(backend).or_insert((0, ""));
            entry.0 += failure.requests;
            entry.1 = entry.1.max(failure.last_at.as_str());
        }
    }
    for (backend, (count, last)) in unavailable {
        if count >= 5 {
            found.push(
                Finding::new(
                    format!("llm_unavailable:{backend}"),
                    Medium,
                    false,
                    format!("Model backend {backend} failed {count} times"),
                )
                .counted(count, Some(last.to_owned())),
            );
        }
    }

    if s.push_failures >= 3 {
        found.push(
            Finding::new(
                "push_failing",
                Medium,
                false,
                format!("{} pushes were not delivered", s.push_failures),
            )
            .counted(s.push_failures, None),
        );
    }

    let audit = |events: &[&str], outcomes: &[&str]| {
        s.audit
            .iter()
            .filter(|row| {
                events.contains(&row.event.as_str()) && outcomes.contains(&row.outcome.as_str())
            })
            .fold(
                (0, None),
                |(count, last): (u64, Option<OffsetDateTime>), row| {
                    (count + row.count, last.max(Some(row.last_at)))
                },
            )
    };
    // id, events, outcomes, the level a count reaches, title
    type Rule = (
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        fn(u64) -> Option<(Severity, bool)>,
        fn(u64) -> String,
    );
    let rules: [Rule; 9] = [
        (
            "security_login_failures",
            &["auth.login"],
            &["fail"],
            |n| match n {
                10.. => Some((High, true)),
                3.. => Some((Medium, false)),
                _ => None,
            },
            |n| format!("{n} failed logins"),
        ),
        (
            "security_pairing_password_failed",
            &["device.pairing_request"],
            &["password_failed"],
            |n| (n >= 3).then_some((High, true)),
            |n| format!("{n} pairing requests with a wrong password"),
        ),
        (
            "security_bootstrap_attempts",
            &["auth.bootstrap"],
            &["invalid_secret", "rejected_network"],
            |n| (n >= 1).then_some((High, true)),
            |n| format!("{n} refused first-owner setup attempts"),
        ),
        (
            "security_audit_flood",
            &["audit.unauthenticated"],
            &["dropped"],
            |n| (n >= 1).then_some((High, true)),
            |_| "Anonymous requests flooded the security log".into(),
        ),
        (
            "security_unlock_denied",
            &["unlock.deny"],
            &["ok"],
            |n| (n >= 1).then_some((Medium, false)),
            |n| format!("{n} unlock requests were denied"),
        ),
        (
            "security_unlock_failed",
            &["unlock.approve"],
            &["fail"],
            |n| (n >= 3).then_some((Medium, false)),
            |n| format!("{n} unlock approvals failed"),
        ),
        (
            "security_password_failed",
            &["account.password_change"],
            &["password_failed"],
            |n| (n >= 3).then_some((High, false)),
            |n| format!("{n} password changes failed"),
        ),
        (
            "security_recovery_requested",
            &["account.recovery.request"],
            // One request writes `prepared`, then `signed`.
            &["prepared"],
            |n| (n >= 1).then_some((Medium, false)),
            |_| "Account recovery was requested".into(),
        ),
        (
            "local_device_denied",
            &[
                "local.device.mutation",
                "local.account.recovery.confirm",
                "local.account.recovery.deny",
            ],
            &["denied"],
            |n| (n >= 3).then_some((Medium, false)),
            |n| format!("{n} local device changes were refused"),
        ),
    ];
    for (id, events, outcomes, level, title) in rules {
        let (count, last) = audit(events, outcomes);
        if let Some((severity, urgent)) = level(count) {
            found.push(
                Finding::new(id, severity, urgent, title(count))
                    .counted(count, last.and_then(rfc3339)),
            );
        }
    }

    if s.memecoin_enabled && !s.memecoin_running {
        found.push(Finding::new(
            "memecoin_not_running",
            Medium,
            false,
            "The memecoin paper trader is not running".into(),
        ));
    }
    if s.memecoin_enabled && s.memecoin_failing {
        found.push(Finding::new(
            "memecoin_tick_failing",
            Medium,
            false,
            "The memecoin paper trader is failing".into(),
        ));
    }
    if s.budget_cents > 0 && s.spent_cents >= s.budget_cents {
        found.push(Finding::new(
            "budget_over",
            Medium,
            false,
            "The monthly model budget is used up".into(),
        ));
    }

    found.sort_by_key(|finding| Reverse((finding.urgent, finding.severity)));
    found.truncate(MAX_FINDINGS);
    found
}

/// The run's record: its facts and a templated summary.
fn finish(s: &Signals) -> Finish {
    let read = s.sources.values().filter(|ok| **ok).count();
    if read == 0 {
        return Finish::failed("signals_unavailable");
    }
    let found = findings(s);
    let unread: Vec<&str> = s
        .sources
        .iter()
        .filter(|(_, ok)| !**ok)
        .map(|(name, _)| *name)
        .collect();
    let facts = json!({
        "window_hours": s.window_hours,
        "sources": s.sources.iter()
            .map(|(name, ok)| (*name, if *ok { "ok" } else { "unavailable" }))
            .collect::<BTreeMap<_, _>>(),
        "findings": found,
    });
    let mut text = match found.len() {
        0 => "Nothing to fix.".to_owned(),
        n => {
            let urgent = found.iter().filter(|finding| finding.urgent).count();
            let titles = found
                .iter()
                .map(|finding| finding.title.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "{n} {} to fix ({urgent} urgent): {titles}.",
                if n == 1 { "thing" } else { "things" }
            )
        }
    };
    if !unread.is_empty() {
        text.push_str(&format!(" Could not read: {}.", unread.join(", ")));
    }
    let summary: String = text.chars().take(MAX_SUMMARY).collect();
    Finish {
        status: if unread.is_empty() {
            RunStatus::Ok
        } else {
            RunStatus::Partial
        },
        error_code: (!unread.is_empty()).then_some("source_unavailable"),
        facts: Some(facts.to_string()),
        summary: Some((summary, SummarySource::Deterministic)),
        priorities: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;
    use crate::jobs::{NewJob, PausedReason};

    fn at(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).unwrap()
    }

    fn job(kind: &str, extra: serde_json::Value) -> Job {
        let mut body = json!({"kind": kind});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        serde_json::from_value::<NewJob>(body)
            .unwrap()
            .into_job(
                uuid::Uuid::now_v7().to_string(),
                at("2026-11-11T12:00:00Z"),
                None,
            )
            .unwrap()
    }

    fn prompt() -> Job {
        job(
            "prompt",
            json!({"params": {"title": "News", "instruction": "Summarize the AI news"}}),
        )
    }

    fn sources(ok: bool) -> BTreeMap<&'static str, bool> {
        [
            "jobs",
            "runs",
            "disks",
            "services",
            "ibkr",
            "llm_usage",
            "security_audit",
        ]
        .into_iter()
        .map(|name| (name, ok))
        .collect()
    }

    /// Every source read, nothing wrong.
    fn healthy() -> Signals {
        Signals {
            window_hours: 24,
            sources: sources(true),
            jobs: vec![prompt(), job("dependency_review", json!({}))],
            runs: Vec::new(),
            disks: vec![("system", 40.0), ("data", 89.9)],
            units: crate::routes::services::REVIEW_UNITS
                .into_iter()
                .map(|unit| (unit, "active"))
                .collect(),
            agent_registry_degraded: false,
            ibkr: vec![("sync_disabled", "off")],
            claude_worker: true,
            research: true,
            llm_failures: Vec::new(),
            push_failures: 2,
            audit: Vec::new(),
            github: PollState::Ok,
            memecoin_enabled: false,
            memecoin_running: false,
            memecoin_failing: true,
            spent_cents: 100,
            budget_cents: 0,
        }
    }

    fn ids(signals: &Signals) -> Vec<String> {
        findings(signals).into_iter().map(|f| f.id).collect()
    }

    fn with(change: impl FnOnce(&mut Signals)) -> Vec<(String, Severity, bool)> {
        let mut signals = healthy();
        change(&mut signals);
        findings(&signals)
            .into_iter()
            .map(|f| (f.id, f.severity, f.urgent))
            .collect()
    }

    fn one(id: &str, severity: Severity, urgent: bool) -> Vec<(String, Severity, bool)> {
        vec![(id.to_owned(), severity, urgent)]
    }

    fn run(kind: Kind, status: RunStatus, code: Option<&str>, started: &str) -> RunOutcome {
        RunOutcome {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind,
            status,
            error_code: code.map(str::to_owned),
            started_at: at(started),
        }
    }

    fn audit(event: &str, outcome: &str, count: u64) -> AuditCount {
        AuditCount {
            event: event.into(),
            outcome: outcome.into(),
            count,
            last_at: at("2026-11-14T05:00:00Z"),
        }
    }

    fn failures(backend: &str, category: &str, requests: u64) -> UsageFailures {
        UsageFailures {
            backend: backend.into(),
            category: category.into(),
            requests,
            last_at: "2026-11-14T05:00:00+00:00".into(),
        }
    }

    #[test]
    fn a_healthy_node_has_nothing_to_fix() {
        assert!(ids(&healthy()).is_empty());
        let finish = finish(&healthy());
        assert_eq!((finish.status, finish.error_code), (RunStatus::Ok, None));
        assert_eq!(
            finish.summary,
            Some(("Nothing to fix.".into(), SummarySource::Deterministic))
        );
        let facts: serde_json::Value = serde_json::from_str(&finish.facts.unwrap()).unwrap();
        assert_eq!(
            facts,
            json!({
                "window_hours": 24,
                "sources": {"disks": "ok", "ibkr": "ok", "jobs": "ok", "llm_usage": "ok",
                            "runs": "ok", "security_audit": "ok", "services": "ok"},
                "findings": [],
            })
        );
        assert!(finish.priorities.is_empty());
    }

    #[test]
    fn host_rules_and_their_thresholds() {
        use Severity::{Critical, High, Low, Medium};
        assert_eq!(
            with(|s| s.disks = vec![("system", 90.0), ("data", 96.9)]),
            [
                ("disk_full:system".into(), High, false),
                ("disk_full:data".into(), High, false)
            ]
        );
        assert_eq!(
            with(|s| s.disks = vec![("data", 97.0)]),
            one("disk_full:data", Critical, true)
        );
        let unit = |name: &'static str, state: &'static str| {
            with(move |s| {
                s.units.insert(name, state);
            })
        };
        assert_eq!(
            unit("jarvis-surrealdb.service", "failed"),
            one("service_failed:jarvis-surrealdb.service", High, false)
        );
        assert!(unit("jarvis-surrealdb.service", "inactive").is_empty());
        assert_eq!(
            unit("jarvis-updater.service", "failed"),
            one("updater_failed", High, false)
        );
        assert!(unit("jarvis-updater.service", "inactive").is_empty());
        for state in ["inactive", "failed", "not_found"] {
            assert_eq!(
                unit("jarvis-updater.timer", state),
                one("updater_timer_off", Medium, false)
            );
            assert_eq!(
                unit("jarvis-backup.timer", state),
                one("backup_not_enabled", Low, false)
            );
        }
        assert!(unit("jarvis-backup.timer", "unknown").is_empty());
        assert_eq!(
            unit("jarvis-backup.service", "failed"),
            one("backup_failed", Critical, true)
        );
        // Units systemctl did not answer for say nothing.
        assert!(with(|s| s.units.clear()).is_empty());
        assert_eq!(
            with(|s| s.agent_registry_degraded = true),
            one("agent_registry_degraded", Medium, false)
        );
        assert_eq!(
            with(|s| s.github = PollState::AuthFailed),
            one("github_auth_failed", High, false)
        );
        for state in [PollState::Error, PollState::RateLimited] {
            assert_eq!(
                with(|s| s.github = state),
                one("github_poll_error", Low, false)
            );
        }
        for state in [PollState::NotConfigured, PollState::Starting] {
            assert!(with(|s| s.github = state).is_empty());
        }
    }

    #[test]
    fn job_rules_count_actionable_codes_only() {
        use Severity::{Low, Medium};
        let mut paused = prompt();
        paused.enabled = false;
        paused.paused_reason = Some(PausedReason::PausedAfterFailures);
        let id = paused.id.clone();
        let found = findings(&Signals {
            jobs: vec![paused],
            ..healthy()
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, format!("job_paused:{id}"));
        assert_eq!(found[0].title, "Research job paused after failed runs");

        let found = findings(&Signals {
            runs: vec![
                run(
                    Kind::DependencyReview,
                    RunStatus::Failed,
                    Some("github_token_refused"),
                    "2026-11-14T05:00:00Z",
                ),
                run(
                    Kind::DependencyReview,
                    RunStatus::Partial,
                    Some("roadmap_limit"),
                    "2026-11-14T05:00:00Z",
                ),
                run(
                    Kind::Prompt,
                    RunStatus::Failed,
                    Some("research_unavailable"),
                    "2026-11-14T05:00:00Z",
                ),
                run(
                    Kind::Prompt,
                    RunStatus::Failed,
                    Some("private_instruction"),
                    "2026-11-14T05:00:00Z",
                ),
                run(Kind::Prompt, RunStatus::Ok, None, "2026-11-14T05:00:00Z"),
                run(
                    Kind::DiskCheck,
                    RunStatus::Missed,
                    None,
                    "2026-11-13T05:00:00Z",
                ),
                run(
                    Kind::Reminder,
                    RunStatus::Missed,
                    None,
                    "2026-11-14T04:00:00Z",
                ),
            ],
            ..healthy()
        });
        let summary: Vec<_> = found
            .iter()
            .map(|f| (f.id.as_str(), f.severity, f.count, f.last_at.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "job_failing:dependency_review:github_token_refused",
                    Medium,
                    Some(1),
                    Some("2026-11-14T05:00:00Z")
                ),
                (
                    "job_failing:dependency_review:roadmap_limit",
                    Medium,
                    Some(1),
                    Some("2026-11-14T05:00:00Z")
                ),
                ("jobs_missed", Low, Some(2), Some("2026-11-14T04:00:00Z")),
            ]
        );
        // A research outage counts from the second failed run.
        let twice = with(|s| {
            s.runs = (0..2)
                .map(|_| {
                    run(
                        Kind::Prompt,
                        RunStatus::Failed,
                        Some("research_unavailable"),
                        "2026-11-14T05:00:00Z",
                    )
                })
                .collect()
        });
        assert_eq!(
            twice,
            one("job_failing:prompt:research_unavailable", Medium, false)
        );
    }

    #[test]
    fn ibkr_problems_map_onto_fixed_findings() {
        use Severity::{High, Low, Medium};
        for (code, severity, urgent) in [
            ("config_refused", High, false),
            ("broker_not_installed", Medium, false),
            ("credentials_missing", High, false),
            ("broker_not_enabled", Medium, false),
            ("broker_failed", High, false),
            ("broker_unreachable", High, false),
            ("broker_restart_needed", Medium, false),
            ("auth_rejected", High, true),
            ("token_expired", High, true),
            ("session_competing", Medium, false),
            ("rate_limited", Low, false),
            ("account_unsupported", Medium, false),
            ("sync_failing", High, false),
            ("sync_stale", Medium, false),
            ("core_store_unavailable", High, false),
        ] {
            let found = findings(&Signals {
                ibkr: vec![(code, "A fixed summary.")],
                ..healthy()
            });
            assert_eq!(found.len(), 1, "{code}");
            assert_eq!(
                (found[0].id.clone(), found[0].severity, found[0].urgent),
                (format!("ibkr:{code}"), severity, urgent)
            );
            assert_eq!(found[0].title, "A fixed summary.");
        }
        for code in ["sync_disabled", "first_sync_pending", "unknown"] {
            assert!(with(|s| s.ibkr = vec![(code, "x")]).is_empty(), "{code}");
        }
    }

    #[test]
    fn worker_model_push_and_trading_rules() {
        use Severity::{High, Medium};
        assert_eq!(
            with(|s| s.research = false),
            one("research_without_worker", Medium, false)
        );
        assert_eq!(
            with(|s| s.claude_worker = false),
            one("claude_worker_off", Medium, false)
        );
        // Only enabled jobs that need them count.
        assert!(with(|s| {
            s.research = false;
            s.claude_worker = false;
            s.jobs.iter_mut().for_each(|job| job.enabled = false);
        })
        .is_empty());
        let review_only = with(|s| {
            s.research = false;
            s.claude_worker = false;
            s.jobs = vec![job("dependency_review", json!({}))];
        });
        assert_eq!(review_only, one("claude_worker_off", Medium, false));

        assert_eq!(
            with(|s| s.llm_failures = vec![failures("claude-cli", "auth", 1)]),
            one("llm_auth:claude-cli", High, true)
        );
        assert_eq!(
            with(|s| s.llm_failures = vec![
                failures("ollama", "timeout", 2),
                failures("ollama", "unavailable", 2),
                failures("ollama", "rate_limited", 9),
                failures("laya", "model_unavailable", 4),
            ]),
            []
        );
        let five = findings(&Signals {
            llm_failures: vec![
                failures("ollama", "timeout", 3),
                failures("ollama", "model_unavailable", 2),
            ],
            ..healthy()
        });
        assert_eq!(
            (five[0].id.as_str(), five[0].count, five[0].severity),
            ("llm_unavailable:ollama", Some(5), Medium)
        );
        // A backend outside the subject charset is left out.
        assert!(with(|s| s.llm_failures = vec![failures("Bad Backend", "auth", 1)]).is_empty());
        assert!(with(|s| s.llm_failures = vec![failures(&"a".repeat(33), "auth", 1)]).is_empty());

        assert_eq!(
            with(|s| s.push_failures = 3),
            one("push_failing", Medium, false)
        );
        assert_eq!(
            with(|s| {
                s.memecoin_enabled = true;
                s.memecoin_running = true;
            }),
            one("memecoin_tick_failing", Medium, false)
        );
        assert_eq!(
            with(|s| {
                s.memecoin_enabled = true;
                s.memecoin_failing = false;
            }),
            one("memecoin_not_running", Medium, false)
        );
        assert_eq!(
            with(|s| s.budget_cents = 100),
            one("budget_over", Medium, false)
        );
        assert!(with(|s| s.budget_cents = 101).is_empty());
    }

    #[test]
    fn security_rules_count_allowlisted_events_at_their_edges() {
        use Severity::{High, Medium};
        let rows = |rows: Vec<AuditCount>| with(|s| s.audit = rows);
        assert!(rows(vec![audit("auth.login", "fail", 2)]).is_empty());
        assert_eq!(
            rows(vec![audit("auth.login", "fail", 3)]),
            one("security_login_failures", Medium, false)
        );
        assert_eq!(
            rows(vec![audit("auth.login", "fail", 9)]),
            one("security_login_failures", Medium, false)
        );
        assert_eq!(
            rows(vec![audit("auth.login", "fail", 10)]),
            one("security_login_failures", High, true)
        );
        assert!(rows(vec![audit("auth.login", "ok", 50)]).is_empty());
        assert!(rows(vec![audit("device.pairing_request", "password_failed", 2)]).is_empty());
        assert_eq!(
            rows(vec![audit("device.pairing_request", "password_failed", 3)]),
            one("security_pairing_password_failed", High, true)
        );
        assert_eq!(
            rows(vec![
                audit("auth.bootstrap", "ok", 1),
                audit("auth.bootstrap", "rejected_network", 1)
            ]),
            one("security_bootstrap_attempts", High, true)
        );
        assert_eq!(
            rows(vec![audit("audit.unauthenticated", "dropped", 1)]),
            one("security_audit_flood", High, true)
        );
        assert_eq!(
            rows(vec![audit("unlock.deny", "ok", 1)]),
            one("security_unlock_denied", Medium, false)
        );
        assert!(rows(vec![audit("unlock.approve", "fail", 2)]).is_empty());
        assert_eq!(
            rows(vec![audit("unlock.approve", "fail", 3)]),
            one("security_unlock_failed", Medium, false)
        );
        assert_eq!(
            rows(vec![audit("account.password_change", "password_failed", 3)]),
            one("security_password_failed", High, false)
        );
        assert_eq!(
            rows(vec![audit("account.recovery.request", "prepared", 1)]),
            one("security_recovery_requested", Medium, false)
        );
        let local = findings(&Signals {
            audit: vec![
                audit("local.device.mutation", "denied", 1),
                audit("local.account.recovery.deny", "denied", 2),
                audit("local.account.recovery.deny", "ok", 5),
            ],
            ..healthy()
        });
        assert_eq!(
            (local[0].id.as_str(), local[0].count),
            ("local_device_denied", Some(3))
        );
        assert_eq!(local[0].last_at.as_deref(), Some("2026-11-14T05:00:00Z"));
    }

    #[test]
    fn findings_are_ordered_capped_and_summarised() {
        let mut signals = healthy();
        signals.disks = vec![("data", 99.0)];
        signals.github = PollState::Error;
        signals.agent_registry_degraded = true;
        signals.sources.insert("ibkr", false);
        signals.sources.insert("services", false);
        let done = finish(&signals);
        assert_eq!(
            (done.status, done.error_code),
            (RunStatus::Partial, Some("source_unavailable"))
        );
        assert_eq!(
            done.summary.unwrap().0,
            "3 things to fix (1 urgent): Disk data is 99% full; The agent bundle did not \
             load; The GitHub poller cannot read GitHub. Could not read: ibkr, services."
        );
        let facts: serde_json::Value = serde_json::from_str(&done.facts.unwrap()).unwrap();
        assert_eq!(facts["sources"]["ibkr"], "unavailable");
        assert_eq!(
            facts["findings"][0],
            json!({"id": "disk_full:data", "severity": "critical", "urgent": true})
        );

        let many = Signals {
            jobs: (0..30)
                .map(|_| {
                    let mut job = prompt();
                    job.enabled = false;
                    job.paused_reason = Some(PausedReason::PausedAfterFailures);
                    job
                })
                .collect(),
            disks: vec![("data", 92.0)],
            // Long titles: more than the summary holds.
            llm_failures: (0..20)
                .map(|i| failures(&format!("{i:0>32}"), "timeout", 5))
                .collect(),
            ..healthy()
        };
        let found = findings(&many);
        assert_eq!(found.len(), MAX_FINDINGS);
        assert_eq!(found[0].id, "disk_full:data");
        let done = finish(&many);
        assert!(done.check().is_ok());
        let summary = done.summary.unwrap().0;
        assert!(summary.starts_with("50 things to fix (0 urgent): Disk data is 92% full; "));
        assert_eq!(summary.chars().count(), MAX_SUMMARY);

        let one_thing = finish(&Signals {
            push_failures: 4,
            ..healthy()
        });
        assert_eq!(
            one_thing.summary.unwrap().0,
            "1 thing to fix (0 urgent): 4 pushes were not delivered."
        );
        let none_read = finish(&Signals {
            sources: sources(false),
            ..healthy()
        });
        assert_eq!(none_read, Finish::failed("signals_unavailable"));
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn database_sources_keep_codes_and_counts_only(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("health_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let audit_row = |event: &str, outcome: &str, age: &str| {
            format!(
                "CREATE security_audit SET id = '{}', ts = time::now() - {age}, \
                 device_id = 'DEVICE-SECRET', event = '{event}', outcome = '{outcome}', \
                 detail = 'DETAIL-SECRET';",
                uuid::Uuid::now_v7()
            )
        };
        for row in [
            audit_row("auth.login", "fail", "1h"),
            audit_row("auth.login", "fail", "2h"),
            audit_row("auth.login", "fail", "3h"),
            audit_row("auth.login", "fail", "2d"),
            audit_row("auth.login", "ok", "1h"),
            audit_row("jobs.created", "ok", "1h"),
            audit_row("local.device.mutation", "denied", "1h"),
        ] {
            db.query(row).await?.check()?;
        }
        let usage_row = |backend: &str, category: &str, age: &str| {
            format!(
                "CREATE llm_usage SET id = '{}', ts = time::now() - {age}, backend = '{backend}', \
                 model = 'MODEL-SECRET', input_tokens = 0, output_tokens = 0, \
                 cache_read_tokens = 0, cache_write_tokens = 0, cost_eur = 0.0, \
                 status = 'failed', failure_category = '{category}', \
                 request_id = 'REQUEST-SECRET';",
                uuid::Uuid::now_v7()
            )
        };
        for row in [
            usage_row("claude-cli", "auth", "1h"),
            usage_row("claude-cli", "auth", "2h"),
            usage_row("ollama", "timeout", "1h"),
            usage_row("ollama", "timeout", "3d"),
        ] {
            db.query(row).await?.check()?;
        }

        let since = OffsetDateTime::now_utc() - time::Duration::hours(24);
        let mut audit = audit_counts(&db, since).await?;
        audit.sort_by(|a, b| (&a.event, &a.outcome).cmp(&(&b.event, &b.outcome)));
        assert_eq!(
            audit
                .iter()
                .map(|row| (row.event.as_str(), row.outcome.as_str(), row.count))
                .collect::<Vec<_>>(),
            [
                ("auth.login", "fail", 3),
                ("auth.login", "ok", 1),
                ("local.device.mutation", "denied", 1),
            ]
        );
        assert!(audit.iter().all(|row| row.last_at > since));
        let mut llm = jarvis_usage::failures_since(&db, 24).await?;
        llm.sort_by(|a, b| a.backend.cmp(&b.backend));
        assert_eq!(
            llm.iter()
                .map(|row| (row.backend.as_str(), row.category.as_str(), row.requests))
                .collect::<Vec<_>>(),
            [("claude-cli", "auth", 2), ("ollama", "timeout", 1)]
        );
        assert!(OffsetDateTime::parse(&llm[0].last_at, &Rfc3339).is_ok());

        let done = finish(&Signals {
            audit,
            llm_failures: llm,
            ..healthy()
        });
        let facts = done.facts.unwrap();
        assert!(facts.contains("security_login_failures") && facts.contains("llm_auth:claude-cli"));
        for secret in ["SECRET", "DEVICE", "DETAIL", "MODEL", "REQUEST"] {
            assert!(!facts.contains(secret), "{secret}");
            assert!(
                !done.summary.as_ref().unwrap().0.contains(secret),
                "{secret}"
            );
        }
        Ok(())
    }
}
