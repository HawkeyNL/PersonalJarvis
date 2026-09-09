use super::hub::PlaybackState;
use crate::{AppState, Authed};
use axum::{extract::State, http::StatusCode, Json};
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
    if !state
        .realtime
        .report_playback(auth.user.id, auth.device.id, req.run_id, req.state)
    {
        return Err(StatusCode::CONFLICT);
    }
    Ok(Json(json!({"status":"ok"})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_request_cannot_supply_an_authoritative_device_or_freeform_status() {
        let valid = json!({"run_id":Uuid::nil(), "state":"started"});
        assert!(serde_json::from_value::<Playback>(valid.clone()).is_ok());
        let mut spoof = valid.clone();
        spoof["device_id"] = json!(Uuid::nil());
        assert!(serde_json::from_value::<Playback>(spoof).is_err());
        let mut arbitrary = valid;
        arbitrary["state"] = json!("arbitrary output or command");
        assert!(serde_json::from_value::<Playback>(arbitrary).is_err());
    }
}
