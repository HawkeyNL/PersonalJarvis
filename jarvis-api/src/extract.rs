//! The `Authed` request extractor — the single gate every protected handler
//! passes through. It resolves a `Bearer` session token to the owner + device +
//! session, rejecting anything unauthenticated with an opaque 401. This is the
//! real trust boundary: voice/verification is convenience only, never this.

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{header::AUTHORIZATION, request::Parts, StatusCode},
    Json,
};
use serde_json::Value;
use uuid::Uuid;

use jarvis_identity as identity;

use crate::error::unauthorized;
use crate::rate_limit::allow_authenticated_device;
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
        if state.require_https && !arrived_via_trusted_https_proxy(parts, state) {
            return Err(unauthorized());
        }
        let auth = identity::authenticate(&state.db, token)
            .await
            .map_err(|_| unauthorized())?;
        if !allow_authenticated_device(
            state,
            auth.device.id,
            "authenticated",
            state.auth_limits.authenticated_per_min,
        ) {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "rate limited",
                    "hint": "te veel pogingen; probeer het straks opnieuw",
                })),
            ));
        }
        Ok(Authed {
            user: auth.user,
            device: auth.device,
            session_id: auth.session_id,
        })
    }
}

/// In production TLS ends at Caddy. Only a direct peer explicitly configured
/// as trusted may assert that the original request used HTTPS. A client cannot
/// make a direct loopback request look encrypted by sending this header.
fn arrived_via_trusted_https_proxy(parts: &Parts, state: &AppState) -> bool {
    let peer = parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|peer| peer.0.ip());
    peer.is_some_and(|ip| state.trusted_proxy_ips.contains(&ip))
        && state.trusted_proxy_hops > 0
        && parts
            .headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}
