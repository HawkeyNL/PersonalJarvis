use crate::{AppState, Authed};
use axum::{extract::State, http::StatusCode, Json};
use jarvis_client_core::realtime::Event;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) async fn owner(auth: Authed, State(state): State<AppState>) -> Json<Value> {
    let owner = state.realtime.voice_owner(auth.user.id);
    Json(json!({"device_id":owner.map(|v|v.0), "run_id":owner.and_then(|v|v.1)}))
}

pub(crate) async fn claim(
    auth: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    if !crate::rate_limit::allow_authenticated_device(&state, auth.device.id, "voice-control", 30) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if !state
        .realtime
        .claim_voice(auth.user.id, auth.device.id, None)
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Ok(Json(json!({"device_id":auth.device.id})))
}

pub(crate) async fn release(
    auth: Authed,
    State(state): State<AppState>,
) -> Result<Json<Value>, StatusCode> {
    if !crate::rate_limit::allow_authenticated_device(&state, auth.device.id, "voice-control", 30) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if !state.realtime.release_voice(auth.user.id, auth.device.id) {
        return Err(StatusCode::CONFLICT);
    }
    Ok(Json(json!({"device_id":null})))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlaybackState {
    Started,
    Stopped,
    Failed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Playback {
    run_id: Uuid,
    state: PlaybackState,
}

pub(crate) async fn playback(
    auth: Authed,
    State(state): State<AppState>,
    Json(req): Json<Playback>,
) -> Result<Json<Value>, StatusCode> {
    if !crate::rate_limit::allow_authenticated_device(&state, auth.device.id, "voice-playback", 120)
    {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    if state.realtime.voice_owner(auth.user.id) != Some((auth.device.id, Some(req.run_id))) {
        return Err(StatusCode::CONFLICT);
    }
    let event = match req.state {
        PlaybackState::Started => Event::VoiceStarted {
            run_id: req.run_id,
            device_id: auth.device.id,
        },
        PlaybackState::Stopped => Event::VoiceStopped {
            run_id: req.run_id,
            device_id: auth.device.id,
        },
        PlaybackState::Failed => Event::VoiceFailed {
            run_id: req.run_id,
            device_id: auth.device.id,
        },
    };
    state.realtime.publish(auth.user.id, event);
    Ok(Json(json!({"status":"ok"})))
}
