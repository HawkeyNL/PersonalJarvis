//! Typed repository for scheduled jobs and their runs (schemas 0019, 0021).
//!
//! Owner writes name the revision they build on and bump it. The runner
//! bumps it only when it disables a job that ran its last slot: it writes a
//! run row and the slot it handled in one transaction that holds only while
//! the job is still at the revision the runner loaded, so a schedule change
//! in between wins and the runner reloads. All values are bound parameters;
//! only constants are formatted into queries.

use jiff::tz::TimeZone;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_store::Database;

use super::{
    Job, JobError, JobPatch, Kind, NewJob, Origin, Priority, Result, Run, RunStatus, SummarySource,
    Trigger, MAX_FACTS_BYTES, MAX_JARVIS_JOBS, MAX_OWNER_JOBS, MAX_PRIORITIES, MAX_REASON,
    MAX_RUNS, MAX_SUMMARY, RUN_RETENTION_DAYS,
};

/// An owner may start a job at most once per this many hours.
pub const OWNER_RUN_HOURS: u32 = 1;
/// Failed runs in a row that pause a chat-made job.
pub const MAX_FAILURES: u16 = 3;
/// Rows deleted per statement and statements per sweep.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;

/// Fields added by 0021 are NONE on older rows.
const JOB_FIELDS: &str = "record::id(id) AS id, kind, cadence, weekday, month_day, \
     (every ?? 1) AS every, start_date, max_runs, at_minute, enabled, revision, params, \
     last_slot_at, (run_count ?? 0) AS run_count, (origin ?? 'owner') AS origin, owner_id, \
     conversation_id, expires_at, (failure_count ?? 0) AS failure_count, paused_reason, \
     created_at, updated_at";
const RUN_FIELDS: &str = "record::id(id) AS id, job_id, kind, slot_at, trigger, status, \
     error_code, summary, summary_source, priorities, started_at, finished_at";

async fn run(db: &Database, query: &str, bindings: Value) -> Result<surrealdb::Response> {
    Ok(db.query(query).bind(bindings).await?.check()?)
}

/// Serialises job creation, so the room checks and the write cannot
/// interleave.
// ponytail: an in-process lock; Core is the only writer of jobs. Move the
// checks into the database if a second writer ever appears.
static CREATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A slot as bound text; queries cast it with `<datetime>`.
fn slot(at: Option<OffsetDateTime>) -> Result<Option<String>> {
    at.map(|at| at.format(&Rfc3339).map_err(|_| JobError::InvalidData))
        .transpose()
}

/// Sets `last_slot_at` from the bound `$last_slot_at`, which may be NONE.
const SET_LAST_SLOT: &str =
    "last_slot_at = IF $last_slot_at = NONE THEN NONE ELSE <datetime>$last_slot_at END";

async fn rows<T: DeserializeOwned>(db: &Database, query: &str, bindings: Value) -> Result<Vec<T>> {
    Ok(run(db, query, bindings).await?.take(0)?)
}

/// Every job, oldest first.
pub async fn list(db: &Database) -> Result<Vec<Job>> {
    rows(
        db,
        &format!("SELECT {JOB_FIELDS} FROM scheduled_jobs ORDER BY created_at LIMIT 100"),
        json!({}),
    )
    .await
}

pub async fn get(db: &Database, id: &str) -> Result<Option<Job>> {
    let found: Vec<Job> = rows(
        db,
        &format!("SELECT {JOB_FIELDS} FROM scheduled_jobs WHERE record::id(id) = $id LIMIT 1"),
        json!({"id": id}),
    )
    .await?;
    Ok(found.into_iter().next())
}

/// Create the owner's job ([`NewJob::owner_id`] set by the caller). There
/// is at most one `dependency_review` and [`MAX_OWNER_JOBS`] of the other
/// kinds. With the zone, the newest slot at creation counts as handled.
pub async fn create(db: &Database, input: NewJob, tz: Option<&TimeZone>) -> Result<Job> {
    let job = input.into_job(
        uuid::Uuid::now_v7().to_string(),
        OffsetDateTime::now_utc(),
        tz,
    )?;
    insert(db, job).await
}

/// Create a `prompt` job Jarvis makes for itself, for the owner `owner_id`:
/// at most [`MAX_JARVIS_JOBS`] that have not expired, apart from the
/// owner's, each expiring after [`super::JARVIS_JOB_DAYS`].
pub async fn create_for_jarvis(
    db: &Database,
    mut input: NewJob,
    owner_id: &str,
    tz: Option<&TimeZone>,
) -> Result<Job> {
    input.owner_id = Some(owner_id.to_owned());
    let job = input.into_jarvis_job(
        uuid::Uuid::now_v7().to_string(),
        OffsetDateTime::now_utc(),
        tz,
    )?;
    insert(db, job).await
}

/// Whether `existing` leaves room for `job`.
fn room(existing: &[Job], job: &Job) -> Result<()> {
    let count = |origin: Origin| {
        existing
            .iter()
            .filter(|known| known.kind != Kind::DependencyReview && known.origin == origin)
            .filter(|known| known.expires_at.is_none_or(|at| at > job.created_at))
            .count()
    };
    match (job.kind, job.origin) {
        (Kind::DependencyReview, _) => {
            if existing
                .iter()
                .any(|known| known.kind == Kind::DependencyReview)
            {
                return Err(JobError::Exists);
            }
        }
        (_, Origin::Owner) if count(Origin::Owner) >= MAX_OWNER_JOBS => {
            return Err(JobError::Full("at most 10 jobs; delete one first"));
        }
        (_, Origin::Jarvis) if count(Origin::Jarvis) >= MAX_JARVIS_JOBS => {
            return Err(JobError::Full("Jarvis keeps at most 3 jobs of its own"));
        }
        _ => {}
    }
    Ok(())
}

/// Every row, unlike `list`, so none can drop out of a count.
async fn all(db: &Database) -> Result<Vec<Job>> {
    rows(
        db,
        &format!("SELECT {JOB_FIELDS} FROM scheduled_jobs"),
        json!({}),
    )
    .await
}

/// Write `job` if there is room. The lock is the first gate; [`write`]
/// holds the same rules in its transaction.
async fn insert(db: &Database, job: Job) -> Result<Job> {
    let _creating = CREATE.lock().await;
    room(&all(db).await?, &job)?;
    write(db, &job).await?;
    get(db, &job.id).await?.ok_or(JobError::InvalidData)
}

/// Create the row in one transaction with the room rules, so they also hold
/// when a caller's timeout released the lock while an earlier write was
/// still in flight.
async fn write(db: &Database, job: &Job) -> Result<()> {
    let limit = match job.origin {
        Origin::Owner => MAX_OWNER_JOBS,
        Origin::Jarvis => MAX_JARVIS_JOBS,
    };
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             IF $kind = 'dependency_review' AND array::len((SELECT VALUE id \
               FROM scheduled_jobs WHERE kind = 'dependency_review')) > 0 \
               {{ THROW 'job exists'; }}; \
             IF $kind != 'dependency_review' AND array::len((SELECT VALUE id \
               FROM scheduled_jobs WHERE kind != 'dependency_review' \
               AND (origin ?? 'owner') = $origin \
               AND (expires_at = NONE OR expires_at > time::now()))) >= $limit \
               {{ THROW 'no room'; }}; \
             CREATE scheduled_jobs SET id = $id, kind = $kind, cadence = $cadence, \
               weekday = $weekday, month_day = $month_day, every = $every, \
               start_date = $start_date, max_runs = $max_runs, at_minute = $at_minute, \
               enabled = $enabled, revision = 1, params = $params, {SET_LAST_SLOT}, \
               run_count = 0, origin = $origin, owner_id = $owner_id, \
               conversation_id = $conversation_id, \
               expires_at = IF $expires_at = NONE THEN NONE ELSE <datetime>$expires_at END, \
               failure_count = 0, created_at = time::now(), updated_at = time::now() \
               RETURN NONE; \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": job.id,
            "kind": job.kind,
            "cadence": job.cadence,
            "weekday": job.weekday,
            "month_day": job.month_day,
            "every": job.every,
            "start_date": job.start_date,
            "max_runs": job.max_runs,
            "at_minute": job.at_minute,
            "enabled": job.enabled,
            "params": job.params,
            "last_slot_at": slot(job.last_slot_at)?,
            "origin": job.origin,
            "owner_id": job.owner_id,
            "conversation_id": job.conversation_id,
            "expires_at": slot(job.expires_at)?,
            "limit": limit,
        }))
        .await?;
    let Err(error) = response.check() else {
        return Ok(());
    };
    // Explain the refusal from the current state; anything else is a
    // database error.
    room(&all(db).await?, job)?;
    Err(error.into())
}

/// Change a job at revision `expected`; a change that changes nothing writes
/// nothing. A schedule change sets `last_slot_at` as [`Job::patched`] says,
/// so no slot before it runs, and starts the run count again; otherwise the
/// runner's values are kept as stored. Enabling a job clears its failure
/// streak and pause.
pub async fn update(
    db: &Database,
    id: &str,
    expected: u64,
    patch: &JobPatch,
    tz: Option<&TimeZone>,
) -> Result<Job> {
    let job = get(db, id).await?.ok_or(JobError::NotFound)?;
    if job.revision != expected {
        return Err(JobError::Conflict);
    }
    let (new, rescheduled) = job.patched(patch, OffsetDateTime::now_utc(), tz)?;
    if new == job {
        return Ok(job);
    }
    let updated: Vec<u64> = run(
        db,
        "LET $seed = IF $last_slot_at = NONE THEN NONE ELSE <datetime>$last_slot_at END; \
         UPDATE scheduled_jobs SET cadence = $cadence, weekday = $weekday, \
           month_day = $month_day, every = $every, start_date = $start_date, \
           max_runs = $max_runs, at_minute = $at_minute, enabled = $enabled, \
           params = $params, \
           last_slot_at = IF $rescheduled THEN $seed ELSE last_slot_at END, \
           run_count = IF $rescheduled THEN 0 ELSE run_count END, \
           failure_count = IF $resumed THEN 0 ELSE failure_count END, \
           paused_reason = IF $resumed THEN NONE ELSE paused_reason END, \
           revision = $revision, updated_at = time::now() \
         WHERE record::id(id) = $id AND revision = $expected RETURN VALUE revision",
        json!({
            "id": id,
            "expected": expected,
            "revision": expected + 1,
            "cadence": new.cadence,
            "weekday": new.weekday,
            "month_day": new.month_day,
            "every": new.every,
            "start_date": new.start_date,
            "max_runs": new.max_runs,
            "at_minute": new.at_minute,
            "enabled": new.enabled,
            "params": new.params,
            "rescheduled": rescheduled,
            "resumed": new.enabled && !job.enabled,
            "last_slot_at": slot(new.last_slot_at)?,
        }),
    )
    .await?
    .take(1)?;
    if updated.is_empty() {
        return Err(match get(db, id).await? {
            None => JobError::NotFound,
            Some(_) => JobError::Conflict,
        });
    }
    get(db, id).await?.ok_or(JobError::NotFound)
}

/// Delete a job at revision `expected`, with its runs.
pub async fn delete(db: &Database, id: &str, expected: u64) -> Result<()> {
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $gone = (DELETE scheduled_jobs WHERE record::id(id) = $id \
               AND revision = $expected RETURN BEFORE); \
             IF array::len($gone) != 1 { THROW 'job unavailable'; }; \
             DELETE scheduled_job_runs WHERE job_id = $id RETURN NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({"id": id, "expected": expected}))
        .await?;
    let Err(error) = response.check() else {
        return Ok(());
    };
    match get(db, id).await? {
        None => Err(JobError::NotFound),
        Some(job) if job.revision != expected => Err(JobError::Conflict),
        Some(_) => Err(error.into()),
    }
}

/// Point the job at the conversation its runs write to. Core's own field:
/// the revision stays.
pub async fn set_conversation(db: &Database, job_id: &str, conversation_id: &str) -> Result<()> {
    run(
        db,
        "UPDATE scheduled_jobs SET conversation_id = $conversation_id \
         WHERE record::id(id) = $id RETURN NONE",
        json!({"id": job_id, "conversation_id": conversation_id}),
    )
    .await?;
    Ok(())
}

/// Count a failed run, or end the streak. The [`MAX_FAILURES`]th failed run
/// in a row disables the job at the next revision, with the reason
/// `paused_after_failures`; true then.
pub async fn record_outcome(db: &Database, job_id: &str, failed: bool) -> Result<bool> {
    let mut response = run(
        db,
        "BEGIN TRANSACTION; \
         UPDATE scheduled_jobs \
           SET failure_count = IF $failed THEN (failure_count ?? 0) + 1 ELSE 0 END \
           WHERE record::id(id) = $id RETURN NONE; \
         UPDATE scheduled_jobs SET enabled = false, \
           paused_reason = 'paused_after_failures', revision = revision + 1 \
           WHERE record::id(id) = $id AND failure_count >= $max AND enabled = true \
           RETURN VALUE record::id(id); \
         COMMIT TRANSACTION;",
        json!({"id": job_id, "failed": failed, "max": MAX_FAILURES}),
    )
    .await?;
    let last = response
        .num_statements()
        .checked_sub(1)
        .ok_or(JobError::InvalidData)?;
    let paused: Vec<String> = response.take(last)?;
    Ok(!paused.is_empty())
}

/// Delete the jobs Jarvis made for itself that expired, with their runs.
/// Returns their ids.
pub async fn delete_expired(db: &Database) -> Result<Vec<String>> {
    let mut response = run(
        db,
        "BEGIN TRANSACTION; \
         LET $ids = (SELECT VALUE record::id(id) FROM scheduled_jobs \
           WHERE origin = 'jarvis' AND expires_at != NONE AND expires_at <= time::now()); \
         DELETE scheduled_jobs WHERE record::id(id) IN $ids RETURN NONE; \
         DELETE scheduled_job_runs WHERE job_id IN $ids RETURN NONE; \
         RETURN $ids; \
         COMMIT TRANSACTION;",
        json!({}),
    )
    .await?;
    let last = response
        .num_statements()
        .checked_sub(1)
        .ok_or(JobError::InvalidData)?;
    Ok(response.take(last)?)
}

/// Write the row of a run before it starts, or a `missed` row, and return
/// its id. A scheduled row is written only while the job is enabled and at
/// the revision `job` was loaded at, and records `slot_at` as handled
/// ([`JobError::Conflict`] otherwise, also for a slot already handled); a
/// scheduled run counts towards `max_runs`, and when it is the job's last
/// ([`Job::ends_with`]) the job is disabled at the next revision. An owner's
/// row starts running, once per [`OWNER_RUN_HOURS`] ([`JobError::TooSoon`]).
/// A refused write writes nothing.
pub async fn start_run(
    db: &Database,
    job: &Job,
    slot_at: OffsetDateTime,
    trigger: Trigger,
    status: RunStatus,
) -> Result<String> {
    let valid = match trigger {
        Trigger::Schedule => matches!(status, RunStatus::Running | RunStatus::Missed),
        Trigger::Owner => status == RunStatus::Running,
    };
    if !valid {
        return Err(JobError::Invalid("not a starting run state"));
    }
    let run_id = uuid::Uuid::now_v7().to_string();
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             LET $slot = <datetime>$slot_at; \
             LET $job = (UPDATE scheduled_jobs \
               SET last_slot_at = IF $owner THEN last_slot_at ELSE $slot END, \
                 run_count = IF $counted THEN (run_count ?? 0) + 1 ELSE run_count END, \
                 enabled = IF $last THEN false ELSE enabled END, \
                 revision = IF $last THEN revision + 1 ELSE revision END \
               WHERE record::id(id) = $job_id \
               AND ($owner OR (revision = $revision AND enabled = true \
                 AND (last_slot_at = NONE OR last_slot_at < $slot))) RETURN VALUE id); \
             IF array::len($job) != 1 {{ THROW 'job changed'; }}; \
             IF $owner AND array::len((SELECT VALUE id FROM scheduled_job_runs \
               WHERE job_id = $job_id AND trigger = 'owner' \
               AND started_at > time::now() - {OWNER_RUN_HOURS}h)) > 0 \
               {{ THROW 'owner run too soon'; }}; \
             CREATE scheduled_job_runs SET id = $id, job_id = $job_id, kind = $kind, \
               slot_at = $slot, trigger = $trigger, status = $status, priorities = [], \
               started_at = time::now(), \
               finished_at = IF $status = 'running' THEN NONE ELSE time::now() END \
               RETURN NONE; \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": run_id,
            "job_id": job.id,
            "revision": job.revision,
            "kind": job.kind,
            "slot_at": slot_at.format(&Rfc3339).map_err(|_| JobError::InvalidData)?,
            "owner": trigger == Trigger::Owner,
            "counted": trigger == Trigger::Schedule && status == RunStatus::Running,
            "last": trigger == Trigger::Schedule && job.ends_with(status),
            "trigger": trigger,
            "status": status,
        }))
        .await?;
    let Err(error) = response.check() else {
        return Ok(run_id);
    };
    // Explain the refusal from the current state; anything else is a
    // database error.
    let Some(current) = get(db, &job.id).await? else {
        return Err(JobError::NotFound);
    };
    let refused = match trigger {
        Trigger::Owner => last_owner_run(db, &job.id).await?.is_some_and(|at| {
            OffsetDateTime::now_utc() - at < time::Duration::hours(OWNER_RUN_HOURS.into())
        }),
        Trigger::Schedule => {
            current.revision != job.revision
                || !current.enabled
                || current.last_slot_at.is_some_and(|at| at >= slot_at)
        }
    };
    Err(match (refused, trigger) {
        (true, Trigger::Owner) => JobError::TooSoon,
        (true, Trigger::Schedule) => JobError::Conflict,
        (false, _) => error.into(),
    })
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Finish {
    pub status: RunStatus,
    pub error_code: Option<&'static str>,
    /// Compact JSON, at most [`MAX_FACTS_BYTES`].
    pub facts: Option<String>,
    pub summary: Option<(String, SummarySource)>,
    pub priorities: Vec<Priority>,
}

impl Finish {
    /// The rules the schema holds a finished run to.
    pub fn check(&self) -> std::result::Result<(), &'static str> {
        fn ok(valid: bool, message: &'static str) -> std::result::Result<(), &'static str> {
            if valid {
                Ok(())
            } else {
                Err(message)
            }
        }
        ok(
            !matches!(self.status, RunStatus::Running | RunStatus::Missed),
            "not a final run state",
        )?;
        ok(
            self.facts
                .as_ref()
                .is_none_or(|facts| (2..=MAX_FACTS_BYTES).contains(&facts.len())),
            "facts must be 2 bytes to 64 KiB",
        )?;
        ok(
            self.summary.as_ref().is_none_or(|(summary, _)| {
                !summary.trim().is_empty() && summary.chars().count() <= MAX_SUMMARY
            }),
            "summary must be 1 to 2000 characters",
        )?;
        ok(
            self.priorities.len() <= MAX_PRIORITIES
                && self.priorities.iter().all(|priority| {
                    valid_ref(&priority.reference)
                        && (1..=MAX_PRIORITIES).contains(&usize::from(priority.rank))
                        && !priority.reason.trim().is_empty()
                        && priority.reason.chars().count() <= MAX_REASON
                }),
            "priorities must be at most 20 valid entries",
        )?;
        Ok(())
    }

    pub fn failed(error_code: &'static str) -> Self {
        Self {
            status: RunStatus::Failed,
            error_code: Some(error_code),
            facts: None,
            summary: None,
            priorities: Vec::new(),
        }
    }
}

/// Record how a running run ended; a run that is no longer running (Core
/// restarted and marked it interrupted) stays as it is.
pub async fn finish_run(db: &Database, run_id: &str, finish: &Finish) -> Result<bool> {
    finish.check().map_err(JobError::Invalid)?;
    let (summary, source) = match &finish.summary {
        Some((summary, source)) => (Some(summary.as_str()), Some(*source)),
        None => (None, None),
    };
    let updated: Vec<String> = rows(
        db,
        "UPDATE scheduled_job_runs SET status = $status, error_code = $error_code, \
           facts = $facts, summary = $summary, summary_source = $source, \
           priorities = $priorities, finished_at = time::now() \
         WHERE record::id(id) = $id AND status = 'running' RETURN VALUE record::id(id)",
        json!({
            "id": run_id,
            "status": finish.status,
            "error_code": finish.error_code,
            "facts": finish.facts,
            "summary": summary,
            "source": source,
            "priorities": finish.priorities,
        }),
    )
    .await?;
    Ok(!updated.is_empty())
}

/// `a1`, `p12`: a letter for the kind of fact and its number (the schema's
/// rule).
pub fn valid_ref(reference: &str) -> bool {
    let mut chars = reference.chars();
    matches!(chars.next(), Some('a' | 'p'))
        && matches!(chars.next(), Some('1'..='9'))
        && reference.len() <= 5
        && chars.all(|c| c.is_ascii_digit())
}

/// Mark every run that is still running as interrupted: no run survives a
/// Core restart. Returns their ids.
pub async fn interrupt_running(db: &Database) -> Result<Vec<String>> {
    rows(
        db,
        "UPDATE scheduled_job_runs SET status = 'interrupted', error_code = 'interrupted', \
           finished_at = time::now() WHERE status = 'running' RETURN VALUE record::id(id)",
        json!({}),
    )
    .await
}

/// When the owner last started the job.
pub async fn last_owner_run(db: &Database, job_id: &str) -> Result<Option<OffsetDateTime>> {
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(with = "time::serde::rfc3339")]
        started_at: OffsetDateTime,
    }
    let found: Vec<Row> = rows(
        db,
        "SELECT started_at FROM scheduled_job_runs WHERE job_id = $id AND trigger = 'owner' \
         ORDER BY started_at DESC LIMIT 1",
        json!({"id": job_id}),
    )
    .await?;
    Ok(found.into_iter().next().map(|row| row.started_at))
}

/// The job's newest `limit` runs (1 to [`MAX_RUNS`]), newest first, without
/// their facts.
pub async fn list_runs(db: &Database, job_id: &str, limit: usize) -> Result<Vec<Run>> {
    if get(db, job_id).await?.is_none() {
        return Err(JobError::NotFound);
    }
    rows(
        db,
        &format!(
            "SELECT {RUN_FIELDS} FROM scheduled_job_runs WHERE job_id = $id \
             ORDER BY started_at DESC LIMIT $limit"
        ),
        json!({"id": job_id, "limit": limit.clamp(1, MAX_RUNS)}),
    )
    .await
}

/// One run with its facts (`None` when it has none).
pub async fn get_run(db: &Database, run_id: &str) -> Result<Option<(Run, Option<Value>)>> {
    let mut response = run(
        db,
        &format!(
            "SELECT {RUN_FIELDS} FROM scheduled_job_runs WHERE record::id(id) = $id LIMIT 1; \
             SELECT VALUE facts FROM scheduled_job_runs WHERE record::id(id) = $id LIMIT 1;"
        ),
        json!({"id": run_id}),
    )
    .await?;
    let found: Vec<Run> = response.take(0)?;
    let facts: Vec<Option<String>> = response.take(1)?;
    let Some(found) = found.into_iter().next() else {
        return Ok(None);
    };
    let facts = facts
        .into_iter()
        .next()
        .flatten()
        .map(|facts| serde_json::from_str(&facts).map_err(|_| JobError::InvalidData))
        .transpose()?;
    Ok(Some((found, facts)))
}

/// Delete runs older than [`RUN_RETENTION_DAYS`], in bounded batches.
pub async fn prune(db: &Database) -> Result<usize> {
    let query = format!(
        "LET $ids = (SELECT VALUE id FROM scheduled_job_runs \
           WHERE started_at < time::now() - {RUN_RETENTION_DAYS}d AND status != 'running' \
           LIMIT {PRUNE_BATCH}); \
         DELETE $ids RETURN NONE; RETURN array::len($ids);"
    );
    let mut total = 0;
    for _ in 0..PRUNE_MAX_BATCHES {
        let deleted: Option<usize> = db.query(query.as_str()).await?.check()?.take(2)?;
        let deleted = deleted.unwrap_or(0);
        total += deleted;
        if deleted < PRUNE_BATCH {
            break;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;
    use crate::jobs::{Cadence, Kind, Params};

    async fn database() -> std::result::Result<Database, Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("jobs_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    fn new_job() -> NewJob {
        serde_json::from_value(json!({"kind": "dependency_review"})).expect("new job")
    }

    fn slot(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).expect("slot")
    }

    #[test]
    fn refs_match_the_schema_rule() {
        for ok in ["a1", "p12", "a9999"] {
            assert!(valid_ref(ok), "{ok}");
        }
        for bad in ["", "a", "a0", "x1", "A1", "a01", "a10000", "a1 ", "p-1"] {
            assert!(!valid_ref(bad), "{bad}");
        }
    }

    fn prompt() -> NewJob {
        serde_json::from_value(json!({
            "kind": "prompt",
            "params": {"title": "News", "instruction": "Summarize the AI news"},
        }))
        .expect("prompt")
    }

    #[test]
    fn room_holds_one_review_ten_owner_jobs_and_three_of_jarvis() {
        let now = OffsetDateTime::now_utc();
        let owner = |n: usize| -> Vec<Job> {
            (0..n)
                .map(|i| prompt().into_job(i.to_string(), now, None).expect("job"))
                .collect()
        };
        let jarvis = |n: usize| -> Vec<Job> {
            (0..n)
                .map(|i| {
                    prompt()
                        .into_jarvis_job(i.to_string(), now, None)
                        .expect("job")
                })
                .collect()
        };
        let review = new_job().into_job("r".into(), now, None).expect("review");
        let new_owner = &owner(1)[0];
        let new_jarvis = &jarvis(1)[0];
        assert!(room(&owner(9), new_owner).is_ok());
        assert!(matches!(
            room(&owner(10), new_owner),
            Err(JobError::Full(_))
        ));
        // The review and Jarvis's jobs do not take the owner's room.
        let mut others = jarvis(3);
        others.push(review.clone());
        others.extend(owner(9));
        assert!(room(&others, new_owner).is_ok());
        assert!(matches!(room(&others, new_jarvis), Err(JobError::Full(_))));
        assert!(matches!(room(&others, &review), Err(JobError::Exists)));
        assert!(room(&owner(10), &review).is_ok());
        // An expired job of Jarvis's no longer counts.
        let mut expired = jarvis(3);
        expired[0].expires_at = Some(now);
        assert!(room(&expired, new_jarvis).is_ok());
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn jobs_are_one_per_kind_and_revision_guarded(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let job = create(&db, new_job(), None).await?;
        assert_eq!(
            (job.cadence, job.weekday, job.at_minute, job.revision),
            (Cadence::Weekly, Some(1), 420, 1)
        );
        assert!(job.enabled && job.last_slot_at.is_none());
        assert!(matches!(
            create(&db, new_job(), None).await,
            Err(JobError::Exists)
        ));
        assert_eq!(list(&db).await?, std::slice::from_ref(&job));

        // A params change keeps the slot the runner stored; a schedule
        // change clears it.
        let id = &job.id;
        let monday = slot("2026-11-09T06:00:00Z");
        start_run(&db, &job, monday, Trigger::Schedule, RunStatus::Missed).await?;
        let roadmap = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
        let linked = update(
            &db,
            id,
            1,
            &JobPatch {
                params: Some(Params {
                    roadmap_id: Some(roadmap.into()),
                    ..Params::default()
                }),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert_eq!(linked.revision, 2);
        assert_eq!(linked.last_slot_at, Some(monday));
        assert_eq!(linked.params.roadmap_id.as_deref(), Some(roadmap));
        assert!(matches!(
            update(&db, id, 1, &JobPatch::default(), None).await,
            Err(JobError::Conflict)
        ));
        // Nothing changes: no new revision.
        assert_eq!(
            update(&db, id, 2, &JobPatch::default(), None)
                .await?
                .revision,
            2
        );
        let daily = update(
            &db,
            id,
            2,
            &JobPatch {
                cadence: Some(Cadence::Daily),
                at_minute: Some(1439),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert_eq!(
            (
                daily.revision,
                daily.weekday,
                daily.at_minute,
                daily.last_slot_at
            ),
            (3, None, 1439, None)
        );
        assert!(matches!(
            update(
                &db,
                id,
                3,
                &JobPatch {
                    weekday: Some(2),
                    ..JobPatch::default()
                },
                None,
            )
            .await,
            Err(JobError::Invalid(_))
        ));
        let unknown = uuid::Uuid::now_v7().to_string();
        assert!(matches!(
            update(&db, &unknown, 1, &JobPatch::default(), None).await,
            Err(JobError::NotFound)
        ));

        // Delete takes the runs along.
        assert!(matches!(delete(&db, id, 2).await, Err(JobError::Conflict)));
        assert!(matches!(
            delete(&db, &unknown, 1).await,
            Err(JobError::NotFound)
        ));
        delete(&db, id, 3).await?;
        assert!(get(&db, id).await?.is_none());
        let runs: Vec<String> = rows(
            &db,
            "SELECT VALUE record::id(id) FROM scheduled_job_runs",
            json!({}),
        )
        .await?;
        assert!(runs.is_empty());
        // The kind is free again. With the zone, the newest slot at creation
        // and after a reschedule counts as handled and never starts.
        let tz = crate::jobs::time_zone().ok_or("zone")?;
        let seeded = create(&db, new_job(), Some(&tz)).await?;
        assert_eq!(seeded.kind, Kind::DependencyReview);
        let handled = seeded.last_slot_at.ok_or("seeded slot")?;
        assert!(handled <= seeded.created_at);
        assert!(seeded.created_at - handled <= time::Duration::days(7));
        assert!(matches!(
            start_run(&db, &seeded, handled, Trigger::Schedule, RunStatus::Running).await,
            Err(JobError::Conflict)
        ));
        let moved = update(
            &db,
            &seeded.id,
            1,
            &JobPatch {
                cadence: Some(Cadence::Daily),
                ..JobPatch::default()
            },
            Some(&tz),
        )
        .await?;
        let handled = moved.last_slot_at.ok_or("seeded slot")?;
        assert!(handled <= moved.updated_at);
        assert!(moved.updated_at - handled <= time::Duration::days(1));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn runs_are_written_first_once_per_slot_and_interrupted_on_restart(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let job = create(&db, new_job(), None).await?;
        let monday = slot("2026-11-09T06:00:00Z");
        let run_id = start_run(&db, &job, monday, Trigger::Schedule, RunStatus::Running).await?;
        assert_eq!(get(&db, &job.id).await?.unwrap().last_slot_at, Some(monday));
        // The same slot twice, or from a job that changed meanwhile, writes
        // nothing.
        assert!(matches!(
            start_run(&db, &job, monday, Trigger::Schedule, RunStatus::Missed).await,
            Err(JobError::Conflict)
        ));
        update(
            &db,
            &job.id,
            1,
            &JobPatch {
                at_minute: Some(480),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        let next = slot("2026-11-16T07:00:00Z");
        assert!(matches!(
            start_run(&db, &job, next, Trigger::Schedule, RunStatus::Running).await,
            Err(JobError::Conflict)
        ));
        let off = update(
            &db,
            &job.id,
            2,
            &JobPatch {
                enabled: Some(false),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert!(matches!(
            start_run(&db, &off, next, Trigger::Schedule, RunStatus::Running).await,
            Err(JobError::Conflict)
        ));

        // The owner may run a disabled job, once an hour; the slot stays.
        let now = OffsetDateTime::now_utc();
        let owner = start_run(&db, &off, now, Trigger::Owner, RunStatus::Running).await?;
        assert!(matches!(
            start_run(
                &db,
                &off,
                now + time::Duration::seconds(1),
                Trigger::Owner,
                RunStatus::Running
            )
            .await,
            Err(JobError::TooSoon)
        ));
        assert!(matches!(
            start_run(&db, &off, now, Trigger::Owner, RunStatus::Missed).await,
            Err(JobError::Invalid(_))
        ));
        assert!(last_owner_run(&db, &job.id).await?.is_some());
        assert_eq!(get(&db, &job.id).await?.unwrap().last_slot_at, None);

        let finish = Finish {
            status: RunStatus::Ok,
            error_code: None,
            facts: Some(r#"{"alerts":[]}"#.into()),
            summary: Some(("No open alerts.".into(), SummarySource::Deterministic)),
            priorities: vec![Priority {
                reference: "a1".into(),
                rank: 1,
                reason: "Fix first.".into(),
            }],
        };
        assert!(finish_run(&db, &run_id, &finish).await?);
        // A finished run stays finished.
        assert!(!finish_run(&db, &run_id, &finish).await?);
        for bad in [
            Finish {
                status: RunStatus::Running,
                ..finish.clone()
            },
            Finish {
                summary: Some((" ".into(), SummarySource::Claude)),
                ..finish.clone()
            },
            Finish {
                priorities: vec![Priority {
                    reference: "x1".into(),
                    ..finish.priorities[0].clone()
                }],
                ..finish.clone()
            },
        ] {
            assert!(matches!(
                finish_run(&db, &run_id, &bad).await,
                Err(JobError::Invalid(_))
            ));
        }

        // A restart interrupts what still runs.
        assert_eq!(interrupt_running(&db).await?, std::slice::from_ref(&owner));
        let runs = list_runs(&db, &job.id, 10).await?;
        assert_eq!(
            runs.iter()
                .map(|run| (run.id.as_str(), run.status, run.trigger))
                .collect::<Vec<_>>(),
            [
                (owner.as_str(), RunStatus::Interrupted, Trigger::Owner),
                (run_id.as_str(), RunStatus::Ok, Trigger::Schedule),
            ]
        );
        let (done, facts) = get_run(&db, &run_id).await?.unwrap();
        assert_eq!(done.summary.as_deref(), Some("No open alerts."));
        assert_eq!(done.summary_source, Some(SummarySource::Deterministic));
        assert_eq!(done.priorities, finish.priorities);
        assert_eq!(facts, Some(json!({"alerts": []})));
        assert_eq!(get_run(&db, &owner).await?.unwrap().1, None);
        assert!(get_run(&db, &uuid::Uuid::now_v7().to_string())
            .await?
            .is_none());
        assert!(matches!(
            list_runs(&db, &uuid::Uuid::now_v7().to_string(), 10).await,
            Err(JobError::NotFound)
        ));

        // Retention drops old finished runs only.
        run(
            &db,
            "UPDATE scheduled_job_runs SET started_at = time::now() - 181d",
            json!({}),
        )
        .await?;
        assert_eq!(prune(&db).await?, 2);
        assert!(list_runs(&db, &job.id, 10).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn the_write_itself_holds_the_room_rules(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let now = OffsetDateTime::now_utc();
        let fresh = || uuid::Uuid::now_v7().to_string();
        // Past the lock and the first check, the transaction still refuses.
        write(&db, &new_job().into_job(fresh(), now, None)?).await?;
        assert!(matches!(
            write(&db, &new_job().into_job(fresh(), now, None)?).await,
            Err(JobError::Exists)
        ));
        for _ in 0..MAX_OWNER_JOBS {
            write(&db, &prompt().into_job(fresh(), now, None)?).await?;
        }
        assert!(matches!(
            write(&db, &prompt().into_job(fresh(), now, None)?).await,
            Err(JobError::Full(_))
        ));
        for _ in 0..MAX_JARVIS_JOBS {
            write(&db, &prompt().into_jarvis_job(fresh(), now, None)?).await?;
        }
        assert!(matches!(
            write(&db, &prompt().into_jarvis_job(fresh(), now, None)?).await,
            Err(JobError::Full(_))
        ));
        assert_eq!(all(&db).await?.len(), 1 + MAX_OWNER_JOBS + MAX_JARVIS_JOBS);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn a_job_from_before_0021_reads_with_defaults(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        // A row as 0019 wrote it: 0021 adds no value to it.
        let id = uuid::Uuid::now_v7().to_string();
        run(
            &db,
            "CREATE scheduled_jobs SET id = $id, kind = 'dependency_review', \
               cadence = 'weekly', weekday = 1, at_minute = 420, enabled = true, \
               revision = 1, params = {}, created_at = time::now(), \
               updated_at = time::now()",
            json!({"id": id}),
        )
        .await?;
        let legacy = list(&db).await?.into_iter().next().ok_or("legacy job")?;
        assert_eq!(
            (
                legacy.id.as_str(),
                legacy.every,
                legacy.run_count,
                legacy.origin,
                legacy.failure_count
            ),
            (id.as_str(), 1, 0, Origin::Owner, 0)
        );
        assert_eq!(
            (
                legacy.month_day,
                legacy.start_date.as_deref(),
                legacy.max_runs,
                legacy.owner_id.as_deref(),
                legacy.expires_at,
                legacy.paused_reason
            ),
            (None, None, None, None, None, None)
        );
        // It still takes a run and an owner change.
        start_run(
            &db,
            &legacy,
            slot("2026-11-09T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Running,
        )
        .await?;
        let daily = update(
            &db,
            &id,
            1,
            &JobPatch {
                cadence: Some(Cadence::Daily),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert_eq!(
            (daily.revision, daily.run_count, daily.weekday),
            (2, 0, None)
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn new_kinds_are_limited_and_end_after_their_last_run(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let owner = uuid::Uuid::now_v7().to_string();
        let mut input = prompt();
        input.owner_id = Some(owner.clone());
        input.max_runs = Some(2);
        let job = create(&db, input, None).await?;
        assert_eq!(get(&db, &job.id).await?.as_ref(), Some(&job));
        assert_eq!(
            (job.origin, job.owner_id.as_deref(), job.params.notify),
            (Origin::Owner, Some(owner.as_str()), Some(true))
        );
        // A second review job alongside is fine; a second review is not.
        create(&db, new_job(), None).await?;
        assert!(matches!(
            create(&db, new_job(), None).await,
            Err(JobError::Exists)
        ));
        for _ in 1..MAX_OWNER_JOBS {
            create(&db, prompt(), None).await?;
        }
        assert!(matches!(
            create(&db, prompt(), None).await,
            Err(JobError::Full(_))
        ));
        // Jarvis has its own room, for prompts only.
        for _ in 0..MAX_JARVIS_JOBS {
            let own = create_for_jarvis(&db, prompt(), &owner, None).await?;
            assert_eq!(own.origin, Origin::Jarvis);
            assert!(own.expires_at.is_some_and(|at| at > own.created_at));
        }
        assert!(matches!(
            create_for_jarvis(&db, prompt(), &owner, None).await,
            Err(JobError::Full(_))
        ));
        let reminder =
            serde_json::from_value(json!({"kind": "reminder", "params": {"title": "Call"}}))?;
        assert!(matches!(
            create_for_jarvis(&db, reminder, &owner, None).await,
            Err(JobError::Invalid(_))
        ));

        // Scheduled runs count; the one that reaches max_runs disables the
        // job at the next revision. Missed slots and the owner's runs do not.
        start_run(
            &db,
            &job,
            slot("2026-11-02T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Missed,
        )
        .await?;
        let job = get(&db, &job.id).await?.ok_or("job")?;
        start_run(
            &db,
            &job,
            slot("2026-11-09T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Running,
        )
        .await?;
        start_run(
            &db,
            &job,
            OffsetDateTime::now_utc(),
            Trigger::Owner,
            RunStatus::Running,
        )
        .await?;
        let once_more = get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!(
            (once_more.run_count, once_more.enabled, once_more.revision),
            (1, true, 1)
        );
        start_run(
            &db,
            &once_more,
            slot("2026-11-16T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Running,
        )
        .await?;
        let ended = get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!(
            (ended.run_count, ended.enabled, ended.revision),
            (2, false, 2)
        );
        // Enabling it again starts the count again.
        let again = update(
            &db,
            &job.id,
            2,
            &JobPatch {
                enabled: Some(true),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert_eq!(
            (again.run_count, again.enabled, again.revision),
            (0, true, 3)
        );

        // A one-time job ends after its slot, run or missed.
        let once = |date: &str| -> std::result::Result<NewJob, serde_json::Error> {
            serde_json::from_value(json!({
                "kind": "reminder", "cadence": "once", "start_date": date,
                "params": {"title": "Call"},
            }))
        };
        let next_year = (OffsetDateTime::now_utc() + time::Duration::days(365)).date();
        let once_job = create(&db, once(&next_year.to_string())?, None);
        assert!(matches!(once_job.await, Err(JobError::Full(_))));
        delete(&db, &job.id, 3).await?;
        let once_job = create(&db, once(&next_year.to_string())?, None).await?;
        start_run(
            &db,
            &once_job,
            slot("2026-11-16T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Missed,
        )
        .await?;
        let missed = get(&db, &once_job.id).await?.ok_or("job")?;
        assert_eq!(
            (missed.enabled, missed.run_count, missed.revision),
            (false, 0, 2)
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn failures_pause_and_expired_jobs_of_jarvis_go(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let owner = uuid::Uuid::now_v7().to_string();
        let job = create(&db, prompt(), None).await?;
        // Two failures, a success, then two more failures: no pause yet.
        for failed in [true, true, false, true, true] {
            assert!(!record_outcome(&db, &job.id, failed).await?);
        }
        let streak = get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!((streak.failure_count, streak.enabled), (2, true));
        // The third in a row pauses it at the next revision.
        assert!(record_outcome(&db, &job.id, true).await?);
        let paused = get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!(
            (
                paused.failure_count,
                paused.enabled,
                paused.paused_reason,
                paused.revision
            ),
            (
                3,
                false,
                Some(super::super::PausedReason::PausedAfterFailures),
                2
            )
        );
        assert!(!record_outcome(&db, &job.id, true).await?);
        // Enabling it again clears the streak and the reason.
        let resumed = update(
            &db,
            &job.id,
            2,
            &JobPatch {
                enabled: Some(true),
                ..JobPatch::default()
            },
            None,
        )
        .await?;
        assert_eq!(
            (
                resumed.failure_count,
                resumed.paused_reason,
                resumed.enabled
            ),
            (0, None, true)
        );
        let conversation = uuid::Uuid::now_v7().to_string();
        set_conversation(&db, &job.id, &conversation).await?;
        let pointed = get(&db, &job.id).await?.ok_or("job")?;
        assert_eq!(
            pointed.conversation_id.as_deref(),
            Some(conversation.as_str())
        );
        assert_eq!(pointed.revision, resumed.revision);

        // An expired job of Jarvis's goes with its runs; others stay.
        let own = create_for_jarvis(&db, prompt(), &owner, None).await?;
        let expired = create_for_jarvis(&db, prompt(), &owner, None).await?;
        start_run(
            &db,
            &expired,
            slot("2026-11-09T06:00:00Z"),
            Trigger::Schedule,
            RunStatus::Missed,
        )
        .await?;
        run(
            &db,
            "UPDATE scheduled_jobs SET expires_at = time::now() - 1s WHERE record::id(id) = $id",
            json!({"id": expired.id}),
        )
        .await?;
        assert_eq!(
            delete_expired(&db).await?,
            std::slice::from_ref(&expired.id)
        );
        assert!(get(&db, &expired.id).await?.is_none());
        assert!(get(&db, &own.id).await?.is_some() && get(&db, &job.id).await?.is_some());
        let runs: Vec<String> = rows(
            &db,
            "SELECT VALUE record::id(id) FROM scheduled_job_runs WHERE job_id = $id",
            json!({"id": expired.id}),
        )
        .await?;
        assert!(runs.is_empty());
        assert!(delete_expired(&db).await?.is_empty());
        Ok(())
    }
}
