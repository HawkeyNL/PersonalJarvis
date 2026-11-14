//! The `health_review` job, the morning check (see `docs/SCHEDULED_JOBS.md`):
//! Core reads its own signals and a static rule table turns them into
//! findings with stable ids. Only fixed codes, allowlisted names, counts and
//! times are read or kept: never message text, audit detail, device ids,
//! addresses, model output or anything of the host beyond the Health page's
//! disks and a fixed list of units. No model is called. With a roadmap, a
//! run proposes the findings to it as items whose notes end with a marker
//! line, `Jarvis check: <finding id>`, and pushes a count when it newly
//! proposes something urgent.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use jarvis_llm::claude_worker_protocol;
use jarvis_store::Database;
use jarvis_usage::UsageFailures;

use super::dependency_review::{proposal_error, submit};
use super::store::{self, Finish, RunOutcome};
use super::{Cadence, Job, Kind, RunStatus, SummarySource, Trigger, DB_TIMEOUT, MAX_SUMMARY};
use crate::ibkr::status::Problem;
use crate::notify::{JobResult, NotifyEvent};
use crate::roadmaps::github::PollState;
use crate::roadmaps::store::Proposal;
use crate::roadmaps::{
    Item, ItemPatch, NewItem, ProposalOp, Snapshot, Status, MAX_ITEMS, MAX_ITEM_TITLE, MAX_NOTES,
    MAX_PROPOSAL_OPS,
};
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
/// IBKR status codes that are findings, and how severe at most (a problem
/// the IBKR status rates a warning is at most medium); `sync_disabled`,
/// `first_sync_pending` and `token_expired` (it renews on the next request)
/// are not.
const IBKR: [(&str, Severity, bool); 14] = [
    ("config_refused", Severity::High, false),
    ("broker_not_installed", Severity::Medium, false),
    ("credentials_missing", Severity::High, false),
    ("broker_not_enabled", Severity::Medium, false),
    ("broker_failed", Severity::High, false),
    ("broker_unreachable", Severity::High, false),
    ("broker_restart_needed", Severity::Medium, false),
    ("auth_rejected", Severity::High, true),
    ("session_competing", Severity::Medium, false),
    ("rate_limited", Severity::Low, false),
    ("account_unsupported", Severity::Medium, false),
    ("sync_failing", Severity::High, false),
    ("sync_stale", Severity::Medium, false),
    ("core_store_unavailable", Severity::High, false),
];
/// Model failure categories that count towards `llm_unavailable`.
const UNAVAILABLE: [&str; 3] = ["unavailable", "model_unavailable", "timeout"];
/// The last line of a proposed item's notes, before the finding id.
const MARKER: &str = "Jarvis check: ";
/// Longest finding id a marker holds.
const MAX_FINDING_ID: usize = 128;
/// How long a rejected proposal keeps its findings from being proposed again.
const SNOOZE: time::Duration = time::Duration::days(7);
const RATIONALE: &str = "From the morning check.";
const RESOLVES: &str = "The morning check sets this item to live once the problem is gone.";
const REOPENS: &str =
    "Set this item to live when it is fixed; the morning check reopens it if the problem comes back.";
const LOGS: &str = "Jarvis never reads logs; these commands are for you to run on the Home Node.";

/// Per finding code (an id up to its first `:`): whether the check sets the
/// item live once the finding is gone, and the owner's fixed fix steps;
/// `{subject}` is the id after the code (an allowlisted name). The `ibkr`
/// steps are followed by the IBKR status's own commands.
const RULES: [(&str, bool, &[&str]); 29] = [
    ("disk_full", true, &[
        "Disk {subject} is nearly full; Core and SurrealDB stop writing when it is full.",
        "See both disks on the Health page in Jarvis, or on the Home Node: df -h / /var/lib/jarvis",
        "Old backup archives are the usual candidates: see docs/BACKUP_AND_RESTORE.md and docs/HOUSEKEEPING.md.",
    ]),
    ("service_failed", true, &[
        "{subject} has failed. To see why:",
        "sudo systemctl status {subject}",
        "sudo journalctl -u {subject} -n 50 --no-pager",
        "Fix the cause, then: sudo systemctl restart {subject}",
        LOGS,
    ]),
    ("updater_failed", true, &[
        "The Core updater (jarvis-updater.service) has failed. To see why:",
        "sudo jarvis update --status",
        "sudo journalctl -u jarvis-updater.service -n 50 --no-pager",
        "Fix the cause, then: sudo systemctl reset-failed jarvis-updater.service",
        LOGS,
    ]),
    ("backup_failed", true, &[
        "The last backup (jarvis-backup.service) failed. To see why:",
        "sudo journalctl -u jarvis-backup.service -n 50 --no-pager",
        "Fix the cause (docs/BACKUP_AND_RESTORE.md), then back up by hand: sudo systemctl start jarvis-backup.service",
        LOGS,
    ]),
    ("backup_not_enabled", true, &[
        "Backups do not run by themselves. Set them up as docs/BACKUP_AND_RESTORE.md describes, then turn on the daily timer:",
        "sudo systemctl enable --now jarvis-backup.timer",
    ]),
    ("agent_registry_degraded", true, &[
        "Core could not load the agent bundle.",
        "See the agent bundle on the Health page in Jarvis, and Core's log: sudo jarvis logs core --lines 100",
        LOGS,
    ]),
    ("github_auth_failed", true, &[
        "GitHub refused the roadmap poller's token: it expired, was revoked or lost access.",
        "Make a new token and put it in place as docs/ROADMAPS.md (GitHub) describes, then: sudo systemctl restart jarvis-core.service",
    ]),
    ("github_poll_error", true, &[
        "The GitHub poller cannot read GitHub (an error or a rate limit); it tries again every 10 minutes.",
        "If it stays, check the Home Node's network and the token's access as docs/ROADMAPS.md (GitHub) describes.",
    ]),
    ("job_paused", true, &[
        "A scheduled job paused itself after failed runs.",
        "Open Jobs in Jarvis, see the error codes of its last runs (docs/SCHEDULED_JOBS.md), fix the cause and turn the job back on.",
    ]),
    ("job_failing", false, &[
        "Scheduled runs failed with the error code in the title.",
        "Open Jobs in Jarvis for the runs; docs/SCHEDULED_JOBS.md says what each code means.",
    ]),
    ("jobs_missed", false, &[
        "Scheduled runs were missed: Core was down or busy at their time.",
        "Check that Core runs on the Health page in Jarvis, and Core's log: sudo jarvis logs core --lines 100",
        LOGS,
    ]),
    ("ibkr", true, &[
        "The IBKR status in Jarvis shows this problem. See docs/IBKR_PORTFOLIO.md; on the Home Node:",
    ]),
    ("research_without_worker", true, &[
        "Research jobs are on, but Research cannot run: it is off or no subscription worker is set up.",
        "Turn Research on in Jarvis, or set up a worker as docs/SUBSCRIPTION_WORKERS.md describes.",
    ]),
    ("claude_worker_off", true, &[
        "The dependency review needs the Claude worker, but its socket is missing.",
        "Set it up as docs/SUBSCRIPTION_WORKERS.md describes, or start it: sudo systemctl enable --now jarvis-claude.socket",
    ]),
    ("llm_auth", false, &[
        "Model backend {subject} refused its credentials: a key or a sign-in expired or was revoked.",
        "Renew that backend's credentials (a subscription worker's sign-in: docs/SUBSCRIPTION_WORKERS.md).",
    ]),
    ("llm_unavailable", false, &[
        "Model backend {subject} was often unavailable or too slow.",
        "Check the provider's status and the Home Node's network; the usage on the Health page in Jarvis shows the failures.",
    ]),
    ("push_failing", false, &[
        "Pushes to your devices were not delivered.",
        "Check the ntfy server and the app's push settings as docs/NOTIFICATIONS.md describes.",
    ]),
    ("security_login_failures", false, &[
        "Logins to Jarvis failed again and again. If that was not you, someone may be guessing the password.",
        "Change the account password and check the paired devices in Jarvis.",
    ]),
    ("security_pairing_password_failed", false, &[
        "Pairing requests came with a wrong account password. If that was not you, someone is trying to pair a device.",
        "Deny pairing requests you do not know, change the account password and check the paired devices in Jarvis.",
    ]),
    ("security_bootstrap_attempts", false, &[
        "Someone tried the first-owner setup, which this Home Node no longer accepts.",
        "If that was not you, check which devices can reach Core on the network.",
    ]),
    ("security_audit_flood", false, &[
        "Anonymous requests came so fast that Core dropped some of their security log rows.",
        "Check which devices can reach Core on the network; something may be scanning it.",
    ]),
    ("security_unlock_denied", false, &[
        "A device asked to unlock Jarvis and was denied.",
        "If you do not know the request, check the paired devices in Jarvis and revoke one you do not trust.",
    ]),
    ("security_unlock_failed", false, &[
        "Unlock approvals failed.",
        "If that was not you, check the paired devices in Jarvis and revoke one you do not trust.",
    ]),
    ("security_password_failed", false, &[
        "Password changes failed with a wrong current password.",
        "If that was not you, a signed-in device is guessing it: check the paired devices in Jarvis.",
    ]),
    ("security_recovery_requested", false, &[
        "Account recovery was requested.",
        "If that was not you, deny it and check the paired devices in Jarvis.",
    ]),
    ("local_device_denied", false, &[
        "Device changes on the Home Node itself were refused.",
        "If that was not you, check who can log in on the Home Node.",
    ]),
    ("memecoin_not_running", true, &[
        "The memecoin paper trader is on but does not run.",
        "See docs/MEMECOIN_PAPER.md; if it stays, restart Core: sudo systemctl restart jarvis-core.service",
    ]),
    ("memecoin_tick_failing", true, &[
        "The memecoin paper trader's last tick failed.",
        "See docs/MEMECOIN_PAPER.md and Core's log: sudo jarvis logs core --lines 100",
        LOGS,
    ]),
    ("budget_over", true, &[
        "The month's model spend reached the budget; paid model calls stop until the next month.",
        "Wait for the next month, or raise JARVIS_LLM_MONTHLY_BUDGET_EUR in Core's configuration and restart Core.",
    ]),
];

/// Read the signals of the last day (a week for a scheduled weekly run),
/// record the findings and, when the job names a roadmap and a source was
/// read, propose them to it. `ok` when every source was read, `partial`
/// with `source_unavailable` when some were (or with a roadmap code when the
/// proposal failed), `failed` with `signals_unavailable` when none were.
pub(super) async fn run(state: &AppState, job: &Job, trigger: Trigger) -> Finish {
    let hours = if job.cadence == Cadence::Weekly && trigger == Trigger::Schedule {
        WEEK_HOURS
    } else {
        DAY_HOURS
    };
    let signals = collect(state, hours, trigger == Trigger::Schedule).await;
    let found = findings(&signals);
    let proposed = match &job.params.roadmap_id {
        Some(roadmap_id) if signals.sources.values().any(|ok| *ok) => {
            let now = OffsetDateTime::now_utc();
            propose(&state.db, roadmap_id, &signals, &found, now).await
        }
        _ => Ok(Proposed::default()),
    };
    if let Some((event, user)) = urgent_push(job, &proposed) {
        state.notifier.notify_user(event, user);
    }
    finish(&signals, &found, &proposed)
}

/// What a run proposed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Proposed {
    created: usize,
    reopened: usize,
    resolved: usize,
    /// Urgent findings among the created and reopened.
    urgent: usize,
}

/// The owner's push when the run newly proposed something urgent.
fn urgent_push(job: &Job, proposed: &Result<Proposed, &str>) -> Option<(NotifyEvent, Uuid)> {
    let urgent = proposed.as_ref().ok()?.urgent;
    let user = job.owner_id.as_deref()?.parse().ok()?;
    (urgent > 0).then(|| {
        let count = u8::try_from(urgent).unwrap_or(u8::MAX);
        (NotifyEvent::JobResult(JobResult::HealthUrgent(count)), user)
    })
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
    /// The IBKR status's fixed commands for an `ibkr:` finding.
    #[serde(skip)]
    commands: &'static [&'static str],
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
            commands: &[],
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
    ibkr: Vec<Problem>,
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
    let since_text = since.format(&Rfc3339).unwrap_or_default();
    let db = &state.db;
    let (jobs, runs, disks, units, ibkr, llm_failures, audit, claude_worker) = tokio::join!(
        bounded("jobs", store::list(db)),
        bounded("runs", store::runs_since(db, since)),
        crate::routes::services::used_percent(),
        crate::routes::services::review_unit_states(crate::routes::services::SYSTEMCTL),
        tokio::time::timeout(IBKR_TIMEOUT, crate::ibkr::status::collect(state)),
        bounded("llm_usage", jarvis_usage::failures_since(db, &since_text)),
        bounded("security_audit", audit_counts(db, since)),
        tokio::fs::try_exists(claude_worker_protocol::SOCKET),
    );
    let ibkr = ibkr
        .ok()
        .map(|collected| crate::ibkr::status::problems(&collected));
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
/// first.
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

    for problem in &s.ibkr {
        if let Some((_, severity, urgent)) = IBKR.iter().find(|entry| entry.0 == problem.code) {
            let mut finding = Finding::new(
                format!("ibkr:{}", problem.code),
                if problem.severity == crate::ibkr::status::Severity::Error {
                    *severity
                } else {
                    (*severity).min(Medium)
                },
                *urgent,
                problem.summary.to_owned(),
            );
            finding.commands = problem.commands;
            found.push(finding);
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
    // Research may run on another worker; `research_without_worker` covers
    // the research jobs.
    if !s.claude_worker && enabled(&[Kind::DependencyReview]) {
        found.push(Finding::new(
            "claude_worker_off",
            Medium,
            false,
            "The Claude worker is off while the dependency review needs it".into(),
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
    found
}

/// The run's record: its facts and a templated summary.
fn finish(s: &Signals, found: &[Finding], proposed: &Result<Proposed, &'static str>) -> Finish {
    let read = s.sources.values().filter(|ok| **ok).count();
    if read == 0 {
        return Finish::failed("signals_unavailable");
    }
    let found = &found[..found.len().min(MAX_FINDINGS)];
    let done = proposed.unwrap_or_default();
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
        "proposed": done.created,
        "reopened": done.reopened,
        "resolved": done.resolved,
    });
    let text = match found.len() {
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
    let mut tail = String::new();
    if done.created + done.reopened + done.resolved > 0 {
        tail.push_str(&format!(
            " Proposed to the roadmap: {} new, {} reopened, {} resolved.",
            done.created, done.reopened, done.resolved
        ));
    }
    if !unread.is_empty() {
        tail.push_str(&format!(" Could not read: {}.", unread.join(", ")));
    }
    let mut summary: String = text
        .chars()
        .take(MAX_SUMMARY - tail.chars().count())
        .collect();
    summary.push_str(&tail);
    let error_code = if unread.is_empty() {
        proposed.err()
    } else {
        Some("source_unavailable")
    };
    Finish {
        status: if error_code.is_none() {
            RunStatus::Ok
        } else {
            RunStatus::Partial
        },
        error_code,
        facts: Some(facts.to_string()),
        summary: Some((summary, SummarySource::Deterministic)),
        priorities: Vec::new(),
    }
}

/// The finding id a marker line names: `[a-z0-9_.:-]`, at most
/// [`MAX_FINDING_ID`] characters.
fn finding_id(value: &str) -> bool {
    (1..=MAX_FINDING_ID).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:-".contains(&b))
}

/// The finding id in the last line of an item's notes, exactly
/// `Jarvis check: <finding id>`.
fn marker(notes: &str) -> Option<&str> {
    let id = notes.rsplit('\n').next()?.strip_prefix(MARKER)?;
    finding_id(id).then_some(id)
}

/// A finding code's rule: whether it resolves and its fix steps.
fn rule(id: &str) -> Option<(bool, &'static [&'static str])> {
    let code = id.split_once(':').map_or(id, |(code, _)| code);
    RULES
        .iter()
        .find(|entry| entry.0 == code)
        .map(|entry| (entry.1, entry.2))
}

/// Whether this run would have seen the finding `id` had it been there, so
/// that its absence means it is gone: a source or unit that was not read
/// never resolves an item.
fn seen(s: &Signals, id: &str) -> bool {
    let (code, subject) = id.split_once(':').unwrap_or((id, ""));
    let source = |name: &str| s.sources.get(name).copied().unwrap_or(false);
    let unit = |name: &str| s.units.get(name).is_some_and(|state| *state != "unknown");
    match code {
        "disk_full" => s.disks.iter().any(|(label, _)| *label == subject),
        "service_failed" => unit(subject),
        "updater_failed" => unit("jarvis-updater.service"),
        "backup_failed" => unit("jarvis-backup.service"),
        "backup_not_enabled" => unit("jarvis-backup.timer"),
        "job_paused" | "research_without_worker" | "claude_worker_off" => source("jobs"),
        "ibkr" => source("ibkr"),
        "github_auth_failed" | "github_poll_error" => s.github != PollState::Starting,
        "agent_registry_degraded"
        | "memecoin_not_running"
        | "memecoin_tick_failing"
        | "budget_over" => true,
        _ => false,
    }
}

/// A new item for `finding`: its title, and notes of its fix steps, when
/// it was found and the marker line. `None` when either is over its bound.
fn new_item(finding: &Finding, today: Date) -> Option<NewItem> {
    let (resolves, steps) = rule(&finding.id)?;
    let subject = finding
        .id
        .split_once(':')
        .map_or("", |(_, subject)| subject);
    let mut notes = String::new();
    for line in steps.iter().chain(finding.commands) {
        notes.push_str(&line.replace("{subject}", subject));
        notes.push('\n');
    }
    notes.push_str(if resolves { RESOLVES } else { REOPENS });
    notes.push_str(&format!("\n\nFound on {today}"));
    if let Some(count) = finding.count {
        notes.push_str(&format!(", {count} times"));
    }
    notes.push_str(&format!(".\n{MARKER}{}", finding.id));
    (finding.title.chars().count() <= MAX_ITEM_TITLE && notes.chars().count() <= MAX_NOTES).then(
        || NewItem {
            title: finding.title.clone(),
            notes: Some(notes),
            ..NewItem::default()
        },
    )
}

fn set_status(item: &Item, status: Status) -> ProposalOp {
    ProposalOp::UpdateItem {
        item_id: item.id.clone(),
        patch: ItemPatch {
            status: Some(status),
            ..ItemPatch::default()
        },
    }
}

/// The operations of one proposal, at most 20. In the findings' order (most
/// urgent and severe first): a new item for a finding no item carries, or
/// `planned` for its `ready` or `live` item; then `live` for each open item
/// whose resolvable finding is gone from a source that was read. A finding
/// in a pending proposal's new item, or in one the owner rejected in the
/// last 7 days, is left out, and so is an item a pending or such a rejected
/// proposal changes. New items stop at the roadmap's room.
fn proposal_ops(
    snapshot: &Snapshot,
    pending: &[Proposal],
    rejected: &[Proposal],
    s: &Signals,
    found: &[Finding],
    today: Date,
) -> (Vec<ProposalOp>, Proposed) {
    let mut busy_ids = HashSet::new();
    let mut busy_items = HashSet::new();
    for op in pending
        .iter()
        .chain(rejected)
        .flat_map(|proposal| &proposal.ops)
    {
        match op {
            ProposalOp::CreateItem { item } => {
                busy_ids.extend(item.notes.as_deref().and_then(marker));
            }
            ProposalOp::UpdateItem { item_id, .. } => {
                busy_items.insert(item_id.as_str());
            }
        }
    }
    let open = |item: &Item| !matches!(item.status, Status::Ready | Status::Live);
    let marked: Vec<(&str, &Item)> = snapshot
        .items
        .iter()
        .filter_map(|item| Some((marker(item.notes.as_deref()?)?, item)))
        .collect();
    let mut room = MAX_ITEMS.saturating_sub(snapshot.items.len());
    let mut ops = Vec::new();
    let mut done = Proposed::default();
    for finding in found {
        let id = finding.id.as_str();
        if ops.len() == MAX_PROPOSAL_OPS {
            break;
        }
        if !finding_id(id) || !busy_ids.insert(id) {
            continue;
        }
        let mut items = marked
            .iter()
            .filter(|(m, _)| *m == id)
            .map(|(_, item)| *item);
        if items.clone().any(open) {
            continue;
        }
        if let Some(closed) = items.next() {
            if busy_items.contains(closed.id.as_str()) {
                continue;
            }
            ops.push(set_status(closed, Status::Planned));
            done.reopened += 1;
        } else {
            let Some(item) = new_item(finding, today).filter(|_| room > 0) else {
                continue;
            };
            room -= 1;
            ops.push(ProposalOp::CreateItem { item });
            done.created += 1;
        }
        done.urgent += usize::from(finding.urgent);
    }
    let present: HashSet<&str> = found.iter().map(|finding| finding.id.as_str()).collect();
    for (id, item) in marked {
        if ops.len() == MAX_PROPOSAL_OPS {
            break;
        }
        if open(item)
            && !present.contains(id)
            && !busy_items.contains(item.id.as_str())
            && rule(id).is_some_and(|(resolves, _)| resolves)
            && seen(s, id)
        {
            busy_items.insert(item.id.as_str());
            ops.push(set_status(item, Status::Live));
            done.resolved += 1;
        }
    }
    (ops, done)
}

/// Propose the findings to the roadmap, at most one proposal per run.
async fn propose(
    db: &Database,
    roadmap_id: &str,
    s: &Signals,
    found: &[Finding],
    now: OffsetDateTime,
) -> Result<Proposed, &'static str> {
    use super::dependency_review::bounded;
    use crate::roadmaps::store as roadmaps;
    let snapshot = bounded(roadmaps::get(db, roadmap_id))
        .await
        .map_err(proposal_error)?
        .filter(|snapshot| snapshot.roadmap.archived_at.is_none())
        .ok_or("roadmap_unavailable")?;
    let pending = bounded(roadmaps::list_proposals(db, roadmap_id))
        .await
        .map_err(proposal_error)?;
    let rejected = bounded(roadmaps::rejected_since(db, roadmap_id, now - SNOOZE))
        .await
        .map_err(proposal_error)?;
    let (ops, proposed) = proposal_ops(&snapshot, &pending, &rejected, s, found, now.date());
    submit(db, &snapshot, &ops, RATIONALE).await?;
    Ok(proposed)
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
            ibkr: vec![problem("sync_disabled", false)],
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

    fn problem(code: &'static str, error: bool) -> Problem {
        use crate::ibkr::status::Severity::{Error, Warn};
        Problem {
            code,
            severity: if error { Error } else { Warn },
            summary: "A fixed summary.",
            commands: &["sudo systemctl restart jarvis-core.service"],
        }
    }

    /// The run's record without a roadmap.
    fn done(s: &Signals) -> Finish {
        finish(s, &findings(s), &Ok(Proposed::default()))
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
        let finish = done(&healthy());
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
                "proposed": 0,
                "reopened": 0,
                "resolved": 0,
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
            // The owner installs Core by hand: an updater timer that does
            // not run is no finding.
            assert!(unit("jarvis-updater.timer", state).is_empty());
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
            ("session_competing", Medium, false),
            ("rate_limited", Low, false),
            ("account_unsupported", Medium, false),
            ("sync_failing", High, false),
            ("sync_stale", Medium, false),
            ("core_store_unavailable", High, false),
        ] {
            let found = findings(&Signals {
                ibkr: vec![problem(code, true)],
                ..healthy()
            });
            assert_eq!(found.len(), 1, "{code}");
            assert_eq!(
                (found[0].id.clone(), found[0].severity, found[0].urgent),
                (format!("ibkr:{code}"), severity, urgent)
            );
            assert_eq!(found[0].title, "A fixed summary.");
        }
        // A warning is at most medium: a sync failing for the first times.
        assert_eq!(
            with(|s| s.ibkr = vec![problem("sync_failing", false)]),
            one("ibkr:sync_failing", Medium, false)
        );
        assert_eq!(
            with(|s| s.ibkr = vec![problem("rate_limited", false)]),
            one("ibkr:rate_limited", Low, false)
        );
        for code in [
            "sync_disabled",
            "first_sync_pending",
            "token_expired",
            "unknown",
        ] {
            assert!(
                with(|s| s.ibkr = vec![problem(code, true)]).is_empty(),
                "{code}"
            );
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
        // Research jobs alone do not need the Claude worker.
        assert!(with(|s| {
            s.claude_worker = false;
            s.jobs = vec![prompt()];
        })
        .is_empty());
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
        let partial = done(&signals);
        assert_eq!(
            (partial.status, partial.error_code),
            (RunStatus::Partial, Some("source_unavailable"))
        );
        assert_eq!(
            partial.summary.unwrap().0,
            "3 things to fix (1 urgent): Disk data is 99% full; The agent bundle did not \
             load; The GitHub poller cannot read GitHub. Could not read: ibkr, services."
        );
        let facts: serde_json::Value = serde_json::from_str(&partial.facts.unwrap()).unwrap();
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
        assert_eq!(found.len(), MAX_FINDINGS + 1);
        assert_eq!(found[0].id, "disk_full:data");
        let capped = done(&many);
        assert!(capped.check().is_ok());
        let facts: serde_json::Value =
            serde_json::from_str(capped.facts.as_deref().unwrap()).unwrap();
        assert_eq!(facts["findings"].as_array().unwrap().len(), MAX_FINDINGS);
        let summary = capped.summary.unwrap().0;
        assert!(summary.starts_with("50 things to fix (0 urgent): Disk data is 92% full; "));
        assert_eq!(summary.chars().count(), MAX_SUMMARY);

        let one_thing = done(&Signals {
            push_failures: 4,
            ..healthy()
        });
        assert_eq!(
            one_thing.summary.unwrap().0,
            "1 thing to fix (0 urgent): 4 pushes were not delivered."
        );
        let none_read = done(&Signals {
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
        let mut llm = jarvis_usage::failures_since(&db, &since.format(&Rfc3339)?).await?;
        llm.sort_by(|a, b| a.backend.cmp(&b.backend));
        assert_eq!(
            llm.iter()
                .map(|row| (row.backend.as_str(), row.category.as_str(), row.requests))
                .collect::<Vec<_>>(),
            [("claude-cli", "auth", 2), ("ollama", "timeout", 1)]
        );
        assert!(OffsetDateTime::parse(&llm[0].last_at, &Rfc3339).is_ok());

        let recorded = done(&Signals {
            audit,
            llm_failures: llm,
            ..healthy()
        });
        let facts = recorded.facts.unwrap();
        assert!(facts.contains("security_login_failures") && facts.contains("llm_auth:claude-cli"));
        for secret in ["SECRET", "DEVICE", "DETAIL", "MODEL", "REQUEST"] {
            assert!(!facts.contains(secret), "{secret}");
            assert!(
                !recorded.summary.as_ref().unwrap().0.contains(secret),
                "{secret}"
            );
        }
        Ok(())
    }

    fn today() -> Date {
        Date::from_calendar_date(2026, time::Month::November, 15).unwrap()
    }

    /// An item whose id is `item-<marker>`.
    fn marked(id: &str, status: Status) -> Item {
        Item {
            id: format!("item-{id}"),
            title: id.into(),
            status,
            auto_status: None,
            phase: None,
            position: 0,
            notes: Some(format!("Fix it.\n{MARKER}{id}")),
            depends_on: Vec::new(),
            links: Vec::new(),
            updated_at: OffsetDateTime::now_utc(),
        }
    }

    fn snapshot(items: Vec<Item>) -> Snapshot {
        let now = OffsetDateTime::now_utc();
        Snapshot {
            roadmap: crate::roadmaps::Roadmap {
                id: Uuid::now_v7().to_string(),
                title: "Te fixen".into(),
                description: None,
                phases: Vec::new(),
                revision: 2,
                created_at: now,
                updated_at: now,
                archived_at: None,
            },
            items,
        }
    }

    fn proposal(ops: Vec<ProposalOp>) -> Proposal {
        Proposal {
            id: Uuid::now_v7().to_string(),
            base_revision: 2,
            ops,
            rationale: None,
            created_at: OffsetDateTime::now_utc(),
            expires_at: OffsetDateTime::now_utc(),
        }
    }

    fn create(id: &str) -> ProposalOp {
        ProposalOp::CreateItem {
            item: NewItem {
                title: "x".into(),
                notes: Some(format!("Fix it.\n{MARKER}{id}")),
                ..NewItem::default()
            },
        }
    }

    fn update(id: &str) -> ProposalOp {
        set_status(&marked(id, Status::Live), Status::Planned)
    }

    /// `create <marker>` or `<item id> <status>` per operation.
    fn describe(ops: &[ProposalOp]) -> Vec<String> {
        ops.iter()
            .map(|op| match op {
                ProposalOp::CreateItem { item } => {
                    format!("create {}", marker(item.notes.as_deref().unwrap()).unwrap())
                }
                ProposalOp::UpdateItem { item_id, patch } => {
                    format!("{item_id} {:?}", patch.status.unwrap())
                }
            })
            .collect()
    }

    fn ops(
        items: Vec<Item>,
        pending: &[Proposal],
        rejected: &[Proposal],
        s: &Signals,
    ) -> (Vec<String>, Proposed) {
        let (ops, done) = proposal_ops(
            &snapshot(items),
            pending,
            rejected,
            s,
            &findings(s),
            today(),
        );
        (describe(&ops), done)
    }

    /// An urgent disk, a high GitHub token and a medium push finding.
    fn three() -> Signals {
        Signals {
            disks: vec![("system", 40.0), ("data", 99.0)],
            github: PollState::AuthFailed,
            push_failures: 3,
            ..healthy()
        }
    }

    #[test]
    fn a_marker_is_exactly_the_last_line() {
        assert_eq!(
            marker("Fix it.\nJarvis check: disk_full:data"),
            Some("disk_full:data")
        );
        assert_eq!(
            marker("Jarvis check: job_paused:0193a2b4-1c2d-7e8f-9a0b-c1d2e3f4a5b6"),
            Some("job_paused:0193a2b4-1c2d-7e8f-9a0b-c1d2e3f4a5b6")
        );
        let longest = format!("Jarvis check: {}", "a".repeat(MAX_FINDING_ID));
        assert!(marker(&longest).is_some());
        for notes in [
            "Jarvis check: disk_full:data\n",
            "Jarvis check: disk_full:data\nThanks",
            "Jarvis check: Disk_full",
            "Jarvis check:  disk_full",
            "Jarvis check: disk full",
            "Jarvis check: ",
            "jarvis check: disk_full",
            "Note: Jarvis check: disk_full",
            "Jarvis check: disk_full\u{202E}",
            &format!("{longest}a"),
        ] {
            assert_eq!(marker(notes), None, "{notes}");
        }
    }

    #[test]
    fn every_finding_has_fixed_steps_within_the_item_bounds() {
        let codes: HashSet<&str> = RULES.iter().map(|entry| entry.0).collect();
        assert_eq!(codes.len(), RULES.len());
        let resolvable: HashSet<&str> = RULES
            .iter()
            .filter(|entry| entry.1)
            .map(|entry| entry.0)
            .collect();
        assert_eq!(
            resolvable,
            HashSet::from([
                "disk_full",
                "service_failed",
                "updater_failed",
                "backup_failed",
                "backup_not_enabled",
                "agent_registry_degraded",
                "github_auth_failed",
                "github_poll_error",
                "job_paused",
                "ibkr",
                "research_without_worker",
                "claude_worker_off",
                "memecoin_not_running",
                "memecoin_tick_failing",
                "budget_over",
            ])
        );
        // Every code with the longest subject, count and IBKR commands.
        let long: &'static str = Box::leak("c".repeat(250).into_boxed_str());
        for (code, _, _) in RULES {
            let mut finding = Finding::new(
                format!("{code}:{}", "s".repeat(MAX_FINDING_ID - code.len() - 1)),
                Severity::Low,
                false,
                "t".repeat(MAX_ITEM_TITLE),
            )
            .counted(u64::MAX, None);
            finding.commands = Box::leak(vec![long; 4].into_boxed_slice());
            let item = new_item(&finding, today()).unwrap();
            let notes = item.notes.unwrap();
            assert!(notes.chars().count() <= MAX_NOTES, "{code}");
            assert_eq!(marker(&notes), Some(finding.id.as_str()));
        }
        // A title or notes over the roadmap's bound is no item.
        let over = Finding::new("budget_over", Severity::Low, false, "t".repeat(201));
        assert_eq!(new_item(&over, today()), None);

        // Every finding the rules make, with its steps, its subject filled
        // in, when it was found and the marker last.
        let mut paused = prompt();
        paused.enabled = false;
        paused.paused_reason = Some(PausedReason::PausedAfterFailures);
        let everything = Signals {
            jobs: vec![paused, prompt(), job("dependency_review", json!({}))],
            runs: vec![
                run(
                    Kind::Prompt,
                    RunStatus::Missed,
                    None,
                    "2026-11-14T05:00:00Z",
                ),
                run(
                    Kind::DependencyReview,
                    RunStatus::Failed,
                    Some("roadmap_limit"),
                    "2026-11-14T05:00:00Z",
                ),
            ],
            disks: vec![("data", 95.0)],
            units: crate::routes::services::REVIEW_UNITS
                .into_iter()
                .map(|unit| (unit, "failed"))
                .collect(),
            agent_registry_degraded: true,
            ibkr: vec![problem("auth_rejected", true)],
            claude_worker: false,
            research: false,
            llm_failures: vec![
                failures("ollama", "auth", 1),
                failures("laya", "timeout", 5),
            ],
            push_failures: 3,
            audit: [
                ("auth.login", "fail"),
                ("device.pairing_request", "password_failed"),
                ("auth.bootstrap", "invalid_secret"),
                ("audit.unauthenticated", "dropped"),
                ("unlock.deny", "ok"),
                ("unlock.approve", "fail"),
                ("account.password_change", "password_failed"),
                ("account.recovery.request", "prepared"),
                ("local.device.mutation", "denied"),
            ]
            .into_iter()
            .map(|(event, outcome)| audit(event, outcome, 10))
            .collect(),
            github: PollState::AuthFailed,
            memecoin_enabled: true,
            memecoin_running: false,
            budget_cents: 100,
            ..healthy()
        };
        let found = findings(&everything);
        let found_codes: HashSet<&str> = found
            .iter()
            .map(|f| f.id.split_once(':').map_or(f.id.as_str(), |(code, _)| code))
            .collect();
        // All but `github_poll_error`: the poller is in one state.
        assert_eq!(found_codes.len(), RULES.len() - 1);
        for finding in &found {
            let item = new_item(finding, today()).unwrap();
            assert_eq!(item.title, finding.title);
            assert_eq!(item.status, Status::Planned);
            let notes = item.notes.unwrap();
            assert!(!notes.contains('{'), "{notes}");
            assert!(
                notes.ends_with(&format!(
                    "\nFound on 2026-11-15{}.\nJarvis check: {}",
                    finding
                        .count
                        .map(|n| format!(", {n} times"))
                        .unwrap_or_default(),
                    finding.id
                )),
                "{notes}"
            );
        }
        let disk = new_item(
            found.iter().find(|f| f.id == "disk_full:data").unwrap(),
            today(),
        )
        .unwrap()
        .notes
        .unwrap();
        assert!(disk.starts_with("Disk data is nearly full;"), "{disk}");
        assert!(disk.contains(RESOLVES));
        let ibkr = found.iter().find(|f| f.id == "ibkr:auth_rejected").unwrap();
        let notes = new_item(ibkr, today()).unwrap().notes.unwrap();
        assert!(
            notes.contains("\nsudo systemctl restart jarvis-core.service\n"),
            "{notes}"
        );
        let push = found.iter().find(|f| f.id == "push_failing").unwrap();
        assert!(new_item(push, today())
            .unwrap()
            .notes
            .unwrap()
            .contains(REOPENS));
        // The roadmap takes every one as it is.
        let mut draft = crate::roadmaps::Draft::new(snapshot(Vec::new()));
        for finding in &found {
            let item = new_item(finding, today()).unwrap();
            draft.apply(&ProposalOp::CreateItem { item }).unwrap();
        }
        draft.finish().unwrap();
    }

    #[test]
    fn findings_become_new_items_or_reopen_their_closed_item() {
        let (ops_, done) = ops(Vec::new(), &[], &[], &three());
        assert_eq!(
            ops_,
            [
                "create disk_full:data",
                "create github_auth_failed",
                "create push_failing"
            ]
        );
        assert_eq!(
            done,
            Proposed {
                created: 3,
                urgent: 1,
                ..Proposed::default()
            }
        );
        // An open item holds its finding; a ready or live one reopens.
        for status in [Status::Planned, Status::InProgress, Status::Blocked] {
            let (ops_, done) = ops(
                vec![
                    marked("disk_full:data", status),
                    marked("github_auth_failed", Status::Live),
                    marked("push_failing", Status::Ready),
                ],
                &[],
                &[],
                &three(),
            );
            assert_eq!(
                ops_,
                [
                    "item-github_auth_failed Planned",
                    "item-push_failing Planned"
                ]
            );
            assert_eq!((done.reopened, done.urgent), (2, 0));
        }
        // Two items with one marker: an open one holds it.
        let (ops_, _) = ops(
            vec![
                marked("disk_full:data", Status::Live),
                marked("disk_full:data", Status::Planned),
            ],
            &[],
            &[],
            &Signals {
                disks: vec![("data", 99.0)],
                ..healthy()
            },
        );
        assert!(ops_.is_empty());
        let (_, done) = ops(
            vec![marked("disk_full:data", Status::Ready)],
            &[],
            &[],
            &three(),
        );
        assert_eq!((done.created, done.reopened, done.urgent), (2, 1, 1));
    }

    #[test]
    fn pending_and_recently_rejected_proposals_hold_their_findings() {
        // In a pending proposal's new item.
        let (ops_, _) = ops(
            Vec::new(),
            &[proposal(vec![create("disk_full:data")])],
            &[],
            &three(),
        );
        assert_eq!(ops_, ["create github_auth_failed", "create push_failing"]);
        // Rejected within the snooze (the store reads only those).
        let (ops_, done) = ops(
            Vec::new(),
            &[],
            &[proposal(vec![
                create("disk_full:data"),
                create("push_failing"),
            ])],
            &three(),
        );
        assert_eq!(ops_, ["create github_auth_failed"]);
        assert_eq!(done.urgent, 0);
        // A closed item a pending or rejected proposal changes: no reopen and
        // no second item.
        for (pending, rejected) in [
            (vec![proposal(vec![update("github_auth_failed")])], vec![]),
            (vec![], vec![proposal(vec![update("github_auth_failed")])]),
        ] {
            let (ops_, _) = ops(
                vec![marked("github_auth_failed", Status::Live)],
                &pending,
                &rejected,
                &three(),
            );
            assert_eq!(ops_, ["create disk_full:data", "create push_failing"]);
        }
        // A marker the owner broke is no marker: the finding is new again.
        let mut edited = marked("push_failing", Status::Planned);
        edited.notes = Some(format!("{MARKER}push_failing\nDone?"));
        let (ops_, _) = ops(vec![edited], &[], &[], &three());
        assert_eq!(ops_.len(), 3);
    }

    #[test]
    fn items_resolve_only_when_their_source_was_read() {
        let items = || {
            vec![
                marked("disk_full:data", Status::Planned),
                marked("service_failed:jarvis-core.service", Status::InProgress),
                marked("backup_failed", Status::Blocked),
                marked("ibkr:sync_failing", Status::Planned),
                marked("github_auth_failed", Status::Planned),
                marked("job_paused:job-1", Status::Planned),
                marked("budget_over", Status::Planned),
                // Not resolvable, closed, or no code the check knows.
                marked("push_failing", Status::Planned),
                marked("security_login_failures", Status::Planned),
                marked("memecoin_not_running", Status::Live),
                marked("unknown_code", Status::Planned),
            ]
        };
        let (ops_, done) = ops(items(), &[], &[], &healthy());
        assert_eq!(
            ops_,
            [
                "item-disk_full:data Live",
                "item-service_failed:jarvis-core.service Live",
                "item-backup_failed Live",
                "item-ibkr:sync_failing Live",
                "item-github_auth_failed Live",
                "item-job_paused:job-1 Live",
                "item-budget_over Live",
            ]
        );
        assert_eq!((done.resolved, done.urgent), (7, 0));
        // Still there: no resolve.
        let (ops_, _) = ops(
            vec![marked("disk_full:data", Status::Planned)],
            &[],
            &[],
            &Signals {
                disks: vec![("data", 91.0)],
                ..healthy()
            },
        );
        assert!(ops_.is_empty());
        // Not read this run: no resolve.
        let unread = Signals {
            disks: vec![("system", 40.0)],
            units: HashMap::from([
                ("jarvis-core.service", "unknown"),
                ("jarvis-surrealdb.service", "active"),
            ]),
            sources: BTreeMap::from([
                ("jobs", false),
                ("ibkr", false),
                ("disks", true),
                ("services", true),
            ]),
            github: PollState::Starting,
            ..healthy()
        };
        let (ops_, _) = ops(items(), &[], &[], &unread);
        assert_eq!(ops_, ["item-budget_over Live"]);
        // An item a pending or recently rejected proposal changes stays.
        let (ops_, _) = ops(
            vec![marked("budget_over", Status::Planned)],
            &[proposal(vec![update("budget_over")])],
            &[],
            &healthy(),
        );
        assert!(ops_.is_empty());
        let (ops_, _) = ops(
            vec![marked("budget_over", Status::Planned)],
            &[],
            &[proposal(vec![update("budget_over")])],
            &healthy(),
        );
        assert!(ops_.is_empty());
    }

    #[test]
    fn a_proposal_holds_twenty_operations_and_no_more_items_than_room() {
        let paused = |n: usize| -> Vec<Job> {
            (0..n)
                .map(|_| {
                    let mut job = prompt();
                    job.enabled = false;
                    job.paused_reason = Some(PausedReason::PausedAfterFailures);
                    job
                })
                .collect()
        };
        let many = Signals {
            jobs: paused(30),
            ..three()
        };
        let (ops_, done) = ops(Vec::new(), &[], &[], &many);
        assert_eq!(ops_.len(), MAX_PROPOSAL_OPS);
        assert_eq!(ops_[0], "create disk_full:data");
        assert_eq!((done.created, done.urgent), (MAX_PROPOSAL_OPS, 1));
        // Resolves come after, within the same cap.
        let (ops_, done) = ops(
            vec![marked("budget_over", Status::Planned)],
            &[],
            &[],
            &Signals {
                jobs: paused(19),
                ..healthy()
            },
        );
        assert_eq!(ops_.len(), MAX_PROPOSAL_OPS);
        assert_eq!(ops_[19], "item-budget_over Live");
        assert_eq!(done.resolved, 1);
        // A full roadmap takes no new item, but a reopen.
        let mut items: Vec<Item> = (0..MAX_ITEMS - 1)
            .map(|n| marked(&format!("other_{n}"), Status::Live))
            .collect();
        items.push(marked("github_auth_failed", Status::Live));
        let (ops_, _) = ops(items.clone(), &[], &[], &three());
        assert_eq!(ops_, ["item-github_auth_failed Planned"]);
        items.remove(0);
        let (ops_, _) = ops(items, &[], &[], &three());
        assert_eq!(
            ops_,
            ["create disk_full:data", "item-github_auth_failed Planned"]
        );
    }

    #[test]
    fn only_a_newly_proposed_urgent_finding_pushes() {
        let mut owned = job("health_review", json!({}));
        let user = Uuid::now_v7();
        owned.owner_id = Some(user.to_string());
        let proposed = |urgent| -> Result<Proposed, &'static str> {
            Ok(Proposed {
                created: 3,
                urgent,
                ..Proposed::default()
            })
        };
        assert_eq!(
            urgent_push(&owned, &proposed(2)),
            Some((NotifyEvent::JobResult(JobResult::HealthUrgent(2)), user))
        );
        assert_eq!(
            urgent_push(&owned, &proposed(300)).unwrap().0,
            NotifyEvent::JobResult(JobResult::HealthUrgent(u8::MAX))
        );
        assert_eq!(urgent_push(&owned, &proposed(0)), None);
        assert_eq!(urgent_push(&owned, &Err("roadmap_limit")), None);
        let mut unowned = owned.clone();
        unowned.owner_id = None;
        assert_eq!(urgent_push(&unowned, &proposed(2)), None);
        // An urgent finding already on the roadmap is not new.
        let (_, done) = ops(
            vec![marked("disk_full:data", Status::Planned)],
            &[],
            &[],
            &three(),
        );
        assert_eq!(done.urgent, 0);
    }

    #[test]
    fn the_record_counts_the_proposal_and_its_failure() {
        let s = three();
        let found = findings(&s);
        let proposed = Ok(Proposed {
            created: 2,
            reopened: 1,
            resolved: 4,
            urgent: 1,
        });
        let recorded = finish(&s, &found, &proposed);
        assert_eq!(
            (recorded.status, recorded.error_code),
            (RunStatus::Ok, None)
        );
        let facts: serde_json::Value =
            serde_json::from_str(recorded.facts.as_deref().unwrap()).unwrap();
        assert_eq!(
            (&facts["proposed"], &facts["reopened"], &facts["resolved"]),
            (&json!(2), &json!(1), &json!(4))
        );
        assert!(recorded
            .summary
            .unwrap()
            .0
            .ends_with(". Proposed to the roadmap: 2 new, 1 reopened, 4 resolved."));
        let failed = finish(&s, &found, &Err("roadmap_limit"));
        assert_eq!(
            (failed.status, failed.error_code),
            (RunStatus::Partial, Some("roadmap_limit"))
        );
        let mut unread = s.clone();
        unread.sources.insert("ibkr", false);
        let both = finish(&unread, &found, &Err("roadmap_limit"));
        assert_eq!(both.error_code, Some("source_unavailable"));
        // The tail always fits.
        let long = Signals {
            llm_failures: (0..40)
                .map(|i| failures(&format!("{i:0>32}"), "timeout", 5))
                .collect(),
            ..unread
        };
        let summary = finish(&long, &findings(&long), &proposed)
            .summary
            .unwrap()
            .0;
        assert_eq!(summary.chars().count(), MAX_SUMMARY);
        assert!(summary.ends_with("4 resolved. Could not read: ibkr."));
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_roadmap_gets_each_finding_once_resolved_snoozed_and_reopened(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::roadmaps::{store as roadmaps, NewRoadmap};
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("health_{}", Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let hub = crate::realtime::Hub::default();
        let device = Uuid::now_v7().to_string();
        let plan = NewRoadmap {
            title: "Te fixen".into(),
            description: None,
            phases: Vec::new(),
        };
        let id = roadmaps::create(&db, &plan, &device, &hub)
            .await?
            .roadmap
            .id;
        let full = Signals {
            disks: vec![("system", 40.0), ("data", 99.0)],
            ..healthy()
        };
        let clear = healthy();
        let now = OffsetDateTime::now_utc();
        let check = |s: &Signals, at: OffsetDateTime| {
            let found = findings(s);
            let (db, id, s) = (&db, id.clone(), s.clone());
            async move { propose(db, &id, &s, &found, at).await }
        };

        // Run 1 proposes the urgent disk; run 2 finds it pending.
        assert_eq!(
            check(&full, now).await,
            Ok(Proposed {
                created: 1,
                urgent: 1,
                ..Proposed::default()
            })
        );
        let pending = roadmaps::list_proposals(&db, &id).await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].rationale.as_deref(), Some(RATIONALE));
        assert_eq!(describe(&pending[0].ops), ["create disk_full:data"]);
        assert_eq!(check(&full, now).await, Ok(Proposed::default()));

        // Accepted, the item holds it; once the disk is clear, run 3
        // proposes it live.
        roadmaps::accept_proposal(&db, &id, &pending[0].id, &device, &Default::default(), &hub)
            .await?;
        assert_eq!(check(&full, now).await, Ok(Proposed::default()));
        assert_eq!(
            check(&clear, now).await,
            Ok(Proposed {
                resolved: 1,
                ..Proposed::default()
            })
        );

        // Rejected, it is not proposed again for 7 days.
        let resolve = roadmaps::list_proposals(&db, &id).await?;
        assert_eq!(resolve.len(), 1);
        roadmaps::reject_proposal(&db, &id, &resolve[0].id, &device).await?;
        assert_eq!(
            roadmaps::rejected_since(&db, &id, now - SNOOZE).await?,
            resolve
        );
        assert!(
            roadmaps::rejected_since(&db, &id, now + time::Duration::minutes(5))
                .await?
                .is_empty()
        );
        assert_eq!(check(&clear, now).await, Ok(Proposed::default()));
        let later = now + time::Duration::days(8);
        assert_eq!(check(&clear, later).await.map(|p| p.resolved), Ok(1));

        // Accepted live, the full disk reopens it, urgent again.
        let resolve = roadmaps::list_proposals(&db, &id).await?;
        roadmaps::accept_proposal(&db, &id, &resolve[0].id, &device, &Default::default(), &hub)
            .await?;
        let roadmap = roadmaps::get(&db, &id).await?.ok_or("roadmap")?;
        assert_eq!(roadmap.items[0].status, Status::Live);
        assert_eq!(
            check(&full, later).await,
            Ok(Proposed {
                reopened: 1,
                urgent: 1,
                ..Proposed::default()
            })
        );
        Ok(())
    }
}
