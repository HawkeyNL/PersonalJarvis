//! Shared HTTP error/response helpers. Every error the API returns to a client
//! is deliberately opaque — a fixed shape (`{"error": ...}`) and a status code,
//! with the real cause going to logs/traces only. This keeps internal details
//! (DB errors, identity internals) from leaking across the trust boundary.

use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

use jarvis_identity as identity;
use jarvis_portfolio as portfolio;
use jarvis_speech as speech;

pub(crate) fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized" })),
    )
}

pub(crate) fn internal(_e: identity::IdentityError) -> (StatusCode, Json<Value>) {
    // Errors are deliberately opaque to clients; details go to logs/traces.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

pub(crate) fn bad_request(message: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message })))
}

pub(crate) fn portfolio_err(_e: portfolio::PortfolioError) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

pub(crate) fn speech_err(e: speech::SpeechError) -> (StatusCode, Json<Value>) {
    match e {
        speech::SpeechError::TooShort => bad_request("audio was empty or too short"),
        speech::SpeechError::NotConfigured(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "speech engine not configured" })),
        ),
        speech::SpeechError::Failed(_) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "speech engine failed" })),
        ),
    }
}
