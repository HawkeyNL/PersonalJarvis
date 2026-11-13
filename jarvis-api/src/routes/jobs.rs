//! Scheduled jobs for the owner (see `docs/SCHEDULED_JOBS.md`). Every write
//! names the revision it builds on (`?expected_revision=N`) and gets 409 when
//! the job has moved on. Bodies are capped by the router; each database call
//! is bounded by [`DB_TIMEOUT`]. Who made a job, its conversation, expiry and
//! failure streak are Core's: a body that names them is refused.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_llm::privacy::is_private;

use crate::error::bad_request;
use crate::jobs::store::{self, OWNER_RUN_HOURS};
use crate::jobs::{Job, JobError, JobPatch, NewJob, Params, DB_TIMEOUT, MAX_RUNS, TIME_ZONE};
use crate::{AppState, Authed};

/// Request bodies of the job routes.
pub(crate) const BODY_LIMIT: usize = 4 * 1024;
const DEFAULT_RUNS: usize = 20;

type Failure = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, Failure>;

fn error(status: StatusCode, message: &str) -> Failure {
    (status, Json(json!({ "error": message })))
}

fn unavailable() -> Failure {
    error(StatusCode::SERVICE_UNAVAILABLE, "jobs unavailable")
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, JobError>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(JobError::Invalid(message))) => Err(bad_request(message)),
        Ok(Err(JobError::NotFound)) => Err(error(StatusCode::NOT_FOUND, "not found")),
        Ok(Err(JobError::Conflict)) => {
            Err(error(StatusCode::CONFLICT, "job changed; reload and retry"))
        }
        Ok(Err(JobError::Exists)) => Err(error(
            StatusCode::CONFLICT,
            "a job of this kind already exists",
        )),
        Ok(Err(JobError::Full(message))) => Err(error(StatusCode::CONFLICT, message)),
        Ok(Err(JobError::TooSoon)) => Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "a job runs on request at most once an hour",
        )),
        Ok(Err(other)) => {
            tracing::warn!(error = %other, "job operation failed");
            Err(unavailable())
        }
        Err(_) => {
            tracing::warn!("job operation timed out");
            Err(unavailable())
        }
    }
}

/// The job with `next_run_at`: its next slot, or null while it is disabled
/// or expired, or the runner is off.
fn job(state: &AppState, job: Job) -> Value {
    let now = OffsetDateTime::now_utc();
    let next = state
        .jobs
        .time_zone()
        .filter(|_| job.enabled)
        .filter(|_| job.expires_at.is_none_or(|at| at > now))
        .and_then(|tz| job.next_slot(now, tz))
        .and_then(|at| at.format(&Rfc3339).ok());
    let mut body = json!(job);
    body["next_run_at"] = json!(next);
    body
}

/// A job may only point at a roadmap that exists and is not archived.
async fn check_roadmap(state: &AppState, params: Option<&Params>) -> Result<(), Failure> {
    // A malformed id is refused by the job's own validation.
    let Some(id) = params
        .and_then(|params| params.roadmap_id.as_deref())
        .filter(|id| Uuid::parse_str(id).is_ok())
    else {
        return Ok(());
    };
    match tokio::time::timeout(DB_TIMEOUT, crate::roadmaps::store::get(&state.db, id)).await {
        Ok(Ok(Some(found))) if found.roadmap.archived_at.is_none() => Ok(()),
        Ok(Ok(_)) => Err(bad_request(
            "params.roadmap_id must name a roadmap that is not archived",
        )),
        Ok(Err(error)) => {
            tracing::warn!(%error, "job roadmap check failed");
            Err(unavailable())
        }
        Err(_) => Err(unavailable()),
    }
}

/// A prompt's instruction leaves the Home Node in a web search and its title
/// may, so neither may carry personal or financial details or a held
/// symbol. Fails closed when the holdings cannot be read.
async fn check_private(state: &AppState, params: Option<&Params>) -> Result<(), Failure> {
    let Some(params) = params.filter(|params| params.instruction.is_some()) else {
        return Ok(());
    };
    let Some(held) = crate::auto_research::held_symbols(state).await else {
        tracing::warn!("job privacy check: holdings unavailable");
        return Err(unavailable());
    };
    if is_private_job(params, &held) {
        return Err(bad_request(
            "params.title or params.instruction may hold personal or financial details; \
             leave them out",
        ));
    }
    Ok(())
}

/// Whether a prompt's instruction or title may hold personal or financial
/// details or a symbol in `held`; jobs without an instruction never do.
pub(crate) fn is_private_job(params: &Params, held: &[String]) -> bool {
    params.instruction.as_deref().is_some_and(|instruction| {
        is_private(instruction, held)
            || params
                .title
                .as_deref()
                .is_some_and(|title| is_private(title, held))
    })
}

/// One security audit row per owner change: the device and the job id,
/// never the job's text.
async fn audit(state: &AppState, authed: &Authed, event: &str, job_id: &str) {
    crate::audit::record_security_event(state, Some(authed.device.id), event, "ok", Some(job_id))
        .await;
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionQuery {
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunsQuery {
    limit: Option<usize>,
}

pub(crate) async fn list(_authed: Authed, State(state): State<AppState>) -> ApiResult {
    let jobs = bounded(store::list(&state.db)).await?;
    let status = if state.jobs.configured() {
        "ok"
    } else {
        "not_configured"
    };
    Ok(Json(json!({
        "status": status,
        "time_zone": TIME_ZONE,
        "jobs": jobs.into_iter().map(|found| job(&state, found)).collect::<Vec<_>>(),
    })))
}

pub(crate) async fn create(
    authed: Authed,
    State(state): State<AppState>,
    Json(mut input): Json<NewJob>,
) -> ApiResult {
    check_roadmap(&state, Some(&input.params)).await?;
    check_private(&state, Some(&input.params)).await?;
    input.owner_id = Some(authed.user.id.hyphenated().to_string());
    let created = bounded(store::create(&state.db, input, state.jobs.time_zone())).await?;
    audit(&state, &authed, "jobs.created", &created.id).await;
    Ok(Json(job(&state, created)))
}

pub(crate) async fn update(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
    Json(patch): Json<JobPatch>,
) -> ApiResult {
    check_roadmap(&state, patch.params.as_ref()).await?;
    check_private(&state, patch.params.as_ref()).await?;
    let updated = bounded(store::update(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &patch,
        state.jobs.time_zone(),
    ))
    .await?;
    if updated.revision != revision.expected_revision {
        audit(&state, &authed, "jobs.updated", &updated.id).await;
    }
    Ok(Json(job(&state, updated)))
}

/// Delete the job and its runs.
pub(crate) async fn delete(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
) -> ApiResult {
    let id = id.to_string();
    bounded(store::delete(&state.db, &id, revision.expected_revision)).await?;
    audit(&state, &authed, "jobs.deleted", &id).await;
    Ok(Json(json!({ "deleted": true })))
}

/// Run the job now (202), at most once an hour; the run is announced with
/// `jobs.run_finished` like a scheduled one.
pub(crate) async fn run_now(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    if !state.jobs.configured() {
        return Err(error(
            StatusCode::CONFLICT,
            "scheduled jobs are not configured",
        ));
    }
    let id = id.to_string();
    if bounded(store::get(&state.db, &id)).await?.is_none() {
        return Err(error(StatusCode::NOT_FOUND, "not found"));
    }
    let last = bounded(store::last_owner_run(&state.db, &id)).await?;
    let interval = time::Duration::hours(OWNER_RUN_HOURS.into());
    if last.is_some_and(|at| OffsetDateTime::now_utc() - at < interval) {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "a job runs on request at most once an hour",
        ));
    }
    state
        .jobs
        .request_run(&id)
        .map_err(|message| error(StatusCode::TOO_MANY_REQUESTS, message))?;
    audit(&state, &authed, "jobs.run_requested", &id).await;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

pub(crate) async fn runs(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<RunsQuery>,
) -> ApiResult {
    let limit = match query.limit {
        None => DEFAULT_RUNS,
        Some(limit) if (1..=MAX_RUNS).contains(&limit) => limit,
        Some(_) => return Err(bad_request("limit must be 1 to 50")),
    };
    let runs = bounded(store::list_runs(&state.db, &id.to_string(), limit)).await?;
    Ok(Json(json!({ "runs": runs })))
}

/// One run with its facts.
pub(crate) async fn run(
    _authed: Authed,
    State(state): State<AppState>,
    Path(run_id): Path<Uuid>,
) -> ApiResult {
    let (found, facts) = bounded(store::get_run(&state.db, &run_id.to_string()))
        .await?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "not found"))?;
    let mut body = json!(found);
    body["facts"] = facts.unwrap_or(Value::Null);
    Ok(Json(body))
}
