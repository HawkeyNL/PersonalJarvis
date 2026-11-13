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
    CanonicalMessage, ConversationMetadata, Event, MessageRole, RunFailure, RunIdentity,
};
use jarvis_llm as llm;
use jarvis_orchestrator as orchestrator;

use crate::error::bad_request;
use crate::metering::{record_failure, record_usage, record_usage_with_metadata};
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
    /// Absent starts a fresh conversation; present continues the owned one.
    #[serde(default)]
    pub(super) conversation_id: Option<Uuid>,
    /// The caller's uploaded, not yet sent photos this message carries. Only
    /// the runs API accepts them. Absent when empty, so the request hash of
    /// a text-only message is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) attachments: Vec<Uuid>,
}

impl ChatReq {
    /// The new user turn's text; empty only when it carries photos.
    pub(super) fn new_user_content(&self) -> Option<&str> {
        self.messages
            .last()
            .filter(|m| m.role == "user")
            .map(|m| m.content.trim())
            .filter(|text| !text.is_empty() || !self.attachments.is_empty())
    }
    pub(super) fn validate_run(&self) -> Result<String, (StatusCode, Json<Value>)> {
        if self.messages.len() > validation::MAX_CHAT_TURNS
            || self
                .messages
                .iter()
                .any(|m| m.content.len() > validation::MAX_CHAT_CONTENT_LEN)
            || self.messages.iter().map(|m| m.content.len()).sum::<usize>() > 128_000
        {
            return Err(bad_request("chat payload too large"));
        }
        if self.attachments.len() > crate::photos::MAX_ATTACHMENTS {
            return Err(bad_request("too many photos"));
        }
        if (1..self.attachments.len()).any(|i| self.attachments[..i].contains(&self.attachments[i]))
        {
            return Err(bad_request("duplicate photo"));
        }
        // Photos never go to web research.
        if !self.attachments.is_empty()
            && self.mode.as_deref().map(llm::RoutingMode::parse) == Some(llm::RoutingMode::Research)
        {
            return Err(bad_request("photos cannot be sent to web research"));
        }
        let text = self
            .new_user_content()
            .ok_or_else(|| bad_request("last message must be a nonempty user message"))?;
        Ok(if text.is_empty() {
            "Photo".to_string()
        } else {
            derive_title(text)
        })
    }
}

/// Synchronous response compatibility for old clients. Its single provider
/// execution still publishes the same live events to newer connected clients.
pub(crate) async fn assistant_chat(
    authed: Authed,
    State(state): State<AppState>,
    Json(mut req): Json<ChatReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !req.attachments.is_empty() {
        return Err(bad_request("photos need the runs API"));
    }
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
                "hint": "too many attempts; try again later",
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
    let photo_turn = !req.attachments.is_empty();
    if history.is_empty() && !photo_turn {
        return Err(bad_request("messages is required"));
    }
    // The last user turn is the new message to store and (maybe) reclassify.
    let new_msg = req
        .messages
        .iter()
        .rev()
        .find(|t| !matches!(t.role.as_str(), "assistant" | "jarvis"))
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty() || photo_turn)
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
        None => append_message(
            &state.db,
            conv_id,
            authed.user.id,
            "user",
            &new_msg,
            None,
            &[],
        )
        .await
        .map_err(|_| internal_error())?,
    };
    // Only the runs API carries photos: their labels for this run.
    let (canonical_history, photo_labels) = if user_already_published {
        // A web search never sees photos, so none are read for one.
        let research =
            req.mode.as_deref().map(llm::RoutingMode::parse) == Some(llm::RoutingMode::Research);
        let (messages, labels) = super::chat_context::load(
            &state.db,
            &state.photos,
            authed.user.id,
            &user_message,
            !research,
        )
        .await
        .map_err(|_| internal_error())?;
        (Some(messages), labels)
    } else {
        (None, Vec::new())
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
        state.realtime.publish(
            authed.user.id,
            Event::AssistantStarted {
                run: run.clone(),
                web_search: false,
            },
        );
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
    // System-1 classifies a bounded copy of the latest user turn. The owner-selected
    // mode and deterministic safety floor retain final authority. A classifier result
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
        // Set below for every non-research turn: always the server persona
        // plus live runtime facts; clients cannot replace it.
        system: None,
        tier: requirements.tier,
        mode,
        messages,
        max_tokens: state.llm_max_tokens,
        // The router picks the concrete model per backend (ADR-028 fase 2).
        model: None,
        provider: None,
    };
    // Explicit Deep/Research requests retain their quality-floor semantics.
    // The owner default is only applied to ordinary Auto conversation turns;
    // every persisted selection was allowlist-validated and is checked again
    // here to fail closed after a policy reload/revocation.
    if mode == llm::RoutingMode::Auto {
        if let Some((provider, model)) = owner_brain {
            // Never reroute a pinned paid brain silently while paid APIs are off.
            if state.model_routing.snapshot().refuses_provider(&provider) {
                let (status, Json(mut body)) = crate::routes::system::paid_api_off();
                body["conversation_id"] = json!(conv_id);
                return Err((status, Json(body)));
            }
            if state.model_policy.allows(&provider, &model) {
                // The pin names a provider too: a claude-cli failure must
                // never become a call to the same model on a paid API.
                chat.model = Some(model);
                chat.provider = Some(provider);
            }
        }
    }

    // Built only on the paths that send it, after the early refusals.
    if requested_mode != llm::RoutingMode::Research {
        let mut system = super::runtime_facts::system_prompt(&state, user_already_published).await;
        if !photo_labels.is_empty() {
            add_photo_sentence(&mut system);
        }
        chat.system = Some(system);
    }
    // Photos in the model input, new or from recent turns: no web search
    // follows and none is offered.
    let image_turn = photo_turn || chat.has_images();

    let llm_started = Instant::now();
    let request_id = realtime_run
        .as_ref()
        .map(|run| run.run_id.to_string())
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    // Only an explicit owner Research request may search the web; a
    // classifier's Research advice stays an ordinary hard-tier answer. The
    // research path receives the latest question alone: no persona, memory,
    // history or device data.
    let reply = if requested_mode == llm::RoutingMode::Research {
        Ok(research_reply(
            &state.model_routing.snapshot(),
            state.llm.as_ref(),
            &new_msg,
        )
        .await)
    } else if let Some(run) = &realtime_run {
        let hub = state.realtime.clone();
        let run = run.clone();
        let user = authed.user.id;
        let hold = std::sync::Mutex::new(crate::auto_research::MarkerHold::default());
        state
            .llm
            .chat_stream(
                &chat,
                std::sync::Arc::new(move |text| {
                    let Ok(mut hold) = hold.lock() else {
                        return;
                    };
                    let ready = hold.push(text).to_owned();
                    drop(hold);
                    if ready.is_empty() {
                        return;
                    }
                    hub.publish(
                        user,
                        Event::AssistantDelta {
                            run: run.clone(),
                            text: ready,
                        },
                    );
                }),
            )
            .await
    } else {
        state.llm.chat(&chat).await
    };
    let metadata = jarvis_usage::UsageMetadata {
        request_id,
        routing_mode: format!("{mode:?}").to_ascii_lowercase(),
        quality_tier: format!("{:?}", requirements.tier).to_ascii_lowercase(),
        latency_ms: llm_started.elapsed().as_millis().min(i64::MAX as u128) as i64,
        ..Default::default()
    };
    if let Err(error) = &reply {
        let pinned = chat.provider.as_deref().zip(chat.model.as_deref());
        record_failure(&state, pinned, error, metadata.clone()).await;
    }
    match reply {
        Ok(mut reply) => {
            let research_advised = research_advised(&mut reply.text, requested_mode, intent_kind);
            // A web answer never shows a photo; a chat answer only one of
            // this run's photos.
            let labels: &[(u8, Uuid)] = if requested_mode == llm::RoutingMode::Research {
                &[]
            } else {
                &photo_labels
            };
            let had_markers = reply.text.contains(PHOTO_MARKER);
            let shown = take_photo_markers(&mut reply.text, labels);
            if had_markers && reply.text.trim().is_empty() && shown.is_empty() {
                reply.text = PHOTO_NOT_FOUND_REPLY.into();
            }
            let input_bytes = chat
                .messages
                .iter()
                .map(|m| {
                    let images = m.images.iter().map(|image| image.bytes().len() / 4);
                    m.content
                        .len()
                        .saturating_add(32)
                        .saturating_add(images.sum())
                })
                .sum::<usize>()
                .saturating_add(chat.system.as_ref().map_or(0, String::len));
            let status = estimate_missing_usage(&mut reply, input_bytes, chat.max_tokens);
            record_usage_with_metadata(
                &state,
                &reply,
                jarvis_usage::UsageMetadata {
                    status: status.into(),
                    ..metadata
                },
            )
            .await;
            let message = append_message(
                &state.db,
                conv_id,
                authed.user.id,
                "assistant",
                &reply.text,
                Some(stored_model(requested_mode, &reply).as_str()),
                &shown,
            )
            .await
            .map_err(|_| internal_error())?;
            if let Some(run) = &realtime_run {
                // Only a runs-API run (its user message was persisted first)
                // may search by itself; the legacy endpoint never does. A
                // turn with photos never leads to a web search.
                let auto = if user_already_published && !image_turn {
                    crate::auto_research::start(&state, research_advised, &new_msg).await
                } else {
                    None
                };
                let research_offer = if auto.is_some() || image_turn {
                    None
                } else {
                    crate::intent::research_offer(
                        research_advised.then_some(crate::intent::WorkKind::Research),
                        || state.llm.can_research(),
                        &new_msg,
                    )
                };
                state.realtime.publish(
                    authed.user.id,
                    Event::AssistantCompleted {
                        run: run.clone(),
                        message,
                        research_offer,
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
                if let Some(slot) = auto {
                    tokio::spawn(auto_research_run(
                        state.clone(),
                        slot,
                        authed.user.id,
                        run.clone(),
                        new_msg.clone(),
                    ));
                }
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
        // A refusal, or photos with no subscription worker to read them: a
        // fixed answer is stored; the owner's photo stays with the message.
        Err(error)
            if matches!(error, llm::LlmError::Refused)
                || (chat.has_images() && matches!(error, llm::LlmError::NotConfigured(_))) =>
        {
            let (text, stop_reason) = if matches!(error, llm::LlmError::Refused) {
                ("Sorry, I can't answer that.", "refusal")
            } else {
                (NO_VISION_REPLY, "vision_unavailable")
            };
            let message = append_message(
                &state.db,
                conv_id,
                authed.user.id,
                "assistant",
                text,
                None,
                &[],
            )
            .await
            .map_err(|_| internal_error())?;
            if let Some(run) = &realtime_run {
                state.realtime.publish(
                    authed.user.id,
                    Event::AssistantCompleted {
                        run: run.clone(),
                        message,
                        research_offer: None,
                    },
                );
            }
            Ok(Json(json!({
                "reply": text,
                "model": Value::Null,
                "stop_reason": stop_reason,
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
                    "hint": "check JARVIS_LLM_API_KEY or start Ollama locally",
                    "conversation_id": conv_id,
                })),
            ))
        }
    }
}

/// Stored when photos were in the input but neither subscription worker
/// could take them. No other backend is tried.
const NO_VISION_REPLY: &str = "I can't look at photos right now: photos only go to \
your Claude or Codex subscription worker, and neither is available. Your photo is kept \
with your message.";
/// Stored when a reply was nothing but photo markers that matched no photo.
const PHOTO_NOT_FOUND_REPLY: &str = "I can't show that photo.";
const PHOTO_SENTENCE: &str = "The owner attached photos labelled [photo N]. To show one \
of them back, put a line that is exactly [[photo:N]].";

/// Tell the model how to show a photo back, if the worker's system cap
/// leaves room; the labels in the turns stand on their own otherwise.
fn add_photo_sentence(system: &mut String) {
    if system.len() + 2 + PHOTO_SENTENCE.len() <= llm::claude_worker_protocol::MAX_SYSTEM_BYTES {
        system.push_str("\n\n");
        system.push_str(PHOTO_SENTENCE);
    }
}

/// Take every `[[photo:N]]` off a reply, so none is ever stored or shown.
/// A line that is exactly one marker naming a label of this run shows that
/// photo (each once, at most four); any other marker, inline or with an
/// unknown number, is only removed.
fn take_photo_markers(text: &mut String, labels: &[(u8, Uuid)]) -> Vec<Uuid> {
    if !text.contains(PHOTO_MARKER) {
        return Vec::new();
    }
    let mut shown = Vec::new();
    let mut kept = Vec::new();
    for line in text.lines() {
        match photo_marker(line.trim()) {
            Some((number, "")) => {
                let label = labels.iter().find(|(label, _)| u32::from(*label) == number);
                if let Some((_, id)) = label {
                    if !shown.contains(id) && shown.len() < crate::photos::MAX_ATTACHMENTS {
                        shown.push(*id);
                    }
                }
            }
            _ => kept.push(without_inline_markers(line)),
        }
    }
    *text = kept.join("\n").trim_matches('\n').to_string();
    shown
}

const PHOTO_MARKER: &str = "[[photo:";

/// `[[photo:N]]` at the start of `text`: N (1 to 3 digits) and the rest.
fn photo_marker(text: &str) -> Option<(u32, &str)> {
    let rest = text.strip_prefix(PHOTO_MARKER)?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let rest_after = rest[digits..].strip_prefix("]]")?;
    (1..=3)
        .contains(&digits)
        .then(|| rest[..digits].parse().ok().map(|n| (n, rest_after)))
        .flatten()
}

fn without_inline_markers(line: &str) -> String {
    // Removing one marker can join the text around it into a new one.
    let mut line = remove_markers_once(line);
    while photo_marker_inside(&line) {
        line = remove_markers_once(&line);
    }
    line
}

fn photo_marker_inside(text: &str) -> bool {
    text.match_indices(PHOTO_MARKER)
        .any(|(start, _)| photo_marker(&text[start..]).is_some())
}

fn remove_markers_once(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(PHOTO_MARKER) {
        out.push_str(&rest[..start]);
        match photo_marker(&rest[start..]) {
            Some((_, after)) => rest = after,
            None => {
                out.push_str(PHOTO_MARKER);
                rest = &rest[start + PHOTO_MARKER.len()..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Stored when a reply was nothing but the web search marker.
const MARKER_ONLY_REPLY: &str = "I can't answer this reliably without a web search.";

/// Whether a web search is advised after this reply: the classifier said
/// research, or an ordinary answer ended with the web search marker. The
/// marker is taken off `reply` in every case, whatever the routing, so it is
/// never stored, published or shown.
fn research_advised(
    reply: &mut String,
    requested_mode: llm::RoutingMode,
    intent_kind: Option<crate::intent::WorkKind>,
) -> bool {
    let taken = crate::auto_research::take_marker(reply);
    if taken && reply.is_empty() {
        *reply = MARKER_ONLY_REPLY.into();
    }
    let marked = taken && requested_mode != llm::RoutingMode::Research;
    intent_kind == Some(crate::intent::WorkKind::Research) || marked
}

const RESEARCH_OFF: &str = "Research is off. The owner can turn on web search with \
`sudo jarvis models route research-web-search on` or in Core Admin (Model routing).";
const RESEARCH_UNAVAILABLE: &str = "Research is unavailable right now: no enabled \
subscription (claude-cli or codex-cli) could run the question. No paid API was used.";
const RESEARCH_STOPPED: &str = "The web search was stopped to protect your privacy: \
the run tried a search or tool that is not allowed, and its answer was discarded. \
Rephrase the question without personal details and ask again.";
const RESEARCH_PRIVATE: &str = "Research did not run: the question may contain \
personal details, such as an email address, a phone, account or card number, a key, \
or your own portfolio or account. Rephrase it without personal details and ask again.";
const RESEARCH_EMPTY: &str = "The web search returned no usable answer. \
Ask again or rephrase the question.";
/// Longest research answer, in characters, that is stored.
const MAX_RESEARCH_ANSWER_CHARS: usize = 12_000;

/// The model a reply is stored under. A completed Research run (a notice has
/// no backend) is `web:<model>`, so later context carries it as untrusted
/// data; metering keeps the plain model.
fn stored_model(requested_mode: llm::RoutingMode, reply: &llm::ChatReply) -> String {
    if requested_mode == llm::RoutingMode::Research && reply.backend.is_some() {
        super::chat_context::web_model(&reply.model)
    } else {
        reply.model.clone()
    }
}

/// Web-derived text before it is stored: markdown images become their alt
/// text, so no client loads a remote URL by itself; the trailing Sources list
/// keeps only https URLs with a host; the whole answer is capped.
fn clean_research_text(text: &str) -> String {
    let text = https_sources_only(&images_as_text(text));
    match text.char_indices().nth(MAX_RESEARCH_ANSWER_CHARS) {
        Some((end, _)) => text[..end].to_string(),
        None => text,
    }
}

fn images_as_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let image = after.find("](").and_then(|alt_end| {
            let alt = &after[..alt_end];
            let target = &after[alt_end + 2..];
            let close = target.find(')')?;
            (!alt.contains(['[', ']', '\n']) && !target[..close].contains('\n'))
                .then_some((alt, alt_end + 2 + close + 1))
        });
        match image {
            Some((alt, len)) => {
                out.push_str(alt);
                rest = &after[len..];
            }
            None => {
                out.push_str("![");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // Any other image syntax (reference style) becomes an ordinary link.
    out.replace("![", "[")
}

fn https_sources_only(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(header) = lines.iter().rposition(|line| {
        let label = line.trim().trim_matches(['#', '*', '_', ':', ' ']);
        label.eq_ignore_ascii_case("sources") || label.eq_ignore_ascii_case("bronnen")
    }) else {
        return text.to_string();
    };
    let mut kept = lines[..=header].to_vec();
    kept.extend(
        lines[header + 1..]
            .iter()
            .copied()
            .filter(|line| is_https_source(line)),
    );
    kept.join("\n")
}

/// A Sources line that is one https URL with a host and no user info,
/// bare or as `[Title](url)`, optionally as a `-` or `*` list item.
fn is_https_source(line: &str) -> bool {
    let candidate = line.trim().trim_start_matches(['-', '*']).trim_start();
    let candidate = titled_link_url(candidate).unwrap_or(candidate);
    !candidate.contains(char::is_whitespace)
        && url::Url::parse(candidate).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
        })
}

/// The URL of a whole `[Title](url)` line. The title is plain text the apps
/// show as is: 1 to 120 characters, no brackets, parentheses, line breaks,
/// control, bidi or other invisible format characters.
fn titled_link_url(candidate: &str) -> Option<&str> {
    let (title, rest) = candidate.strip_prefix('[')?.split_once("](")?;
    let plain = (1..=120).contains(&title.chars().count())
        && crate::validation::no_hidden_chars(title)
        && !title.contains(['[', ']', '(', ')']);
    plain.then(|| rest.strip_suffix(')')).flatten()
}

/// Missing usage on a backend reply must not turn a metered success into an
/// unrecorded/free request: charge a conservative input-byte ceiling plus
/// framing and the requested output-token limit. Returns the usage status.
fn estimate_missing_usage(
    reply: &mut llm::ChatReply,
    input_bytes: usize,
    max_tokens: u32,
) -> &'static str {
    if reply.backend.is_none() || reply.usage.is_some() {
        return "succeeded";
    }
    reply.usage = Some(llm::Usage {
        input_tokens: input_bytes.saturating_add(1024).min(i32::MAX as usize) as u32,
        output_tokens: max_tokens,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    });
    "succeeded_estimated_usage"
}

/// Automatic web search after an answer: a second run on the same question
/// with no user message of its own, marked `web_search` when it starts. A
/// search that did not run (switched off meanwhile, worker slot busy, no
/// subscription) fails this run and stores nothing; the answer stands and
/// the owner can still ask for Research.
async fn auto_research_run(
    state: AppState,
    slot: crate::auto_research::AutoSlot,
    user: Uuid,
    answered: RunIdentity,
    question: String,
) {
    let _slot = slot;
    let run = RunIdentity {
        run_id: Uuid::now_v7(),
        ..answered
    };
    state.realtime.publish(
        user,
        Event::AssistantStarted {
            run: run.clone(),
            web_search: true,
        },
    );
    let started = Instant::now();
    let Some((mut reply, model)) = auto_research_answer(
        &state.model_routing.snapshot(),
        state.llm.as_ref(),
        &question,
    )
    .await
    else {
        state.realtime.publish(
            user,
            Event::AssistantFailed {
                run,
                reason: RunFailure::ProviderUnavailable,
            },
        );
        return;
    };
    let status = estimate_missing_usage(&mut reply, question.len(), state.llm_max_tokens);
    record_usage_with_metadata(
        &state,
        &reply,
        jarvis_usage::UsageMetadata {
            request_id: run.run_id.to_string(),
            routing_mode: "research_auto".into(),
            quality_tier: "hard".into(),
            latency_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            status: status.into(),
            ..Default::default()
        },
    )
    .await;
    // The owner may have deleted the conversation while the search ran.
    match append_to_live_conversation(&state.db, run.conversation_id, user, &reply.text, &model)
        .await
    {
        Ok(Some((message, title))) => {
            state.realtime.publish(
                user,
                Event::AssistantCompleted {
                    run: run.clone(),
                    message,
                    research_offer: None,
                },
            );
            state.realtime.publish(
                user,
                Event::ConversationUpdated(ConversationMetadata {
                    id: run.conversation_id,
                    title,
                    updated_at: OffsetDateTime::now_utc(),
                }),
            );
        }
        Ok(None) => state.realtime.publish(
            user,
            Event::AssistantFailed {
                run,
                reason: RunFailure::Interrupted,
            },
        ),
        Err(()) => state.realtime.publish(
            user,
            Event::AssistantFailed {
                run,
                reason: RunFailure::PersistenceUnavailable,
            },
        ),
    }
}

/// The automatic search's answer and the model it is stored under
/// (`web:<model>`), or `None` when no search ran: then only a fixed notice
/// would remain, which an unrequested run does not store.
async fn auto_research_answer(
    routing: &llm::RoutingSnapshot,
    brain: &dyn llm::LlmProvider,
    question: &str,
) -> Option<(llm::ChatReply, String)> {
    let mut reply = research_reply(routing, brain, question).await;
    // A web page may inject a trailing marker; strip it like any answer.
    research_advised(&mut reply.text, llm::RoutingMode::Research, None);
    take_photo_markers(&mut reply.text, &[]);
    if reply.text.trim().is_empty() {
        reply.text = RESEARCH_EMPTY.into();
    }
    reply.backend.is_some().then(|| {
        let model = stored_model(llm::RoutingMode::Research, &reply);
        (reply, model)
    })
}

/// A scheduled `prompt` job's search (docs/SCHEDULED_JOBS.md): the
/// automatic search's path within `limit`, metered as `job` after it. The
/// answer and the model it is stored under (`web:<model>`), `Ok(None)` when
/// no search ran, `Err` when it took too long.
pub(crate) async fn job_research(
    state: &AppState,
    request_id: Uuid,
    question: &str,
    limit: std::time::Duration,
) -> Result<Option<(String, String)>, tokio::time::error::Elapsed> {
    let started = Instant::now();
    let answer = tokio::time::timeout(
        limit,
        auto_research_answer(
            &state.model_routing.snapshot(),
            state.llm.as_ref(),
            question,
        ),
    )
    .await?;
    let Some((mut reply, model)) = answer else {
        return Ok(None);
    };
    let status = estimate_missing_usage(&mut reply, question.len(), state.llm_max_tokens);
    record_usage_with_metadata(
        state,
        &reply,
        jarvis_usage::UsageMetadata {
            request_id: request_id.to_string(),
            routing_mode: "job".into(),
            quality_tier: "hard".into(),
            latency_ms: started.elapsed().as_millis().min(i64::MAX as u128) as i64,
            status: status.into(),
            ..Default::default()
        },
    )
    .await;
    Ok(Some((reply.text, model)))
}

/// Post an assistant message to a job's conversation, made anew (titled
/// `title`) when it has none or the owner deleted it, and tell the apps.
/// Returns the conversation and message ids. Each database call is bounded
/// by the jobs' database timeout.
pub(crate) async fn post_job_message(
    state: &AppState,
    user: Uuid,
    conversation: Option<Uuid>,
    title: &str,
    content: &str,
    model: &str,
) -> Result<(Uuid, Uuid), ()> {
    let limit = crate::jobs::DB_TIMEOUT;
    let append = |id: Uuid| async move {
        tokio::time::timeout(
            limit,
            append_to_live_conversation(&state.db, id, user, content, model),
        )
        .await
        .map_err(|_| ())?
        .map(|found| found.map(|found| (id, found)))
    };
    let mut posted = match conversation {
        Some(id) => append(id).await?,
        None => None,
    };
    if posted.is_none() {
        let title = clean_title(title);
        let id = tokio::time::timeout(limit, create_conversation(&state.db, user, &title))
            .await
            .map_err(|_| ())??;
        posted = append(id).await?;
    }
    let (id, (message, title)) = posted.ok_or(())?;
    state.realtime.publish(
        user,
        Event::ConversationUpdated(ConversationMetadata {
            id,
            title,
            updated_at: OffsetDateTime::now_utc(),
        }),
    );
    Ok((id, message.id))
}

/// The answer to an explicit Research request. Off, too long, carrying a
/// personal identifier ([`llm::privacy::leaks_identifier`]) or unavailable is
/// a clear, fixed answer, never a silent paid-API or non-search answer. There
/// is no override. An automatic run passed the wider `is_private` first.
async fn research_reply(
    routing: &llm::RoutingSnapshot,
    brain: &dyn llm::LlmProvider,
    latest_question: &str,
) -> llm::ChatReply {
    let notice = |text: String| llm::ChatReply {
        text,
        model: "research".into(),
        backend: None,
        requested_route: None,
        actual_provider: None,
        fallback_count: 0,
        stop_reason: Some("research_unavailable".into()),
        usage: None,
    };
    if !routing.research_allowed() {
        return notice(RESEARCH_OFF.into());
    }
    let Some(request) = llm::ResearchRequest::new(latest_question) else {
        return notice(format!(
            "Your research question is too long: at most {} characters. Ask a shorter question.",
            llm::MAX_RESEARCH_QUESTION_CHARS
        ));
    };
    if llm::privacy::leaks_identifier(request.question()) {
        return notice(RESEARCH_PRIVATE.into());
    }
    match brain.research(&request).await {
        // The run happened: its model and usage stay for metering, only an
        // answer that the cleanup emptied becomes a fixed text.
        Ok(reply) => {
            let text = clean_research_text(&reply.text);
            llm::ChatReply {
                text: if text.trim().is_empty() {
                    RESEARCH_EMPTY.into()
                } else {
                    text
                },
                ..reply
            }
        }
        // A worker stopped the run: a tool or search query it may not run.
        Err(error) if error.usage_failure_category() == "tool_use_refused" => {
            tracing::warn!("research stopped by the worker");
            notice(RESEARCH_STOPPED.into())
        }
        Err(error) => {
            tracing::warn!(failure = ?error.failure_category(), "research unavailable");
            notice(RESEARCH_UNAVAILABLE.into())
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
        "New conversation".to_string()
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
    attachments: &[Uuid],
) -> Result<CanonicalMessage, ()> {
    let id = Uuid::now_v7();
    let at = OffsetDateTime::now_utc();
    // Without photos the statement is exactly the old one.
    let attachments_field = if attachments.is_empty() {
        ""
    } else {
        ", attachments = $attachments"
    };
    let res = db.query(format!(
        "BEGIN TRANSACTION; CREATE chat_messages SET id = $id, conversation_id = $conversation_id, \
         user_id = $user_id, role = $role, content = $content, model = $model, created_at = <datetime>$at{attachments_field}; \
         UPDATE conversations SET updated_at = time::now() WHERE record::id(id) = $conversation_id AND user_id = $user_id; COMMIT TRANSACTION;",
    )).bind(json!({"id": id.to_string(), "at": at.format(&time::format_description::well_known::Rfc3339).map_err(|_| ())?, "conversation_id": conv_id.to_string(),
        "user_id": user_id.to_string(), "role": role, "content": content, "model": model,
        "attachments": attachments.iter().map(Uuid::to_string).collect::<Vec<_>>()})).await;
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
        attachments: attachments.to_vec(),
    })
}

/// Appends an assistant message only while the owner's conversation still
/// exists. One transaction writes the conversation row first, so a concurrent
/// delete either commits before it (nothing is stored) or conflicts. Returns
/// the message and the conversation's current title, or `None` when the
/// conversation is gone.
async fn append_to_live_conversation(
    db: &jarvis_store::Database,
    conv_id: Uuid,
    user_id: Uuid,
    content: &str,
    model: &str,
) -> Result<Option<(CanonicalMessage, String)>, ()> {
    let id = Uuid::now_v7();
    let at = OffsetDateTime::now_utc();
    let mut response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $conversation = UPDATE conversations SET updated_at = time::now() \
             WHERE record::id(id) = $conversation_id AND user_id = $user_id RETURN AFTER; \
             IF array::len($conversation) = 1 { CREATE chat_messages SET id = $id, \
             conversation_id = $conversation_id, user_id = $user_id, role = 'assistant', \
             content = $content, model = $model, created_at = <datetime>$at RETURN NONE; }; \
             SELECT VALUE title FROM conversations \
             WHERE record::id(id) = $conversation_id AND user_id = $user_id; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({"id": id.to_string(),
            "at": at.format(&time::format_description::well_known::Rfc3339).map_err(|_| ())?,
            "conversation_id": conv_id.to_string(), "user_id": user_id.to_string(),
            "content": content, "model": model}))
        .await
        .map_err(|_| ())?
        .check()
        .map_err(|_| ())?;
    let last = response.num_statements().checked_sub(1).ok_or(())?;
    let titles: Vec<String> = response.take(last).map_err(|_| ())?;
    Ok(titles.into_iter().next().map(|title| {
        let message = CanonicalMessage {
            id,
            conversation_id: conv_id,
            role: MessageRole::Assistant,
            content: content.to_string(),
            model: Some(model.to_string()),
            created_at: at,
            attachments: Vec::new(),
        };
        (message, title)
    }))
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
        #[serde(default)]
        attachments: Option<Vec<String>>,
    }
    #[derive(Deserialize)]
    struct PhotoRow {
        id: String,
        saved: bool,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
    }
    let mut response = state.db.query(
        "SELECT record::id(id) AS id, role, content, model, created_at, attachments FROM chat_messages WHERE conversation_id = $id AND user_id = $user ORDER BY created_at ASC; \
         SELECT record::id(id) AS id, saved, created_at FROM chat_photos WHERE conversation_id = $id AND user_id = $user ORDER BY created_at ASC",
    ).bind(json!({"id": id.to_string(), "user":authed.user.id.to_string()})).await.map_err(|_| internal_error())?;
    let rows: Vec<MessageRow> = response.take(0).map_err(|_| internal_error())?;
    let photos: Vec<PhotoRow> = response.take(1).map_err(|_| internal_error())?;
    let messages: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({ "id":row.id, "role": row.role, "content": row.content, "model": row.model, "at": row.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "attachments": row.attachments.unwrap_or_default() })
        })
        .collect();
    // The conversation's photos that still exist; a message id missing here
    // is a deleted or expired photo.
    let photos: Vec<Value> = photos
        .into_iter()
        .map(|photo| {
            json!({ "id": photo.id, "saved": photo.saved,
                "expires_at": crate::photos::expires_at(photo.saved, false, photo.created_at) })
        })
        .collect();
    Ok(Json(
        json!({ "id": id, "title": title, "messages": messages, "photos": photos,
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

/// Delete a conversation with its messages and its photos, kept ones too —
/// owner-only. Photo files are removed after the commit; the orphan sweep is
/// the backstop.
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
         SELECT VALUE record::id(id) FROM chat_photos WHERE conversation_id = $id AND user_id = $user_id; \
         DELETE chat_photos WHERE conversation_id = $id AND user_id = $user_id; \
         DELETE conversations WHERE record::id(id) = $id AND user_id = $user_id RETURN $id AS id; COMMIT TRANSACTION;",
    ).bind(json!({"id": id.to_string(), "user_id": authed.user.id.to_string()})).await.map_err(|_| internal_error())?;
    let photos: Vec<String> = response.take(1).map_err(|_| internal_error())?;
    let deleted: Option<Value> = response.take(3).map_err(|_| internal_error())?;
    if deleted.is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such conversation" })),
        ));
    }
    let photos = photos
        .iter()
        .filter_map(|photo| Uuid::try_parse(photo).ok())
        .collect();
    crate::photos::remove_files(&state.photos, photos).await;
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
    authed: Authed,
    State(state): State<AppState>,
    Json(req): Json<OrchestrateReq>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // One orchestration fans out into several model calls: it shares the
    // device's LLM budget with chat instead of bypassing it.
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
                "hint": "too many attempts; try again later",
            })),
        ));
    }
    let task = req.task.trim();
    if task.is_empty() {
        return Err(bad_request("task is required"));
    }
    if task.len() > validation::MAX_TASK_LEN {
        return Err(bad_request("task too long"));
    }
    let system = super::runtime_facts::system_prompt(&state, false).await;
    let result = orchestrator::plan_and_execute(&state.llm, task, &system).await;
    if let Err(error) = &result {
        record_failure(&state, None, error, Default::default()).await;
    }
    match result {
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
            "answer": "Sorry, I can't answer that.",
            "plan": Value::Array(vec![]),
            "steps": Value::Array(vec![]),
        }))),
        Err(e) => {
            tracing::warn!(error = %e, "orchestration failed");
            Err((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "brain unavailable",
                    "hint": "check your brain config (router/keys/Ollama)",
                })),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn photo_markers_show_only_this_runs_photos_and_never_stay_in_the_text() {
        let ids: Vec<Uuid> = (1..=5).map(Uuid::from_u128).collect();
        let labels: Vec<(u8, Uuid)> = (1..=4).map(|n| (n as u8, ids[n - 1])).collect();
        let take = |text: &str| {
            let mut text = text.to_string();
            let shown = take_photo_markers(&mut text, &labels);
            (text, shown)
        };
        assert_eq!(
            take("Here it is:\n[[photo:2]]\nA red square."),
            ("Here it is:\nA red square.".to_string(), vec![ids[1]])
        );
        // Unknown numbers, inline markers and near misses never attach.
        assert_eq!(
            take("[[photo:9]]\nSee [[photo:1]] above.\n  [[photo:3]]  \n[[photo:]]\n[[photo:1]] x"),
            ("See  above.\n[[photo:]]\n x".to_string(), vec![ids[2]])
        );
        // Each photo once, in reply order, at most four.
        let (text, shown) =
            take("[[photo:4]]\n[[photo:4]]\n[[photo:1]]\n[[photo:2]]\n[[photo:3]]\nDone.");
        assert_eq!(text, "Done.");
        assert_eq!(shown, [ids[3], ids[0], ids[1], ids[2]]);
        // Without labels (a web answer, or no photos) markers only vanish.
        let mut web = "Answer\n[[photo:1]]\nSources:".to_string();
        assert!(take_photo_markers(&mut web, &[]).is_empty());
        assert_eq!(web, "Answer\nSources:");
        // Text without markers is untouched, whitespace included.
        let mut plain = "  Line one\r\nLine two\n".to_string();
        assert!(take_photo_markers(&mut plain, &labels).is_empty());
        assert_eq!(plain, "  Line one\r\nLine two\n");
        assert_eq!(take("[[photo:0004]]").1, Vec::<Uuid>::new());
        // Removing a marker cannot leave a new one behind.
        assert_eq!(
            take("x [[pho[[photo:1]]to:2]] y [[[[photo:3]]photo:4]]]]"),
            ("x  y ]]".to_string(), Vec::new())
        );
    }

    #[test]
    fn the_photo_sentence_respects_the_worker_system_cap() {
        let max = llm::claude_worker_protocol::MAX_SYSTEM_BYTES;
        let mut system = "persona".to_string();
        add_photo_sentence(&mut system);
        assert!(system.ends_with(PHOTO_SENTENCE));
        let full = "p".repeat(max - PHOTO_SENTENCE.len() - 1);
        let mut unchanged = full.clone();
        add_photo_sentence(&mut unchanged);
        assert_eq!(unchanged, full);
        let mut fits = "p".repeat(max - PHOTO_SENTENCE.len() - 2);
        add_photo_sentence(&mut fits);
        assert_eq!(fits.len(), max);
    }

    /// Records what research received; ordinary chat must never be used.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<String>>,
        fail: bool,
        refused: bool,
        answer: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl llm::LlmProvider for Recorder {
        fn label(&self) -> &str {
            "recorder"
        }
        async fn chat(&self, _req: &llm::ChatRequest) -> Result<llm::ChatReply, llm::LlmError> {
            panic!("research must never fall back to an ordinary chat");
        }
        async fn research(
            &self,
            req: &llm::ResearchRequest,
        ) -> Result<llm::ChatReply, llm::LlmError> {
            self.seen.lock().unwrap().push(req.question().to_owned());
            if self.fail {
                return Err(llm::LlmError::NotConfigured("no subscription".into()));
            }
            if self.refused {
                return Err(llm::LlmError::Api {
                    status: 502,
                    body: "Claude run attempted tool use and was refused".into(),
                });
            }
            Ok(llm::ChatReply {
                text: self
                    .answer
                    .unwrap_or("Answer\nSources:\nhttps://example.org/a")
                    .into(),
                model: "claude-opus-5".into(),
                backend: Some("claude-cli".into()),
                requested_route: None,
                actual_provider: None,
                fallback_count: 0,
                stop_reason: Some("end_turn".into()),
                usage: None,
            })
        }
    }

    fn routing(raw: &str) -> llm::RoutingSnapshot {
        llm::RoutingSnapshot {
            routing: Some(llm::ModelRouting::parse(raw.as_bytes()).unwrap()),
            unavailable_reason: None,
        }
    }

    #[tokio::test]
    async fn research_off_is_a_clear_answer_and_calls_nothing() {
        let brain = Recorder::default();
        for snapshot in [
            llm::RoutingSnapshot::default(),
            routing(r#"{"version":1,"paid_api":"off"}"#),
            llm::RoutingSnapshot::unavailable("routing_invalid"),
        ] {
            let reply = research_reply(&snapshot, &brain, "nieuws?").await;
            assert_eq!(reply.text, RESEARCH_OFF);
            assert!(reply.backend.is_none());
        }
        assert!(brain.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn research_sends_only_the_latest_question() {
        let brain = Recorder::default();
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        let reply = research_reply(&on, &brain, "  Wat is de laatste Rust-release?  ").await;
        assert_eq!(reply.text, "Answer\nSources:\nhttps://example.org/a");
        assert_eq!(
            *brain.seen.lock().unwrap(),
            ["Wat is de laatste Rust-release?"]
        );
        // Overlong: refused with a clear answer, never cut or sent.
        let long = "x".repeat(llm::MAX_RESEARCH_QUESTION_CHARS + 1);
        let reply = research_reply(&on, &brain, &long).await;
        assert!(reply.text.contains("too long"));
        assert_eq!(brain.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn private_research_questions_are_refused_and_call_nothing() {
        let brain = Recorder::default();
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        for question in [
            "hoe doet mijn portfolio het?",
            "mail jan.devries@example.nl over het weer",
            "bel 06-12345678 terug",
            "NL91ABNA0417164300 rente?",
            "is sk-proj-a1b2c3d4e5f6g7h8i9j0k1l2 still valid?",
        ] {
            let reply = research_reply(&on, &brain, question).await;
            assert_eq!(reply.text, RESEARCH_PRIVATE, "{question}");
            assert!(reply.backend.is_none());
        }
        assert!(brain.seen.lock().unwrap().is_empty());
        // Amounts, dates and symbols are not identifiers: an explicit
        // search on them is the owner's choice.
        for question in [
            "ASML nieuws vandaag",
            "bitcoin price usd 2026",
            "nieuws 2026-11-10",
        ] {
            let reply = research_reply(&on, &brain, question).await;
            assert_eq!(reply.text, "Answer\nSources:\nhttps://example.org/a");
        }
        assert_eq!(brain.seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_run_the_worker_stopped_gets_the_privacy_notice() {
        let brain = Recorder {
            refused: true,
            ..Default::default()
        };
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        let reply = research_reply(&on, &brain, "nieuws?").await;
        assert_eq!(reply.text, RESEARCH_STOPPED);
        assert!(reply.backend.is_none());
        assert_ne!(RESEARCH_STOPPED, RESEARCH_UNAVAILABLE);
    }

    #[tokio::test]
    async fn research_without_a_subscription_says_so() {
        let brain = Recorder {
            fail: true,
            ..Default::default()
        };
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        let reply = research_reply(&on, &brain, "nieuws?").await;
        assert_eq!(reply.text, RESEARCH_UNAVAILABLE);
        assert!(reply.backend.is_none());
    }

    #[tokio::test]
    async fn research_answers_are_stored_as_web_and_notices_are_not() {
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        let research = llm::RoutingMode::Research;
        let answer = research_reply(&on, &Recorder::default(), "nieuws?").await;
        assert_eq!(
            answer.model, "claude-opus-5",
            "metering keeps the plain model"
        );
        assert_eq!(stored_model(research, &answer), "web:claude-opus-5");
        assert_eq!(
            stored_model(llm::RoutingMode::Auto, &answer),
            "claude-opus-5"
        );
        let off = research_reply(&llm::RoutingSnapshot::default(), &Recorder::default(), "x").await;
        assert_eq!(stored_model(research, &off), "research");
    }

    #[tokio::test]
    async fn automatic_search_stores_only_a_real_answer_as_web() {
        let auto = routing(r#"{"version":1,"research_web_search":"auto"}"#);
        let brain = Recorder::default();
        let (reply, model) = auto_research_answer(&auto, &brain, " weer Amsterdam morgen ")
            .await
            .unwrap();
        assert_eq!(model, "web:claude-opus-5");
        assert_eq!(reply.text, "Answer\nSources:\nhttps://example.org/a");
        assert_eq!(*brain.seen.lock().unwrap(), ["weer Amsterdam morgen"]);
        // Worker slot busy or no subscription: nothing is stored, the run fails.
        let busy = Recorder {
            fail: true,
            ..Default::default()
        };
        assert!(auto_research_answer(&auto, &busy, "weer?").await.is_none());
        assert_eq!(busy.seen.lock().unwrap().len(), 1);
        // Switched off meanwhile: no search at all.
        let off = Recorder::default();
        assert!(
            auto_research_answer(&llm::RoutingSnapshot::default(), &off, "weer?")
                .await
                .is_none()
        );
        assert!(off.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn research_answer_emptied_by_cleanup_is_a_fixed_text() {
        let brain = Recorder {
            answer: Some(" ![](https://x.example/t.gif) \n"),
            ..Default::default()
        };
        let on = routing(r#"{"version":1,"research_web_search":"on"}"#);
        let reply = research_reply(&on, &brain, "nieuws?").await;
        assert_eq!(reply.text, RESEARCH_EMPTY);
        assert_eq!(reply.model, "claude-opus-5");
    }

    #[test]
    fn a_trailing_marker_or_the_classifier_advises_research() {
        use crate::intent::WorkKind;
        use llm::RoutingMode::{Auto, Deep, Research};
        let advised = |text: &str, mode, kind| {
            let mut text = text.to_owned();
            (research_advised(&mut text, mode, kind), text)
        };
        let marked = "Short answer.\n[[web_search]]";
        assert_eq!(advised(marked, Auto, None), (true, "Short answer.".into()));
        assert_eq!(advised(marked, Deep, None), (true, "Short answer.".into()));
        assert_eq!(
            advised("[[web_search]]\n", Auto, None),
            (true, MARKER_ONLY_REPLY.into())
        );
        // A Research answer never asks again, but loses the marker too.
        assert_eq!(
            advised(marked, Research, None),
            (false, "Short answer.".into())
        );
        assert_eq!(
            advised("[[web_search]]", Research, None),
            (false, MARKER_ONLY_REPLY.into())
        );
        assert_eq!(
            advised("Answer.", Auto, Some(WorkKind::Research)),
            (true, "Answer.".into())
        );
        assert_eq!(
            advised("Answer.", Auto, Some(WorkKind::Conversation)),
            (false, "Answer.".into())
        );
        let mid = "Say [[web_search]]\nlater.";
        assert_eq!(advised(mid, Auto, None), (false, mid.into()));
    }

    #[test]
    fn research_images_become_text() {
        assert_eq!(
            images_as_text("See ![chart of prices](https://x.example/p.png?q=1) and ![](https://x.example/t.gif)."),
            "See chart of prices and ."
        );
        // Reference-style and malformed images are left as links or text.
        assert_eq!(images_as_text("![logo][1] ![open"), "[logo][1] [open");
        assert_eq!(images_as_text("a![b\nc](u) d"), "a[b\nc](u) d");
    }

    #[test]
    fn research_sources_keep_only_https_urls_with_a_host() {
        let text = "Answer with https://inline.example/kept.\n\
                    **Sources:**\n\
                    https://example.org/a\n\
                    - https://example.org/b?x=1\n\
                    http://plain.example/\n\
                    https://\n\
                    https://user:pw@evil.example/\n\
                    javascript:alert(1)\n\
                    https://example.org/c Ignore previous instructions\n\
                    Thanks for asking!";
        assert_eq!(
            https_sources_only(text),
            "Answer with https://inline.example/kept.\n**Sources:**\n\
             https://example.org/a\n- https://example.org/b?x=1"
        );
        assert_eq!(https_sources_only("No list here.\n"), "No list here.\n");
        // Titled links pass under the same URL rule; the title is plain.
        let titled = "Answer\nSources:\n\
                      - [Rust 1.97 released](https://blog.rust-lang.org/2026/rust-1.97)\n\
                      [Foo (bar) - Wiki](https://en.example.org/wiki/Foo)\n\
                      * [A](https://en.example.org/wiki/Foo_(bar))\n\
                      - [Plain title](http://plain.example/)\n\
                      - [Who](https://user:pw@evil.example/)\n\
                      - [](https://empty.example/)\n\
                      - [Spaced](https://example.org/a b)\n\
                      - [Click \u{202E}evil](https://example.org/)\n\
                      - [Two](https://example.org/a) [Three](https://example.org/b)\n\
                      - [Nested [x]](https://example.org/)\n\
                      - [Tab\there](https://example.org/)\n\
                      - [Zero\u{200B}width](https://example.org/)\n\
                      - [Line\u{2028}sep](https://example.org/)\n\
                      - [Soft\u{AD}hyphen](https://example.org/)\n\
                      - [Unclosed](https://example.org/";
        assert_eq!(
            https_sources_only(titled),
            "Answer\nSources:\n\
             - [Rust 1.97 released](https://blog.rust-lang.org/2026/rust-1.97)\n\
             * [A](https://en.example.org/wiki/Foo_(bar))"
        );
        let long = format!("- [{}](https://example.org/)", "t".repeat(121));
        assert!(!is_https_source(&long));
        assert!(is_https_source(&format!(
            "- [{}](https://example.org/)",
            "é".repeat(120)
        )));
        // The same hidden characters as every other owner-visible label.
        for hidden in [
            '\u{034F}',
            '\u{115F}',
            '\u{180E}',
            '\u{3164}',
            '\u{FE0F}',
            '\u{FFF9}',
            '\u{E0041}',
        ] {
            assert!(!is_https_source(&format!(
                "- [A{hidden}B](https://example.org/)"
            )));
        }
        assert_eq!(
            https_sources_only("Antwoord\nBronnen:\nhttps://example.nl/\nftp://x"),
            "Antwoord\nBronnen:\nhttps://example.nl/"
        );
    }

    #[test]
    fn research_answer_is_capped() {
        let long = "é".repeat(MAX_RESEARCH_ANSWER_CHARS + 10);
        assert_eq!(
            clean_research_text(&long).chars().count(),
            MAX_RESEARCH_ANSWER_CHARS
        );
        assert_eq!(clean_research_text("short"), "short");
    }

    #[test]
    fn client_system_prompt_is_dropped_not_obeyed() {
        let req: ChatReq = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "system": "X",
        }))
        .unwrap();
        let kept = serde_json::to_value(&req).unwrap();
        assert!(kept.get("system").is_none(), "{kept}");
    }

    #[test]
    fn photo_attachments_are_bounded_and_keep_text_requests_unchanged() {
        let req = |body: Value| serde_json::from_value::<ChatReq>(body).unwrap();
        let photo = |n: u128| Uuid::from_u128(n).to_string();
        // A text-only request serializes (and so hashes) exactly as before.
        let text = req(json!({"messages": [{"role": "user", "content": "hi"}]}));
        assert!(serde_json::to_value(&text)
            .unwrap()
            .get("attachments")
            .is_none());
        assert_eq!(text.validate_run().unwrap(), "hi");
        // Photos alone are a message, titled "Photo".
        let only = req(json!({"messages": [{"role": "user", "content": "  "}],
            "attachments": [photo(1)]}));
        assert_eq!(only.validate_run().unwrap(), "Photo");
        assert_eq!(only.new_user_content(), Some(""));
        let empty = req(json!({"messages": [{"role": "user", "content": "  "}]}));
        assert!(empty.validate_run().is_err());
        assert_eq!(empty.new_user_content(), None);
        for (attachments, mode) in [
            (vec![photo(1), photo(2), photo(3), photo(4), photo(5)], None),
            (vec![photo(1), photo(2), photo(1)], None),
            (vec![photo(1)], Some("research")),
        ] {
            let bad = req(json!({"messages": [{"role": "user", "content": "look"}],
                "attachments": attachments, "mode": mode}));
            assert!(bad.validate_run().is_err());
        }
        let four = req(json!({"messages": [{"role": "user", "content": "look"}],
            "attachments": [photo(1), photo(2), photo(3), photo(4)], "mode": "deep"}));
        assert_eq!(four.validate_run().unwrap(), "look");
        assert!(serde_json::from_value::<ChatReq>(json!({
            "messages": [{"role": "user", "content": "look"}],
            "attachments": ["../../etc/passwd"],
        }))
        .is_err());
    }
}
