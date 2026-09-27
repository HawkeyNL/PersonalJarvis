//! Logical coding sessions only. This endpoint never executes a command; a
//! subsequent approved broker may consume these bounded records.
use crate::{audit::record_security_event, AppState, Authed};
use axum::{
    extract::{Path, State},
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
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
    #[serde(default)]
    owner_constraints: Vec<String>,
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
    if r.owner_constraints.len() > 8
        || r.owner_constraints
            .iter()
            .any(|value| value.trim().is_empty() || value.chars().count() > 1_000)
    {
        return Err(bad());
    }
    s.db.query("CREATE coding_sessions SET id=$id,user_id=$user_id,repository=$repository,base_revision=$base_revision,objective=$objective,owner_constraints=$owner_constraints,state='active',checkpoint=NONE,created_at=time::now(),updated_at=time::now() RETURN NONE")
 .bind(json!({"id":session.id.to_string(),"user_id":a.user.id.to_string(),"repository":session.repository,"base_revision":session.base_revision,"objective":session.objective,"owner_constraints":r.owner_constraints})).await.map_err(|_|err())?;
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

/// Owner-scoped factual run status. This never exposes a capability, provider
/// transcript or raw sandbox log.
pub(crate) async fn run_status(
    a: Authed,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(load_run(&s.db, a.user.id, id).await?))
}

async fn load_run(
    db: &jarvis_store::Database,
    user_id: Uuid,
    id: Uuid,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let mut response = db.query("SELECT record::id(id) AS run_id,coding_session_id,repository_id,repository_owner,repository_name,base_sha,snapshot_sha256,status,summary,failure_category,artifacts,compute_class,created_at,updated_at,completed_at FROM coding_runs WHERE record::id(id)=$id AND user_id=$user LIMIT 1")
        .bind(json!({"id":id.to_string(),"user":user_id.to_string()}))
        .await.map_err(|_|err())?;
    let rows: Vec<Value> = response.take(0).map_err(|_| err())?;
    let row = rows.into_iter().next().ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"no such coding run"})),
        )
    })?;
    Ok(row)
}

/// Cancellation is routed only to the local broker. Core verifies the owner
/// scope first, while the broker independently verifies its Unix peer and the
/// same durable run owner before stopping any workload.
pub(crate) async fn run_cancel(
    a: Authed,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let row = load_run(&s.db, a.user.id, id).await?;
    if row
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "completed" | "failed" | "timed_out" | "cancelled"))
    {
        return Ok(Json(json!({"status":"already_terminal"})));
    }
    let socket = s.codex_broker_socket.as_deref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"secure coding execution unavailable"})),
        )
    })?;
    let reply = forward_to_broker(
        socket,
        &jarvis_codex::BrokerRequest::CancelCodingRun {
            run_id: id,
            user_id: a.user.id,
        },
    )
    .await
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"coding cancellation unavailable"})),
        )
    })?;
    Ok(Json(json!({"status":reply})))
}

pub(crate) async fn run_artifact(
    a: Authed,
    State(s): State<AppState>,
    Path((id, name)): Path<(Uuid, String)>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let artifact = match name.as_str() {
        "result.json" => jarvis_codex::CodingArtifactName::ResultJson,
        "patch.diff" => jarvis_codex::CodingArtifactName::PatchDiff,
        _ => return Err(bad()),
    };
    let row = load_run(&s.db, a.user.id, id).await?;
    if row.get("status").and_then(Value::as_str) != Some("completed") {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({"error":"artifact unavailable"})),
        ));
    }
    let socket = s.codex_broker_socket.as_deref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"artifact service unavailable"})),
        )
    })?;
    let request = jarvis_codex::BrokerRequest::GetCodingArtifact {
        run_id: id,
        user_id: a.user.id,
        artifact,
    };
    let content = fetch_artifact(socket, &request, artifact.file_name())
        .await
        .map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"artifact service unavailable"})),
            )
        })?;
    let mime = if name == "result.json" {
        "application/json"
    } else {
        "text/plain; charset=utf-8"
    };
    Ok(([(CONTENT_TYPE, mime)], content).into_response())
}

async fn fetch_artifact(
    socket: &str,
    request: &jarvis_codex::BrokerRequest,
    expected_name: &str,
) -> Result<String, ()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let (read, mut write) = stream.into_split();
    let encoded = serde_json::to_vec(request).map_err(|_| ())?;
    write.write_all(&encoded).await.map_err(|_| ())?;
    write.write_all(b"\n").await.map_err(|_| ())?;
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        BufReader::new(read.take(1_100_001)).read_line(&mut line),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if line.is_empty() || line.len() > 1_100_000 || !line.ends_with('\n') {
        return Err(());
    }
    let reply: Value = serde_json::from_str(&line).map_err(|_| ())?;
    if reply.get("status").and_then(Value::as_str) != Some("artifact")
        || reply.get("name").and_then(Value::as_str) != Some(expected_name)
    {
        return Err(());
    }
    let content = reply.get("content").and_then(Value::as_str).ok_or(())?;
    if content.len() > 512 * 1024 {
        return Err(());
    }
    Ok(content.to_owned())
}

async fn forward_to_broker(
    socket: &str,
    request: &jarvis_codex::BrokerRequest,
) -> Result<String, ()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
        BufReader::new(read.take(4097)).read_line(&mut reply),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if reply.len() > 4096 {
        return Err(());
    }
    let status: Value = serde_json::from_str(&reply).map_err(|_| ())?;
    match status.get("status").and_then(Value::as_str) {
        Some("accepted") => Ok("accepted".into()),
        Some("cancelling") => Ok("cancelling".into()),
        _ => Err(()),
    }
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
