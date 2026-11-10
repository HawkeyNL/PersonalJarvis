//! Typed repository for scheduled jobs and their runs (schema 0019).
//!
//! Owner writes name the revision they build on and bump it. The runner
//! never bumps it: it writes a run row and the slot it handled in one
//! transaction that holds only while the job is still at the revision the
//! runner loaded, so a schedule change in between wins and the runner
//! reloads. All values are bound parameters; only constants are formatted
//! into queries.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_store::Database;

use super::{
    Job, JobError, JobPatch, NewJob, Priority, Result, Run, RunStatus, SummarySource, Trigger,
    MAX_FACTS_BYTES, MAX_RUNS, RUN_RETENTION_DAYS,
};

/// An owner may start a job at most once per this many hours.
pub const OWNER_RUN_HOURS: u32 = 1;
/// Rows deleted per statement and statements per sweep.
const PRUNE_BATCH: usize = 5_000;
const PRUNE_MAX_BATCHES: usize = 20;

const JOB_FIELDS: &str = "record::id(id) AS id, kind, cadence, weekday, at_minute, enabled, \
     revision, params, last_slot_at, created_at, updated_at";
const RUN_FIELDS: &str = "record::id(id) AS id, job_id, kind, slot_at, trigger, status, \
     error_code, summary, summary_source, priorities, started_at, finished_at";

async fn run(db: &Database, query: &str, bindings: Value) -> Result<surrealdb::Response> {
    Ok(db.query(query).bind(bindings).await?.check()?)
}

async fn rows<T: DeserializeOwned>(db: &Database, query: &str, bindings: Value) -> Result<Vec<T>> {
    Ok(run(db, query, bindings).await?.take(0)?)
}

/// Every job, oldest first. There is at most one per kind.
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

/// Create a job; at most one per kind.
pub async fn create(db: &Database, input: NewJob) -> Result<Job> {
    let job = input.into_job(uuid::Uuid::now_v7().to_string(), OffsetDateTime::now_utc())?;
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             IF array::len((SELECT VALUE id FROM scheduled_jobs WHERE kind = $kind)) > 0 \
               { THROW 'job exists'; }; \
             CREATE scheduled_jobs SET id = $id, kind = $kind, cadence = $cadence, \
               weekday = $weekday, at_minute = $at_minute, enabled = $enabled, revision = 1, \
               params = $params, last_slot_at = NONE, created_at = time::now(), \
               updated_at = time::now() RETURN NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({
            "id": job.id,
            "kind": job.kind,
            "cadence": job.cadence,
            "weekday": job.weekday,
            "at_minute": job.at_minute,
            "enabled": job.enabled,
            "params": job.params,
        }))
        .await?;
    if let Err(error) = response.check() {
        let exists = list(db).await?.iter().any(|known| known.kind == job.kind);
        return Err(if exists {
            JobError::Exists
        } else {
            error.into()
        });
    }
    get(db, &job.id).await?.ok_or(JobError::InvalidData)
}

/// Change a job at revision `expected`; a change that changes nothing writes
/// nothing. A schedule change clears `last_slot_at`, so no slot before it
/// runs; otherwise the runner's value is kept as stored.
pub async fn update(db: &Database, id: &str, expected: u64, patch: &JobPatch) -> Result<Job> {
    let job = get(db, id).await?.ok_or(JobError::NotFound)?;
    if job.revision != expected {
        return Err(JobError::Conflict);
    }
    let (new, rescheduled) = job.patched(patch)?;
    if new == job {
        return Ok(job);
    }
    let updated: Vec<u64> = rows(
        db,
        "UPDATE scheduled_jobs SET cadence = $cadence, weekday = $weekday, \
           at_minute = $at_minute, enabled = $enabled, params = $params, \
           last_slot_at = IF $rescheduled THEN NONE ELSE last_slot_at END, \
           revision = $revision, updated_at = time::now() \
         WHERE record::id(id) = $id AND revision = $expected RETURN VALUE revision",
        json!({
            "id": id,
            "expected": expected,
            "revision": expected + 1,
            "cadence": new.cadence,
            "weekday": new.weekday,
            "at_minute": new.at_minute,
            "enabled": new.enabled,
            "params": new.params,
            "rescheduled": rescheduled,
        }),
    )
    .await?;
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

/// Write the row of a run before it starts, or a `missed` row, and return
/// its id. A scheduled row is written only while the job is enabled and at
/// the revision `job` was loaded at, and records `slot_at` as handled; an
/// owner's row only once per [`OWNER_RUN_HOURS`]. A refused write (also a
/// second row for the same slot) is an error and writes nothing.
pub async fn start_run(
    db: &Database,
    job: &Job,
    slot_at: OffsetDateTime,
    trigger: Trigger,
    status: RunStatus,
) -> Result<String> {
    if !matches!(status, RunStatus::Running | RunStatus::Missed) {
        return Err(JobError::Invalid("a run starts as running or missed"));
    }
    let run_id = uuid::Uuid::now_v7().to_string();
    run(
        db,
        &format!(
            "BEGIN TRANSACTION; \
             LET $slot = <datetime>$slot_at; \
             LET $job = (UPDATE scheduled_jobs \
               SET last_slot_at = IF $owner THEN last_slot_at ELSE $slot END \
               WHERE record::id(id) = $job_id \
               AND ($owner OR (revision = $revision AND enabled = true)) RETURN VALUE id); \
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
        ),
        json!({
            "id": run_id,
            "job_id": job.id,
            "revision": job.revision,
            "kind": job.kind,
            "slot_at": slot_at.format(&Rfc3339).map_err(|_| JobError::InvalidData)?,
            "owner": trigger == Trigger::Owner,
            "trigger": trigger,
            "status": status,
        }),
    )
    .await?;
    Ok(run_id)
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

/// Record how a running run ended; a run that is no longer running (Core
/// restarted and marked it interrupted) stays as it is.
pub async fn finish_run(db: &Database, run_id: &str, finish: &Finish) -> Result<bool> {
    if matches!(finish.status, RunStatus::Running | RunStatus::Missed)
        || finish
            .facts
            .as_ref()
            .is_some_and(|f| f.len() > MAX_FACTS_BYTES)
    {
        return Err(JobError::Invalid("not a final run state"));
    }
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

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn jobs_are_one_per_kind_and_revision_guarded(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let job = create(&db, new_job()).await?;
        assert_eq!(
            (job.cadence, job.weekday, job.at_minute, job.revision),
            (Cadence::Weekly, Some(1), 420, 1)
        );
        assert!(job.enabled && job.last_slot_at.is_none());
        assert!(matches!(
            create(&db, new_job()).await,
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
                }),
                ..JobPatch::default()
            },
        )
        .await?;
        assert_eq!(linked.revision, 2);
        assert_eq!(linked.last_slot_at, Some(monday));
        assert_eq!(linked.params.roadmap_id.as_deref(), Some(roadmap));
        assert!(matches!(
            update(&db, id, 1, &JobPatch::default()).await,
            Err(JobError::Conflict)
        ));
        // Nothing changes: no new revision.
        assert_eq!(update(&db, id, 2, &JobPatch::default()).await?.revision, 2);
        let daily = update(
            &db,
            id,
            2,
            &JobPatch {
                cadence: Some(Cadence::Daily),
                at_minute: Some(1439),
                ..JobPatch::default()
            },
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
                }
            )
            .await,
            Err(JobError::Invalid(_))
        ));
        let unknown = uuid::Uuid::now_v7().to_string();
        assert!(matches!(
            update(&db, &unknown, 1, &JobPatch::default()).await,
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
        // The kind is free again.
        assert_eq!(create(&db, new_job()).await?.kind, Kind::DependencyReview);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn runs_are_written_first_once_per_slot_and_interrupted_on_restart(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = database().await?;
        let job = create(&db, new_job()).await?;
        let monday = slot("2026-11-09T06:00:00Z");
        let run_id = start_run(&db, &job, monday, Trigger::Schedule, RunStatus::Running).await?;
        assert_eq!(get(&db, &job.id).await?.unwrap().last_slot_at, Some(monday));
        // The same slot twice, or from a job that changed meanwhile, writes
        // nothing.
        assert!(
            start_run(&db, &job, monday, Trigger::Schedule, RunStatus::Missed)
                .await
                .is_err()
        );
        update(
            &db,
            &job.id,
            1,
            &JobPatch {
                at_minute: Some(480),
                ..JobPatch::default()
            },
        )
        .await?;
        let next = slot("2026-11-16T07:00:00Z");
        assert!(
            start_run(&db, &job, next, Trigger::Schedule, RunStatus::Running)
                .await
                .is_err()
        );
        let off = update(
            &db,
            &job.id,
            2,
            &JobPatch {
                enabled: Some(false),
                ..JobPatch::default()
            },
        )
        .await?;
        assert!(
            start_run(&db, &off, next, Trigger::Schedule, RunStatus::Running)
                .await
                .is_err()
        );

        // The owner may run a disabled job, once an hour; the slot stays.
        let now = OffsetDateTime::now_utc();
        let owner = start_run(&db, &off, now, Trigger::Owner, RunStatus::Running).await?;
        assert!(start_run(
            &db,
            &off,
            now + time::Duration::seconds(1),
            Trigger::Owner,
            RunStatus::Running
        )
        .await
        .is_err());
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
        assert!(finish_run(
            &db,
            &run_id,
            &Finish {
                status: RunStatus::Running,
                ..finish.clone()
            }
        )
        .await
        .is_err());

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
}
