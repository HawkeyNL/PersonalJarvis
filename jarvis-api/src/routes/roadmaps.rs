//! In-app roadmaps for the owner (see `docs/ROADMAPS.md`). Every write names
//! the revision it builds on (`?expected_revision=N`) and gets 409 when the
//! roadmap has moved on. Bodies are capped by the router; each database call
//! is bounded by [`DB_TIMEOUT`].

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::bad_request;
use crate::roadmaps::store;
use crate::roadmaps::{ItemPatch, NewItem, NewRoadmap, RoadmapError, RoadmapPatch, MAX_HISTORY};
use crate::{AppState, Authed};

/// Request bodies of the roadmap routes.
pub(crate) const BODY_LIMIT: usize = 64 * 1024;
const DB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const DEFAULT_HISTORY: usize = 50;

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, RoadmapError>>,
) -> Result<T, (StatusCode, Json<Value>)> {
    let unavailable = || error(StatusCode::SERVICE_UNAVAILABLE, "roadmaps unavailable");
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(RoadmapError::Invalid(message))) => Err(bad_request(message)),
        Ok(Err(RoadmapError::NotFound)) => Err(error(StatusCode::NOT_FOUND, "not found")),
        Ok(Err(RoadmapError::Archived)) => Err(error(StatusCode::CONFLICT, "roadmap is archived")),
        Ok(Err(RoadmapError::Conflict)) => Err(error(
            StatusCode::CONFLICT,
            "roadmap changed; reload and retry",
        )),
        Ok(Err(RoadmapError::Stale)) => Err(error(
            StatusCode::CONFLICT,
            "proposal is stale; the roadmap changed since it was made",
        )),
        Ok(Err(RoadmapError::Limit(message))) => Err(error(StatusCode::CONFLICT, message)),
        Ok(Err(other)) => {
            tracing::warn!(error = %other, "roadmap operation failed");
            Err(unavailable())
        }
        Err(_) => {
            tracing::warn!("roadmap operation timed out");
            Err(unavailable())
        }
    }
}

fn snapshot(snapshot: crate::roadmaps::Snapshot) -> Json<Value> {
    Json(json!(snapshot))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListQuery {
    #[serde(default)]
    archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionQuery {
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryQuery {
    limit: Option<usize>,
}

pub(crate) async fn list(
    _authed: Authed,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> ApiResult {
    let roadmaps = bounded(store::list(&state.db, query.archived)).await?;
    Ok(Json(json!({ "roadmaps": roadmaps })))
}

pub(crate) async fn create(
    authed: Authed,
    State(state): State<AppState>,
    Json(input): Json<NewRoadmap>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::create(&state.db, &input, &device))
        .await
        .map(snapshot)
}

pub(crate) async fn get(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let found = bounded(store::get(&state.db, &id.to_string())).await?;
    found
        .map(snapshot)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "not found"))
}

pub(crate) async fn update(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
    Json(patch): Json<RoadmapPatch>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::update(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &patch,
        &device,
    ))
    .await
    .map(snapshot)
}

/// Archive, never delete.
pub(crate) async fn archive(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::archive(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &device,
    ))
    .await
    .map(snapshot)
}

pub(crate) async fn create_item(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(revision): Query<RevisionQuery>,
    Json(input): Json<NewItem>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    let (item_id, roadmap) = bounded(store::create_item(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &input,
        &device,
    ))
    .await?;
    let mut body = json!(roadmap);
    body["item_id"] = item_id.into();
    Ok(Json(body))
}

pub(crate) async fn update_item(
    authed: Authed,
    State(state): State<AppState>,
    Path((id, item)): Path<(Uuid, Uuid)>,
    Query(revision): Query<RevisionQuery>,
    Json(patch): Json<ItemPatch>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::update_item(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &item.to_string(),
        &patch,
        &device,
    ))
    .await
    .map(snapshot)
}

pub(crate) async fn delete_item(
    authed: Authed,
    State(state): State<AppState>,
    Path((id, item)): Path<(Uuid, Uuid)>,
    Query(revision): Query<RevisionQuery>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::delete_item(
        &state.db,
        &id.to_string(),
        revision.expected_revision,
        &item.to_string(),
        &device,
    ))
    .await
    .map(snapshot)
}

pub(crate) async fn history(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult {
    let limit = match query.limit {
        None => DEFAULT_HISTORY,
        Some(limit) if (1..=MAX_HISTORY).contains(&limit) => limit,
        Some(_) => return Err(bad_request("limit must be 1 to 100")),
    };
    let changes = bounded(store::history(&state.db, &id.to_string(), limit)).await?;
    Ok(Json(json!({ "changes": changes })))
}

pub(crate) async fn proposals(
    _authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let proposals = bounded(store::list_proposals(&state.db, &id.to_string())).await?;
    Ok(Json(json!({ "proposals": proposals })))
}

pub(crate) async fn accept_proposal(
    authed: Authed,
    State(state): State<AppState>,
    Path((id, proposal)): Path<(Uuid, Uuid)>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::accept_proposal(
        &state.db,
        &id.to_string(),
        &proposal.to_string(),
        &device,
    ))
    .await
    .map(snapshot)
}

pub(crate) async fn reject_proposal(
    authed: Authed,
    State(state): State<AppState>,
    Path((id, proposal)): Path<(Uuid, Uuid)>,
) -> ApiResult {
    let device = authed.device.id.to_string();
    bounded(store::reject_proposal(
        &state.db,
        &id.to_string(),
        &proposal.to_string(),
        &device,
    ))
    .await?;
    Ok(Json(json!({ "rejected": true })))
}
