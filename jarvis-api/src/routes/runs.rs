//! Additive asynchronous chat submission. A durable reservation is written
//! before spawning; ambiguous/crashed runs are never automatically retried.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use jarvis_client_core::realtime::{
    CanonicalMessage, ConversationMetadata, Event, MessageRole, RunFailure, RunIdentity,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use super::chat::{conversation_title, execute_chat, ChatReq};
use crate::{AppState, Authed};

type ApiError = (StatusCode, Json<Value>);
fn unavailable() -> ApiError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":"chat persistence unavailable"})),
    )
}
fn conflict() -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":"conversation is busy or request identity conflicts"})),
    )
}

#[derive(Deserialize)]
pub(crate) struct Submit {
    request_id: Uuid,
    #[serde(flatten)]
    chat: ChatReq,
}

#[derive(Deserialize)]
struct Stored {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    request_id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    conversation_id: Uuid,
    payload_hash: String,
    #[serde(with = "uuid::serde::hyphenated")]
    epoch: Uuid,
    state: String,
}

fn identity(user: Uuid, device: Uuid, request: Uuid) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"jarvis-assistant-run-v1\0");
    for id in [user, device, request] {
        hash.update(id.as_bytes());
    }
    let hash = hash.finalize();
    Uuid::from_slice(&hash[..16]).expect("fixed-size digest")
}

async fn load(state: &AppState, user: Uuid, id: Uuid) -> Result<Option<Stored>, ApiError> {
    let mut result = state.db.query("SELECT record::id(id) AS id, request_id, conversation_id, payload_hash, epoch, state FROM assistant_runs WHERE record::id(id) = $id AND user_id = $user LIMIT 1")
        .bind(json!({"id":id.to_string(), "user":user.to_string()})).await.map_err(|_| unavailable())?;
    result.take(0).map_err(|_| unavailable())
}

fn response(stored: &Stored, epoch: Uuid, active: bool) -> Json<Value> {
    let state = if stored.state == "running" && (stored.epoch != epoch || !active) {
        "interrupted"
    } else {
        &stored.state
    };
    Json(
        json!({"run_id":stored.id, "request_id":stored.request_id, "conversation_id":stored.conversation_id, "state":state}),
    )
}

pub(crate) async fn status(
    auth: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let stored = load(&state, auth.user.id, id)
        .await?
        .ok_or((StatusCode::NOT_FOUND, Json(json!({"error":"no such run"}))))?;
    Ok(response(
        &stored,
        state.realtime.epoch().ok_or_else(unavailable)?,
        state
            .realtime
            .identified_run_active(auth.user.id, stored.conversation_id, stored.id),
    ))
}

/// Recover a lost submission acknowledgement without resubmitting paid work.
/// The lookup key is derived from this authenticated device, never caller identity.
pub(crate) async fn request_status(
    auth: Authed,
    State(state): State<AppState>,
    Path(request): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let id = identity(auth.user.id, auth.device.id, request);
    status(auth, State(state), Path(id)).await
}

pub(crate) async fn submit(
    auth: Authed,
    State(state): State<AppState>,
    Json(mut req): Json<Submit>,
) -> Result<Json<Value>, ApiError> {
    let title = req.chat.validate_run()?;
    let encoded = serde_json::to_vec(&req.chat).map_err(|_| unavailable())?;
    let hash = format!("{:x}", Sha256::digest(encoded));
    let id = identity(auth.user.id, auth.device.id, req.request_id);
    let epoch = state.realtime.epoch().ok_or_else(unavailable)?;
    if let Some(stored) = load(&state, auth.user.id, id).await? {
        if stored.payload_hash != hash {
            return Err(conflict());
        }
        return Ok(response(
            &stored,
            epoch,
            state
                .realtime
                .identified_run_active(auth.user.id, stored.conversation_id, stored.id),
        ));
    }
    let fresh = req.chat.conversation_id.is_none();
    let conversation = req.chat.conversation_id.unwrap_or_else(Uuid::now_v7);
    if !fresh
        && conversation_title(&state.db, conversation, auth.user.id)
            .await
            .is_none()
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"no such conversation"})),
        ));
    }
    let guard = match state
        .realtime
        .reserve_identified_run(auth.user.id, conversation, id)
    {
        Some(guard) => guard,
        None => {
            // An identical request can arrive after the in-process reservation
            // but before its database transaction commits. Do not misreport it
            // as another prompt, or dispatch a second worker. Wait only for
            // this exact authenticated run, bounded independently of DB latency.
            if !state
                .realtime
                .identified_run_active(auth.user.id, conversation, id)
            {
                return Err(conflict());
            }
            return tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Some(stored) = load(&state, auth.user.id, id).await? {
                        if stored.payload_hash != hash {
                            return Err(conflict());
                        }
                        return Ok(response(
                            &stored,
                            epoch,
                            state.realtime.identified_run_active(
                                auth.user.id,
                                stored.conversation_id,
                                stored.id,
                            ),
                        ));
                    }
                    if !state
                        .realtime
                        .identified_run_active(auth.user.id, conversation, id)
                    {
                        return Err(unavailable());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|_| unavailable())?;
        }
    };
    let message = CanonicalMessage {
        id: Uuid::now_v7(),
        conversation_id: conversation,
        role: MessageRole::User,
        content: req.chat.new_user_content().ok_or_else(conflict)?.to_owned(),
        model: None,
        created_at: OffsetDateTime::now_utc(),
    };
    let at = message
        .created_at
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| unavailable())?;
    let result = state.db.query(
        "BEGIN TRANSACTION; \
         CREATE assistant_runs SET id = $id, user_id = $user, device_id = $device, request_id = $request, conversation_id = $conversation, payload_hash = $hash, epoch = $epoch, state = 'running', created_at = time::now(), updated_at = time::now(); \
         IF $fresh { CREATE conversations SET id = $conversation, user_id = $user, title = $title, created_at = time::now(), updated_at = time::now(); }; \
         CREATE chat_messages SET id = $message, user_id = $user, conversation_id = $conversation, role = 'user', content = $content, created_at = <datetime>$at; \
         UPDATE conversations SET updated_at = <datetime>$at WHERE record::id(id) = $conversation AND user_id = $user; \
         COMMIT TRANSACTION;"
    ).bind(json!({"id":id.to_string(), "user":auth.user.id.to_string(), "device":auth.device.id.to_string(), "request":req.request_id.to_string(), "conversation":conversation.to_string(), "hash":hash, "epoch":epoch.to_string(), "fresh":fresh, "title":title, "message":message.id.to_string(), "content":message.content, "at":at})).await.map_err(|_| unavailable())?;
    if result.check().is_err() {
        // A concurrent retry may have won the durable unique-index race.
        if let Some(stored) = load(&state, auth.user.id, id).await? {
            if stored.payload_hash == hash {
                return Ok(response(
                    &stored,
                    epoch,
                    state.realtime.identified_run_active(
                        auth.user.id,
                        stored.conversation_id,
                        stored.id,
                    ),
                ));
            }
        }
        return Err(conflict());
    }
    req.chat.conversation_id = Some(conversation);
    let run = RunIdentity {
        run_id: id,
        request_id: req.request_id,
        conversation_id: conversation,
    };
    let reply = Json(
        json!({"run_id":id, "request_id":req.request_id, "conversation_id":conversation, "state":"running"}),
    );
    let user = auth.user.id;
    state.realtime.claim_voice(user, auth.device.id, Some(id));
    if fresh {
        state.realtime.publish(
            user,
            Event::ConversationCreated(ConversationMetadata {
                id: conversation,
                title,
                updated_at: OffsetDateTime::now_utc(),
            }),
        );
    }
    state.realtime.publish(
        user,
        Event::MessageCreated {
            request_id: req.request_id,
            message: message.clone(),
        },
    );
    // Detached from the HTTP/socket lifetime. Dropping a display is not a
    // generation cancellation request. The guard also releases on unwind.
    tokio::spawn(async move {
        let _guard = guard;
        let result = execute_chat(
            auth,
            state.clone(),
            req.chat,
            Some(run.clone()),
            Some(message),
        )
        .await;
        let outcome = if result.is_ok() {
            "completed"
        } else {
            "failed"
        };
        if let Err((status, _)) = result {
            let reason = if status == StatusCode::INTERNAL_SERVER_ERROR {
                RunFailure::PersistenceUnavailable
            } else {
                RunFailure::ProviderUnavailable
            };
            state
                .realtime
                .publish(user, Event::AssistantFailed { run, reason });
        }
        let saved = state.db.query("UPDATE assistant_runs SET state = $state, updated_at = time::now() WHERE record::id(id) = $id AND user_id = $user RETURN NONE")
            .bind(json!({"state":outcome, "id":id.to_string(), "user":user.to_string()})).await;
        if !saved.map(|result| result.check().is_ok()).unwrap_or(false) {
            tracing::warn!("assistant run outcome persistence failed; never retry automatically");
        }
    });
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_run_stays_interrupted_while_another_run_uses_conversation() {
        let hub = crate::realtime::Hub::default();
        let user = Uuid::now_v7();
        let conversation = Uuid::now_v7();
        let old = Stored {
            id: Uuid::now_v7(),
            request_id: Uuid::now_v7(),
            conversation_id: conversation,
            payload_hash: String::new(),
            epoch: hub.epoch().unwrap(),
            state: "running".into(),
        };
        let _current = hub
            .reserve_identified_run(user, conversation, Uuid::now_v7())
            .unwrap();
        let status = response(
            &old,
            hub.epoch().unwrap(),
            hub.identified_run_active(user, conversation, old.id),
        );
        assert_eq!(status.0["state"], "interrupted");
        assert_eq!(status.0["run_id"], old.id.to_string());
    }
    #[test]
    fn request_identity_is_principal_bound() {
        let u = Uuid::now_v7();
        let d = Uuid::now_v7();
        let r = Uuid::now_v7();
        assert_eq!(identity(u, d, r), identity(u, d, r));
        assert_ne!(identity(u, d, r), identity(Uuid::now_v7(), d, r));
        assert_ne!(identity(u, d, r), identity(u, Uuid::now_v7(), r));
    }
}
