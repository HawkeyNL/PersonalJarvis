use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{StatusCode, Uri},
    response::Response,
};
use tokio::time::{timeout, Instant};

use super::Subscription;

pub(crate) async fn capability(
    _: Authed,
) -> axum::Json<jarvis_client_core::realtime::RealtimeCapability> {
    axum::Json(jarvis_client_core::realtime::RealtimeCapability {
        protocol: jarvis_client_core::realtime::REALTIME_PROTOCOL,
        transport: "websocket".into(),
        reconciliation: "rest".into(),
        asynchronous_chat: true,
    })
}
use crate::{AppState, Authed};

pub(crate) async fn connect(
    auth: Authed,
    State(state): State<AppState>,
    uri: Uri,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    // No URL tickets, bearer parameters or caller-selected identity/cursor.
    if uri.query().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !crate::rate_limit::allow_authenticated_device(
        &state,
        auth.device.id,
        "realtime-connect",
        30,
    ) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let subscription = state
        .realtime
        .subscribe(auth.user.id, auth.device.id)
        .ok_or(StatusCode::TOO_MANY_REQUESTS)?;
    Ok(upgrade
        .max_frame_size(1024)
        .max_message_size(1024)
        .on_upgrade(move |socket| serve(socket, subscription, state, auth)))
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    matches!(
        timeout(Duration::from_secs(5), socket.send(message)).await,
        Ok(Ok(()))
    )
}

async fn serve(
    mut socket: WebSocket,
    mut subscription: Subscription,
    state: AppState,
    auth: Authed,
) {
    let mut heartbeat = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_pong = Instant::now();
    let mut last_ping = Instant::now() - Duration::from_secs(30);
    loop {
        tokio::select! {
            event = subscription.receiver.recv() => {
                let Some(event) = event else { break };
                let Ok(text) = serde_json::to_string(event.as_ref()) else { break };
                if !send(&mut socket, Message::Text(text.into())).await { break; }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Pong(_))) => {
                        // Only one expected heartbeat response per interval.
                        if last_pong.elapsed() < Duration::from_secs(10) { break; }
                        last_pong = Instant::now();
                        state.realtime.renew_voice(auth.user.id, auth.device.id);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if last_ping.elapsed() < Duration::from_secs(10) { break; }
                        last_ping = Instant::now();
                        if !send(&mut socket, Message::Pong(payload)).await { break; }
                        // Native clients should answer server heartbeats, not
                        // flood unsolicited messages. Commands belong in HTTP.
                    }
                    _ => break,
                }
            }
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(75) { break; }
                let valid = timeout(Duration::from_secs(5), jarvis_identity::session_is_active(
                    &state.db, auth.session_id, auth.user.id, auth.device.id,
                )).await;
                if !matches!(valid, Ok(Ok(true))) { break; }
                state.realtime.voice_owner(auth.user.id);
                if !send(&mut socket, Message::Ping(Vec::new().into())).await { break; }
            }
        }
    }
    let _ = timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}
