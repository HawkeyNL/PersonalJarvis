//! Scheduled jobs for the owner (see `docs/SCHEDULED_JOBS.md`). Every write
//! names the revision it builds on (`?expected_revision=N`) and gets 409 when
//! the job has moved on. Bodies are capped by the router; each database call
//! is bounded by [`DB_TIMEOUT`].

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

use crate::error::bad_request;
use crate::jobs::store::{self, OWNER_RUN_HOURS};
use crate::jobs::{Job, JobError, JobPatch, NewJob, Params, MAX_RUNS, TIME_ZONE};
use crate::{AppState, Authed};

/// Request bodies of the job routes.
pub(crate) const BODY_LIMIT: usize = 4 * 1024;
const DB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
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
/// or the runner is off.
fn job(state: &AppState, job: Job) -> Value {
    let next = state
        .jobs
        .time_zone()
        .filter(|_| job.enabled)
        .and_then(|tz| job.next_slot(OffsetDateTime::now_utc(), tz))
        .and_then(|at| at.format(&Rfc3339).ok());
    let mut body = json!(job);
    body["next_run_at"] = json!(next);
    body
}

/// A job may only point at a roadmap that exists and is not archived.
async fn check_roadmap(state: &AppState, params: Option<&Params>) -> Result<(), Failure> {
    let Some(id) = params.and_then(|params| params.roadmap_id.as_deref()) else {
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
    _authed: Authed,
    State(state): State<AppState>,
    Json(input): Json<NewJob>,
) -> ApiResult {
    check_roadmap(&state, Some(&input.params)).await?;
    let created = bounded(store::create(&state.db, input, state.jobs.time_zone())).await?;
    Ok(Json(job(&state, created)))
}

pub(crate) async fn update(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
    Json(patch): Json<JobPatch>,
) -> ApiResult {
    check_roadmap(&state, patch.params.as_ref()).await?;
    let updated = bounded(store::update(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &patch,
        state.jobs.time_zone(),
    ))
    .await?;
    Ok(Json(job(&state, updated)))
}

/// Delete the job and its runs.
pub(crate) async fn delete(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
) -> ApiResult {
    bounded(store::delete(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
    ))
    .await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Run the job now (202), at most once an hour; the run is announced with
/// `jobs.run_finished` like a scheduled one.
pub(crate) async fn run_now(
    _authed: Authed,
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
