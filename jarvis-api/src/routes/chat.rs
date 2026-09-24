//! Chat with the brain + conversation persistence (ADR-030) and multi-step
//! orchestration (ADR-028). The persona is prepended server-side and the API key
//! is never exposed. Titles are deterministic and do not spend model tokens.
//! Final presentation events follow confirmed canonical persistence.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_client_core::realtime::{
    CanonicalMessage, ConversationMetadata, Event, MessageRole, RunIdentity,
};
use jarvis_llm as llm;
use jarvis_orchestrator as orchestrator;

use crate::error::bad_request;
use crate::metering::{record_usage, record_usage_with_metadata};
use crate::rate_limit::allow_authenticated_device;
use crate::routes::system::{validate_brain_selection, BrainPreferenceReq};
use crate::validation;
use crate::{AppState, Authed};

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ChatTurn {
    role: String,
    content: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct ChatReq {
    messages: Vec<ChatTurn>,
    /// Optional tier hint: `default` | `hard` | `cheap`.
    #[serde(default)]
    tier: Option<String>,
    /// Provider-neutral request intent. `tier` is retained only for older
    /// clients and maps to the same semantic routing floor.
    #[serde(default)]
    mode: Option<String>,
    /// Optional system-prompt override (defaults to the Jarvis persona).
    #[serde(default)]
    system: Option<String>,
    /// Absent starts a fresh conversation; present continues the owned one.
    #[serde(default)]
    pub(super) conversation_id: Option<Uuid>,
}

impl ChatReq {
    pub(super) fn new_user_content(&self) -> Option<&str> {
        self.messages
            .last()
            .filter(|m| m.role == "user")
            .map(|m| m.content.trim())
            .filter(|text| !text.is_empty())
    }
    pub(super) fn validate_run(&self) -> Result<String, (StatusCode, Json<Value>)> {
        if self.messages.len() > validation::MAX_CHAT_TURNS
            || self
                .messages
                .iter()
                .any(|m| m.content.len() > validation::MAX_CHAT_CONTENT_LEN)
            || self.messages.iter().map(|m| m.content.len()).sum::<usize>() > 128_000
            || self.system.as_ref().is_some_and(|s| s.len() > 24_000)
        {
            return Err(bad_request("chat payload too large"));
        }
        let last = self
            .messages
            .last()
            .filter(|m| m.role == "user" && !m.content.trim().is_empty())
            .ok_or_else(|| bad_request("last message must be a nonempty user message"))?;
        Ok(derive_title(&last.content))
    }
}

/// Synchronous response compatibility for old clients. Its single provider
/// execution still publishes the same live events to newer connected clients.
pub(crate) async fn assistant_chat(
    authed: Authed,
    State(state): State<AppState>,
    Json(mut req): Json<ChatReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let title = req.validate_run()?;
    let fresh = req.conversation_id.is_none();
    let conversation = match req.conversation_id {
        Some(id) => {
            if conversation_title(&state.db, id, authed.user.id)
                .await
                .is_none()
            {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"no such conversation"})),
                ));
            }
            id
        }
        None => create_conversation(&state.db, authed.user.id, &title)
            .await
            .map_err(|_| internal_error())?,
    };
    let guard = state
        .realtime
        .reserve_run(authed.user.id, conversation)
        .ok_or((
            StatusCode::CONFLICT,
            Json(json!({"error":"conversation has an active run"})),
        ))?;
    req.conversation_id = Some(conversation);
    let run = RunIdentity {
        run_id: Uuid::now_v7(),
        request_id: Uuid::now_v7(),
        conversation_id: conversation,
    };
    let user = authed.user.id;
    state
        .realtime
        .claim_voice(user, authed.device.id, Some(run.run_id));
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
    // Even an old client's HTTP disconnect must not cancel a shared answer.
    // Legacy requests have no caller request UUID; only the additive runs API
    // offers retry idempotency. Never automatically replay this endpoint.
    tokio::spawn(async move {
        let _guard = guard;
        let mut result = execute_chat(authed, state.clone(), req, Some(run.clone()), None).await;
        if let Ok(Json(value)) = &mut result {
            value["new_topic"] = json!(fresh);
        } else {
            state.realtime.publish(
                user,
                Event::AssistantFailed {
                    run,
                    reason: jarvis_client_core::realtime::RunFailure::ProviderUnavailable,
                },
            );
        }
        result
    })
    .await
    .map_err(|_| internal_error())?
}

pub(super) async fn execute_chat(
    authed: Authed,
    state: AppState,
    req: ChatReq,
    realtime_run: Option<RunIdentity>,
    persisted_user_message: Option<CanonicalMessage>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !allow_authenticated_device(
        &state,
        authed.device.id,
        "llm",
        state.auth_limits.llm_per_min,
    ) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "rate limited",
                "hint": "te veel pogingen; probeer het straks opnieuw",
            })),
        ));
    }
    // Generous bounds so a single request cannot ship an unbounded transcript or
    // a giant paste into the LLM (real conversations stay well under these).
    if req.messages.len() > validation::MAX_CHAT_TURNS {
        return Err(bad_request("too many messages"));
    }
    if req
        .messages
        .iter()
        .any(|t| t.content.len() > validation::MAX_CHAT_CONTENT_LEN)
    {
        return Err(bad_request("message too long"));
    }
    if req
        .messages
        .iter()
        .map(|turn| turn.content.len())
        .sum::<usize>()
        > 128_000
    {
        return Err(bad_request("conversation payload too large"));
    }
    let history: Vec<llm::ChatMessage> = req
        .messages
        .iter()
        .filter_map(|t| {
            let content = t.content.trim();
            if content.is_empty() {
                return None;
            }
            Some(match t.role.as_str() {
                "assistant" | "jarvis" => llm::ChatMessage::assistant(content),
                _ => llm::ChatMessage::user(content),
            })
        })
        .collect();
    if history.is_empty() {
        return Err(bad_request("messages is required"));
    }
    // The last user turn is the new message to store and (maybe) reclassify.
    let new_msg = req
        .messages
        .iter()
        .rev()
        .find(|t| !matches!(t.role.as_str(), "assistant" | "jarvis"))
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_request("a user message is required"))?;

    // Which conversation does this belong to? Append to the current one unless
    // the topic shifted; with no valid current one, start fresh.
    let existing = match req.conversation_id {
        Some(cid) => conversation_title(&state.db, cid, authed.user.id)
            .await
            .map(|t| (cid, t)),
        None => None,
    };
    let (conv_id, conv_title, new_topic) = match (existing, realtime_run.as_ref()) {
        (Some((cid, title)), Some(_)) => (cid, title, false),
        (None, Some(_)) => return Err(internal_error()),
        (Some((cid, title)), None) => {
            let (same, proposed) = classify_topic(&state, Some(&title), &new_msg).await;
            if same {
                (cid, title, false)
            } else {
                let id = create_conversation(&state.db, authed.user.id, &proposed)
                    .await
                    .map_err(|_| internal_error())?;
                (id, proposed, true)
            }
        }
        (None, None) => {
            let (_same, proposed) = classify_topic(&state, None, &new_msg).await;
            let id = create_conversation(&state.db, authed.user.id, &proposed)
                .await
                .map_err(|_| internal_error())?;
            (id, proposed, true)
        }
    };

    // Save what the owner said up front, so it survives even a brain outage.
    let user_already_published = persisted_user_message.is_some();
    let user_message = match persisted_user_message {
        Some(message) => message,
        None => append_message(&state.db, conv_id, authed.user.id, "user", &new_msg, None)
            .await
            .map_err(|_| internal_error())?,
    };
    let canonical_history = if user_already_published {
        Some(
            super::chat_context::load(&state.db, authed.user.id, &user_message)
                .await
                .map_err(|_| internal_error())?,
        )
    } else {
        None
    };
    if let Some(run) = &realtime_run {
        if !user_already_published {
            state.realtime.publish(
                authed.user.id,
                Event::MessageCreated {
                    request_id: run.request_id,
                    message: user_message,
                },
            );
        }
        state
            .realtime
            .publish(authed.user.id, Event::AssistantStarted(run.clone()));
    }

    // A fresh topic starts with a clean slate; a continuation keeps its context.
    let messages = if let Some(canonical) = canonical_history {
        canonical
    } else if new_topic {
        vec![llm::ChatMessage::user(&new_msg)]
    } else {
        history
    };
    let requested_mode = req
        .mode
        .as_deref()
        .map(llm::RoutingMode::parse)
        .unwrap_or_else(|| {
            req.tier
                .as_deref()
                .map(|tier| match llm::Tier::parse(tier) {
                    llm::Tier::Cheap => llm::RoutingMode::Fast,
                    llm::Tier::Hard => llm::RoutingMode::Deep,
                    llm::Tier::Default => llm::RoutingMode::Auto,
                })
                .unwrap_or_default()
        });
    let owner_brain = if requested_mode == llm::RoutingMode::Auto {
        match conversation_brain_override(&state.db, conv_id, authed.user.id).await {
            Some(preferred) => Some(preferred),
            None => async_global_brain(&state, authed.user.id).await,
        }
    } else {
        None
    };
    let owner_brain_pinned = owner_brain
        .as_ref()
        .is_some_and(|(provider, model)| state.model_policy.allows(provider, model));
    // Jev classifies a bounded copy of the latest user turn. The owner-selected
    // mode and deterministic safety floor retain final authority. A Jev result
    // cannot name a model, trigger a tool, or approve a side effect.
    let intent_kind = if crate::intent::should_classify(requested_mode, owner_brain_pinned) {
        crate::intent::decide(&state, &new_msg).await
    } else {
        None
    };
    let mode = crate::intent::available_route_mode(
        requested_mode,
        intent_kind,
        &new_msg,
        state.llm.as_ref(),
    );
    // Classification establishes a quality floor only. The complete original
    // conversation below remains the model input; no lossy summary or hidden
    // model-selection prompt is substituted for the owner's request.
    let requirements = llm::classify_task(mode, &new_msg);
    let mut chat = llm::ChatRequest {
        system: Some(
            req.system
                .unwrap_or_else(|| state.jarvis_system.to_string()),
        ),
        tier: requirements.tier,
        mode,
        messages,
        max_tokens: state.llm_max_tokens,
        // The router picks the concrete model per backend (ADR-028 fase 2).
        model: None,
    };
    // Explicit Deep/Research requests retain their quality-floor semantics.
    // The owner default is only applied to ordinary Auto conversation turns;
    // every persisted selection was allowlist-validated and is checked again
    // here to fail closed after a policy reload/revocation.
    if mode == llm::RoutingMode::Auto {
        if let Some((provider, model)) = owner_brain {
            if state.model_policy.allows(&provider, &model) {
                chat.model = Some(model);
            }
        }
    }

    let llm_started = Instant::now();
    let request_id = realtime_run
        .as_ref()
        .map(|run| run.run_id.to_string())
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let reply = if let Some(run) = &realtime_run {
        let hub = state.realtime.clone();
        let run = run.clone();
        let user = authed.user.id;
        state
            .llm
            .chat_stream(
                &chat,
                std::sync::Arc::new(move |text| {
                    hub.publish(
                        user,
                        Event::AssistantDelta {
                            run: run.clone(),
                            text: text.to_string(),
                        },
                    );
                }),
            )
            .await
    } else {
        state.llm.chat(&chat).await
    };
    match reply {
        Ok(mut reply) => {
            let estimated_usage = reply.backend.is_some() && reply.usage.is_none();
            if estimated_usage {
                // Missing stream usage must not turn a metered success into
                // an unrecorded/free request. Charge a conservative input-byte
                // ceiling plus framing and the requested output-token limit.
                let input_bytes = chat
                    .messages
                    .iter()
                    .map(|m| m.content.len().saturating_add(32))
                    .sum::<usize>()
                    .saturating_add(chat.system.as_ref().map_or(0, String::len))
                    .saturating_add(1024);
                reply.usage = Some(llm::Usage {
                    input_tokens: input_bytes.min(i32::MAX as usize) as u32,
                    output_tokens: chat.max_tokens,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                });
            }
            record_usage_with_metadata(
                &state,
                &reply,
                jarvis_usage::UsageMetadata {
                    request_id,
                    routing_mode: format!("{mode:?}").to_ascii_lowercase(),
                    quality_tier: format!("{:?}", requirements.tier).to_ascii_lowercase(),
                    latency_ms: llm_started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                    status: if estimated_usage {
                        "succeeded_estimated_usage"
                    } else {
                        "succeeded"
                    }
                    .into(),
                    ..Default::default()
                },
            )
            .await;
            let message = append_message(
                &state.db,
                conv_id,
                authed.user.id,
                "assistant",
                &reply.text,
                Some(reply.model.as_str()),
            )
            .await
            .map_err(|_| internal_error())?;
            if let Some(run) = &realtime_run {
                state.realtime.publish(
                    authed.user.id,
                    Event::AssistantCompleted {
                        run: run.clone(),
                        message,
                    },
                );
                state.realtime.publish(
                    authed.user.id,
                    Event::ConversationUpdated(ConversationMetadata {
                        id: conv_id,
                        title: conv_title.clone(),
                        updated_at: OffsetDateTime::now_utc(),
                    }),
                );
            }
            Ok(Json(json!({
                "reply": reply.text,
                "model": reply.model,
                "backend": reply.backend,
                "routing_reason": requirements.routing_reason,
                "stop_reason": reply.stop_reason,
                "conversation_id": conv_id,
                "conversation_title": conv_title,
                "new_topic": new_topic,
                "routing_mode": mode,
                "intent_kind": intent_kind.map(crate::intent::WorkKind::as_str),
            })))
        }
        Err(llm::LlmError::Refused) => {
            let text = "Sorry, daar kan ik niet op antwoorden.";
            let message =
                append_message(&state.db, conv_id, authed.user.id, "assistant", text, None)
                    .await
                    .map_err(|_| internal_error())?;
            if let Some(run) = &realtime_run {
                state.realtime.publish(
                    authed.user.id,
                    Event::AssistantCompleted {
                        run: run.clone(),
                        message,
                    },
                );
            }
            Ok(Json(json!({
                "reply": text,
                "model": Value::Null,
                "stop_reason": "refusal",
                "conversation_id": conv_id,
                "conversation_title": conv_title,
                "new_topic": new_topic,
            })))
        }
        Err(e) => {
            // Details stay in logs; the client gets an opaque, actionable hint.
            // The user's message is already saved under `conv_id`.
            tracing::warn!(failure = ?e.failure_category(), "assistant chat failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer JARVIS_LLM_API_KEY of start Ollama lokaal",
                    "conversation_id": conv_id,
                })),
            ))
        }
    }
}

async fn conversation_brain_override(
    db: &jarvis_store::Database,
    conversation_id: Uuid,
    user_id: Uuid,
) -> Option<(String, String)> {
    #[derive(Deserialize)]
    struct Row {
        brain_provider: Option<String>,
        brain_model: Option<String>,
    }
    db.query("SELECT brain_provider, brain_model FROM conversations WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1")
        .bind(json!({"id":conversation_id.to_string(), "user_id":user_id.to_string()})).await.ok()
        .and_then(|mut r| r.take::<Option<Row>>(0).ok()).flatten()
        .and_then(|r| r.brain_provider.zip(r.brain_model))
}

async fn async_global_brain(state: &AppState, user_id: Uuid) -> Option<(String, String)> {
    #[derive(Deserialize)]
    struct Row {
        provider: Option<String>,
        model: Option<String>,
    }
    state
        .db
        .query(
            "SELECT provider, model FROM owner_brain_preferences WHERE user_id = $user_id LIMIT 1",
        )
        .bind(json!({"user_id":user_id.to_string()}))
        .await
        .ok()
        .and_then(|mut r| r.take::<Option<Row>>(0).ok())
        .flatten()
        .and_then(|r| r.provider.zip(r.model))
}

/// Preserve the legacy response shape without a second paid classification
/// call. Conversation changes are explicit owner navigation, not model guesses.
async fn classify_topic(
    _state: &AppState,
    current_title: Option<&str>,
    new_msg: &str,
) -> (bool, String) {
    (
        current_title.is_some(),
        current_title
            .map(str::to_string)
            .unwrap_or_else(|| derive_title(new_msg)),
    )
}

/// A short, single-line title derived from the first user message.
fn derive_title(msg: &str) -> String {
    let t = clean_title(msg);
    if t.is_empty() {
        "Nieuw gesprek".to_string()
    } else {
        t
    }
}

/// Normalize a title: single line, trimmed, capped at ~48 chars.
fn clean_title(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = one_line.chars().take(48).collect();
    capped.trim().to_string()
}

/// A conversation's title, if it belongs to this user.
fn internal_error() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

#[derive(Deserialize)]
struct TitleRow {
    title: String,
}

pub(super) async fn conversation_title(
    db: &jarvis_store::Database,
    id: Uuid,
    user_id: Uuid,
) -> Option<String> {
    let mut response = db.query(
        "SELECT title FROM conversations WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1",
    ).bind(json!({"id": id.to_string(), "user_id": user_id.to_string()})).await.ok()?;
    response
        .take::<Option<TitleRow>>(0)
        .ok()
        .flatten()
        .map(|row| row.title)
}

/// Create a new conversation and return its id (ADR-030).
async fn create_conversation(
    db: &jarvis_store::Database,
    user_id: Uuid,
    title: &str,
) -> Result<Uuid, ()> {
    let id = Uuid::now_v7();
    db.query(
        "CREATE conversations SET id = $id, user_id = $user_id, title = $title, \
         created_at = time::now(), updated_at = time::now() RETURN NONE",
    )
    .bind(json!({"id": id.to_string(), "user_id": user_id.to_string(), "title": title}))
    .await
    .map_err(|_| ())?
    .check()
    .map_err(|_| ())?;
    Ok(id)
}

/// Append a canonical message and bump `updated_at` atomically. Persistence
/// must succeed before publishing a completion to any display.
async fn append_message(
    db: &jarvis_store::Database,
    conv_id: Uuid,
    user_id: Uuid,
    role: &str,
    content: &str,
    model: Option<&str>,
) -> Result<CanonicalMessage, ()> {
    let id = Uuid::now_v7();
    let at = OffsetDateTime::now_utc();
    let res = db.query(
        "BEGIN TRANSACTION; CREATE chat_messages SET id = $id, conversation_id = $conversation_id, \
         user_id = $user_id, role = $role, content = $content, model = $model, created_at = <datetime>$at; \
         UPDATE conversations SET updated_at = time::now() WHERE record::id(id) = $conversation_id AND user_id = $user_id; COMMIT TRANSACTION;",
    ).bind(json!({"id": id.to_string(), "at": at.format(&time::format_description::well_known::Rfc3339).map_err(|_| ())?, "conversation_id": conv_id.to_string(),
        "user_id": user_id.to_string(), "role": role, "content": content, "model": model})).await;
    res.map_err(|_| ())?.check().map_err(|_| ())?;
    Ok(CanonicalMessage {
        id,
        conversation_id: conv_id,
        role: if role == "user" {
            MessageRole::User
        } else {
            MessageRole::Assistant
        },
        content: content.to_string(),
        model: model.map(str::to_string),
        created_at: at,
    })
}

#[derive(Deserialize)]
struct ConversationRow {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    title: String,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

/// List the owner's conversations, newest-active first (ADR-030).
pub(crate) async fn list_conversations(
    authed: Authed,
    State(state): State<AppState>,
) -> Json<Value> {
    let rows: Vec<ConversationRow> = match state.db.query(
        "SELECT record::id(id) AS id, title, updated_at FROM conversations WHERE user_id = $user_id \
         ORDER BY updated_at DESC LIMIT 100",
    ).bind(json!({"user_id": authed.user.id.to_string()})).await { Ok(mut response) => response.take(0).unwrap_or_default(), Err(error) => { tracing::warn!(%error, "failed to list conversations"); Vec::new() } };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| json!({ "id": row.id, "title": row.title, "updated_at": row.updated_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() }))
        .collect();
    Json(json!({ "conversations": items }))
}

/// A single conversation's messages, in order (ADR-030).
pub(crate) async fn get_conversation(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let title = conversation_title(&state.db, id, authed.user.id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "no such conversation" })),
            )
        })?;
    #[derive(Deserialize)]
    struct MessageRow {
        id: String,
        role: String,
        content: String,
        model: Option<String>,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
    }
    let mut response = state.db.query(
        "SELECT record::id(id) AS id, role, content, model, created_at FROM chat_messages WHERE conversation_id = $id AND user_id = $user ORDER BY created_at ASC",
    ).bind(json!({"id": id.to_string(), "user":authed.user.id.to_string()})).await.map_err(|_| internal_error())?;
    let rows: Vec<MessageRow> = response.take(0).map_err(|_| internal_error())?;
    let messages: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({ "id":row.id, "role": row.role, "content": row.content, "model": row.model, "at": row.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect();
    Ok(Json(
        json!({ "id": id, "title": title, "messages": messages,
            "assistant_running": state.realtime.run_active(authed.user.id, id) }),
    ))
}

/// Set or clear this conversation's owner-selected default. The actual router
/// still checks the root-owned allowlist at execution time, so a later policy
/// revocation turns this into Auto instead of granting stale access.
pub(crate) async fn conversation_brain_set(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<BrainPreferenceReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_brain_selection(&state, req.provider.as_deref(), req.model.as_deref())?;
    let mut response = state.db.query(
        "UPDATE conversations SET brain_provider = $provider, brain_model = $model, updated_at = time::now() \
         WHERE record::id(id) = $id AND user_id = $user_id RETURN record::id(id) AS id",
    ).bind(json!({"id":id.to_string(), "user_id":authed.user.id.to_string(), "provider":req.provider, "model":req.model})).await
        .map_err(|_| internal_error())?;
    let updated: Option<Value> = response.take(0).map_err(|_| internal_error())?;
    if updated.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"no such conversation"})),
        ));
    }
    if let Some(title) = conversation_title(&state.db, id, authed.user.id).await {
        state.realtime.publish(
            authed.user.id,
            Event::ConversationUpdated(ConversationMetadata {
                id,
                title,
                updated_at: OffsetDateTime::now_utc(),
            }),
        );
    }
    Ok(Json(json!({"status":"updated"})))
}

/// Delete a conversation and its messages (ON DELETE CASCADE) — owner-only.
pub(crate) async fn delete_conversation(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _guard = state.realtime.reserve_run(authed.user.id, id).ok_or((
        StatusCode::CONFLICT,
        Json(json!({"error":"conversation has an active run"})),
    ))?;
    let mut response = state.db.query(
        "BEGIN TRANSACTION; DELETE chat_messages WHERE conversation_id = $id AND user_id = $user_id; \
         DELETE conversations WHERE record::id(id) = $id AND user_id = $user_id RETURN $id AS id; COMMIT TRANSACTION;",
    ).bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string()})).await.map_err(|_| internal_error())?;
    let deleted: Option<Value> = response.take(1).map_err(|_| internal_error())?;
    if deleted.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such conversation" })),
        ));
    }
    state.realtime.publish(
        authed.user.id,
        Event::ConversationDeleted {
            conversation_id: id,
        },
    );
    Ok(Json(json!({ "status": "deleted" })))
}

#[derive(Deserialize)]
pub(crate) struct OrchestrateReq {
    /// The task to plan and carry out.
    task: String,
}

/// Plan→execute a task (ADR-028 fase 3): a strong model plans, cheap models run
/// the steps, a synthesis composes + checks. Pure reasoning — no tools/actions.
/// Every underlying call is billed against the budget (ADR-027).
pub(crate) async fn assistant_orchestrate(
    _authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<OrchestrateReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let task = req.task.trim();
    if task.is_empty() {
        return Err(bad_request("task is required"));
    }
    if task.len() > validation::MAX_TASK_LEN {
        return Err(bad_request("task too long"));
    }
    match orchestrator::plan_and_execute(&state.llm, task, &state.jarvis_system).await {
        Ok(run) => {
            for reply in &run.calls {
                record_usage(&state, reply).await;
            }
            let steps: Vec<Value> = run
                .steps
                .iter()
                .map(|s| json!({ "step": s.step, "output": s.output, "model": s.model }))
                .collect();
            Ok(Json(json!({
                "plan": run.plan,
                "steps": steps,
                "answer": run.answer,
            })))
        }
        Err(llm::LlmError::Refused) => Ok(Json(json!({
            "answer": "Sorry, daar kan ik niet op antwoorden.",
            "plan": Value::Array(vec![]),
            "steps": Value::Array(vec![]),
        }))),
        Err(e) => {
            tracing::warn!(error = %e, "orchestration failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "controleer je brein-config (router/keys/Ollama)",
                })),
            ))
        }
    }
}
