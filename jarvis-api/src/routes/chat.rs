//! Chat with the brain + conversation persistence (ADR-030) and multi-step
//! orchestration (ADR-028). The persona is prepended server-side and the API key
//! is never exposed. A topic shift auto-splits into a new conversation; every
//! metered LLM call is billed against the budget via [`record_usage`]. Persistence
//! is best-effort — it must never break the reply.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_llm as llm;
use jarvis_orchestrator as orchestrator;

use crate::error::bad_request;
use crate::metering::record_usage;
use crate::validation;
use crate::{AppState, Authed};

#[derive(Deserialize)]
struct ChatTurn {
    role: String,
    content: String,
}

#[derive(Deserialize)]
pub(crate) struct ChatReq {
    messages: Vec<ChatTurn>,
    /// Optional tier hint: `default` | `hard` | `cheap`.
    #[serde(default)]
    tier: Option<String>,
    /// Optional system-prompt override (defaults to the Jarvis persona).
    #[serde(default)]
    system: Option<String>,
    /// The conversation this turn belongs to (ADR-030). Absent ⇒ start a fresh
    /// one; present ⇒ append, unless the topic shifted (then Jarvis splits it off
    /// into a new conversation and returns the new id).
    #[serde(default)]
    conversation_id: Option<Uuid>,
}

/// Chat with the brain (protected). Persists the turn under a conversation and
/// auto-splits a new topic into its own conversation (ADR-030). The persona is
/// prepended server-side; the API key is never exposed.
pub(crate) async fn assistant_chat(
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    let (conv_id, conv_title, new_topic) = match existing {
        Some((cid, title)) => {
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
        None => {
            let (_same, proposed) = classify_topic(&state, None, &new_msg).await;
            let id = create_conversation(&state.db, authed.user.id, &proposed)
                .await
                .map_err(|_| internal_error())?;
            (id, proposed, true)
        }
    };

    // Save what the owner said up front, so it survives even a brain outage.
    append_message(&state.db, conv_id, authed.user.id, "user", &new_msg, None).await;

    // A fresh topic starts with a clean slate; a continuation keeps its context.
    let messages = if new_topic {
        vec![llm::ChatMessage::user(&new_msg)]
    } else {
        history
    };
    let chat = llm::ChatRequest {
        system: Some(
            req.system
                .unwrap_or_else(|| state.jarvis_system.to_string()),
        ),
        tier: req
            .tier
            .as_deref()
            .map(llm::Tier::parse)
            .unwrap_or_default(),
        messages,
        max_tokens: state.llm_max_tokens,
        // The router picks the concrete model per backend (ADR-028 fase 2).
        model: None,
    };

    match state.llm.chat(&chat).await {
        Ok(reply) => {
            record_usage(&state, &reply).await;
            append_message(
                &state.db,
                conv_id,
                authed.user.id,
                "assistant",
                &reply.text,
                Some(reply.model.as_str()),
            )
            .await;
            Ok(Json(json!({
                "reply": reply.text,
                "model": reply.model,
                "stop_reason": reply.stop_reason,
                "conversation_id": conv_id,
                "conversation_title": conv_title,
                "new_topic": new_topic,
            })))
        }
        Err(llm::LlmError::Refused) => {
            let text = "Sorry, daar kan ik niet op antwoorden.";
            append_message(&state.db, conv_id, authed.user.id, "assistant", text, None).await;
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
            tracing::warn!(error = %e, "assistant chat failed");
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

/// Ask a cheap model whether `new_msg` continues the current topic, and get a
/// short title for the (new) topic. Best-effort: any failure keeps the current
/// conversation (or, with none, derives a title) so chat never breaks (ADR-030).
async fn classify_topic(
    state: &AppState,
    current_title: Option<&str>,
    new_msg: &str,
) -> (bool, String) {
    let snippet: String = new_msg.chars().take(600).collect();
    let system = "Je bepaalt of een nieuw bericht bij het lopende gespreksonderwerp \
         hoort of een nieuw onderwerp begint. Antwoord UITSLUITEND met JSON: \
         {\"same_topic\": true of false, \"title\": \"korte titel, max 5 woorden\"}. \
         Is er geen lopend onderwerp, dan is same_topic altijd false.";
    let user = format!(
        "Lopend onderwerp: \"{}\"\nNieuw bericht: \"{}\"",
        current_title.unwrap_or("(geen)"),
        snippet
    );
    let req = llm::ChatRequest {
        system: Some(system.to_string()),
        tier: llm::Tier::Cheap,
        messages: vec![llm::ChatMessage::user(&user)],
        max_tokens: 60,
        model: None,
    };
    match state.llm.chat(&req).await {
        Ok(reply) => {
            record_usage(state, &reply).await;
            parse_topic(&reply.text)
                .unwrap_or_else(|| (current_title.is_some(), derive_title(new_msg)))
        }
        // Brain down for the classifier: don't fragment — keep the current
        // conversation if there is one, else start one with a derived title.
        Err(_) => (current_title.is_some(), derive_title(new_msg)),
    }
}

/// Extract `{same_topic, title}` from a model reply (tolerant of prose around
/// the JSON). Returns None if no usable object is found.
fn parse_topic(text: &str) -> Option<(bool, String)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let same = v
        .get("same_topic")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .map(clean_title)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Nieuw gesprek".to_string());
    Some((same, title))
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

async fn conversation_title(
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
    .map_err(|_| ())?;
    Ok(id)
}

/// Append a message and bump the conversation's `updated_at`. Best-effort:
/// persistence must never break the reply, so a failure is logged, not surfaced.
async fn append_message(
    db: &jarvis_store::Database,
    conv_id: Uuid,
    user_id: Uuid,
    role: &str,
    content: &str,
    model: Option<&str>,
) {
    let res = db.query(
        "BEGIN TRANSACTION; CREATE chat_messages SET id = $id, conversation_id = $conversation_id, \
         user_id = $user_id, role = $role, content = $content, model = $model, created_at = time::now(); \
         UPDATE conversations SET updated_at = time::now() WHERE record::id(id) = $conversation_id AND user_id = $user_id; COMMIT TRANSACTION;",
    ).bind(json!({"id": Uuid::now_v7().to_string(), "conversation_id": conv_id.to_string(),
        "user_id": user_id.to_string(), "role": role, "content": content, "model": model})).await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "failed to persist chat message");
    }
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
        role: String,
        content: String,
        model: Option<String>,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
    }
    let mut response = state.db.query(
        "SELECT role, content, model, created_at FROM chat_messages WHERE conversation_id = $id ORDER BY created_at ASC",
    ).bind(json!({"id": id.to_string()})).await.map_err(|_| internal_error())?;
    let rows: Vec<MessageRow> = response.take(0).map_err(|_| internal_error())?;
    let messages: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({ "role": row.role, "content": row.content, "model": row.model, "at": row.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect();
    Ok(Json(
        json!({ "id": id, "title": title, "messages": messages }),
    ))
}

/// Delete a conversation and its messages (ON DELETE CASCADE) — owner-only.
pub(crate) async fn delete_conversation(
    authed: Authed,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut response = state.db.query(
        "BEGIN TRANSACTION; DELETE chat_messages WHERE conversation_id = $id AND user_id = $user_id; \
         DELETE conversations WHERE record::id(id) = $id AND user_id = $user_id RETURN record::id(id) AS id; COMMIT TRANSACTION;",
    ).bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string()})).await.map_err(|_| internal_error())?;
    let deleted: Option<ConversationRow> = response.take(1).map_err(|_| internal_error())?;
    if deleted.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such conversation" })),
        ));
    }
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
