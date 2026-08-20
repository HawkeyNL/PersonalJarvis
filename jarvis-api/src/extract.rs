//! The `Authed` request extractor — the single gate every protected handler
//! passes through. It resolves a `Bearer` session token to the owner + device +
//! session, rejecting anything unauthenticated with an opaque 401. This is the
//! real trust boundary: voice/verification is convenience only, never this.

use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    Json,
};
use serde_json::Value;
use uuid::Uuid;

use jarvis_identity as identity;

use crate::error::unauthorized;
use crate::AppState;

/// Authenticated principal, extracted from a `Bearer` session token.
pub struct Authed {
    pub user: identity::User,
    pub device: identity::Device,
    pub session_id: Uuid,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or_else(unauthorized)?;
        let auth = identity::authenticate(&state.db, token)
            .await
            .map_err(|_| unauthorized())?;
        Ok(Authed {
            user: auth.user,
            device: auth.device,
            session_id: auth.session_id,
        })
    }
}
