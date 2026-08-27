//! Logical coding sessions only. This endpoint never executes a command; a
//! subsequent approved broker may consume these bounded records.
use crate::{audit::record_security_event, AppState, Authed};
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
#[derive(Deserialize)]
pub(crate) struct SignedRun {
    request: jarvis_codex::SignedCodingRequest,
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

/// Forward a signed, typed start/resume request to the separate local broker.
/// This handler cannot execute Codex itself and intentionally has no command,
/// path, environment or OpenSandbox control fields.
pub(crate) async fn start_or_resume(
    a: Authed,
    State(s): State<AppState>,
    Path((id, mode)): Path<(Uuid, String)>,
    Json(body): Json<SignedRun>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let expected_start = match mode.as_str() {
        "start" => true,
        "resume" => false,
        _ => return Err(bad()),
    };
    let request = body.request;
    let session_matches = match &request.operation {
        jarvis_codex::CodingOperation::StartCodingRun {
            coding_session_id, ..
        }
        | jarvis_codex::CodingOperation::ResumeCodingRun {
            coding_session_id, ..
        } => *coding_session_id == id,
    };
    let type_matches = matches!(
        (&request.operation, expected_start),
        (jarvis_codex::CodingOperation::StartCodingRun { .. }, true)
            | (jarvis_codex::CodingOperation::ResumeCodingRun { .. }, false)
    );
    if request.user_id != a.user.id
        || request.device_id != a.device.id
        || !session_matches
        || !type_matches
        || request.message().is_err()
        || request
            .reject_if_expired(time::OffsetDateTime::now_utc())
            .is_err()
    {
        record_security_event(
            &s,
            Some(a.device.id),
            "coding_run",
            "denied",
            Some("invalid approval"),
        )
        .await;
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"coding operation denied"})),
        ));
    }
    let Some(socket) = s.codex_broker_socket.as_deref() else {
        record_security_event(
            &s,
            Some(a.device.id),
            "coding_run",
            "denied",
            Some("broker unavailable"),
        )
        .await;
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"secure coding execution unavailable"})),
        ));
    };
    let envelope = if expected_start {
        jarvis_codex::BrokerRequest::StartCodingRun { request }
    } else {
        jarvis_codex::BrokerRequest::ResumeCodingRun { request }
    };
    if forward_to_broker(socket, &envelope).await.is_err() {
        record_security_event(
            &s,
            Some(a.device.id),
            "coding_run",
            "denied",
            Some("broker rejected"),
        )
        .await;
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"coding operation denied"})),
        ));
    }
    record_security_event(
        &s,
        Some(a.device.id),
        "coding_run",
        "forwarded",
        Some(if expected_start { "start" } else { "resume" }),
    )
    .await;
    Ok(Json(
        json!({"status":"accepted","execution":"brokered_opensandbox"}),
    ))
}

async fn forward_to_broker(socket: &str, request: &jarvis_codex::BrokerRequest) -> Result<(), ()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let (read, mut write) = stream.into_split();
    let encoded = serde_json::to_vec(request).map_err(|_| ())?;
    if encoded.len() > 64 * 1024 {
        return Err(());
    }
    write.write_all(&encoded).await.map_err(|_| ())?;
    write.write_all(b"\n").await.map_err(|_| ())?;
    let mut reply = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        BufReader::new(read).read_line(&mut reply),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    (reply.trim() == r#"{"status":"accepted"}"#)
        .then_some(())
        .ok_or(())
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
