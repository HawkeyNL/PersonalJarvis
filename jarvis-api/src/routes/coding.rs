//! Logical coding sessions only. This endpoint never executes a command; a
//! subsequent approved broker may consume these bounded records.
use crate::{AppState, Authed};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Deserialize)]
pub(crate) struct Create {
    repository: String,
    base_revision: String,
    objective: String,
}
#[derive(Deserialize)]
pub(crate) struct Lifecycle {
    state: String,
}

pub(crate) async fn create(
    a: Authed,
    State(s): State<AppState>,
    Json(r): Json<Create>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let session = jarvis_codex::CodingSession::new(r.repository, r.base_revision, r.objective)
        .map_err(|_| bad())?;
    s.db.query("CREATE coding_sessions SET id=$id,user_id=$user_id,repository=$repository,base_revision=$base_revision,objective=$objective,state='active',checkpoint=NONE,created_at=time::now(),updated_at=time::now() RETURN NONE")
 .bind(json!({"id":session.id.to_string(),"user_id":a.user.id.to_string(),"repository":session.repository,"base_revision":session.base_revision,"objective":session.objective})).await.map_err(|_|err())?;
    Ok(Json(
        json!({"session_id":session.id,"state":"active","execution":"requires signed approval and OpenSandbox"}),
    ))
}
pub(crate) async fn list(a: Authed, State(s): State<AppState>) -> Json<Value> {
    let rows:Vec<Value>=s.db.query("SELECT record::id(id) AS id,repository,base_revision,objective,state,checkpoint,updated_at FROM coding_sessions WHERE user_id=$user_id ORDER BY updated_at DESC LIMIT 50").bind(json!({"user_id":a.user.id.to_string()})).await.ok().and_then(|mut x|x.take(0).ok()).unwrap_or_default();
    Json(json!({"sessions":rows}))
}
pub(crate) async fn lifecycle(
    a: Authed,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<Lifecycle>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !matches!(r.state.as_str(), "suspended" | "cancelled" | "archived") {
        return Err(bad());
    }
    let mut q=s.db.query("UPDATE coding_sessions SET state=$state,updated_at=time::now() WHERE record::id(id)=$id AND user_id=$user_id AND state IN ['active','suspended','completed','cancelled'] RETURN record::id(id) AS id").bind(json!({"state":r.state,"id":id.to_string(),"user_id":a.user.id.to_string()})).await.map_err(|_|err())?;
    let changed: Option<Value> = q.take(0).map_err(|_| err())?;
    if changed.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"no such coding session"})),
        ));
    };
    Ok(Json(json!({"status":"updated"})))
}
fn bad() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":"invalid coding session request"})),
    )
}
fn err() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error":"internal error"})),
    )
}
