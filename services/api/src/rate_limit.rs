//! In-process, per-IP rate limiting for auth-sensitive endpoints (Priority 2 of
//! the security hardening brief).
//!
//! Single-node only: the counters live in memory, so they reset on restart and
//! do not coordinate across processes. That is sufficient for the current
//! single-node Home Node deployment (`docs/AI_HOME_NODE_GUI_PLAN.md`); a shared
//! store (e.g. Redis) would only be needed if the API is ever run as multiple
//! horizontally-scaled instances — deliberately avoided for now.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::AppState;

/// Tunable limits for the auth endpoints (configurable via `JARVIS_AUTH_*`).
#[derive(Clone, Copy, Debug)]
pub struct AuthLimits {
    pub enroll_per_min: u32,
    pub challenge_per_min: u32,
    pub login_per_min: u32,
    pub login_max_failures: u32,
    pub login_lock_secs: u64,
}

impl Default for AuthLimits {
    fn default() -> Self {
        Self {
            enroll_per_min: 10,
            challenge_per_min: 30,
            login_per_min: 20,
            login_max_failures: 5,
            login_lock_secs: 300,
        }
    }
}

/// Fixed-window request counter keyed by an arbitrary string (e.g. `"path:ip"`).
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, (Instant, u32)>>,
    /// Consecutive *failed* attempts per key, for failure-based lockout — kept
    /// separate from `hits` so a successful call can clear a caller's penalty.
    failures: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one hit for `key`; return `true` while it stays within `max` hits
    /// per `window`, `false` once the caller has exceeded the limit.
    pub fn check(&self, key: &str, max: u32, window: Duration) -> bool {
        let now = Instant::now();
        // A poisoned lock is not a security event; recover the guard and carry on.
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        // Opportunistic prune so the map cannot grow without bound over long
        // uptimes (otherwise bounded only by the number of distinct clients).
        if map.len() > 4096 {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0); // window elapsed → start a fresh count
        }
        entry.1 += 1;
        entry.1 <= max
    }

    /// Record one failed attempt for `key` within `window`; return the running
    /// failure count. Used for failure-based lockout (e.g. bad login signatures).
    pub fn note_failure(&self, key: &str, window: Duration) -> u32 {
        let now = Instant::now();
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1
    }

    /// How many failures `key` has accrued in the current `window` (0 if none or
    /// the window has elapsed).
    pub fn failures_in_window(&self, key: &str, window: Duration) -> u32 {
        let now = Instant::now();
        let map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(key) {
            Some((start, n)) if now.duration_since(*start) < window => *n,
            _ => 0,
        }
    }

    /// Clear a caller's failure penalty (e.g. after a successful login).
    pub fn clear_failures(&self, key: &str) {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(key);
    }
}

// ---- HTTP middleware --------------------------------------------------------

/// The connection peer address (injected by `into_make_service_with_connect_info`);
/// falls back to a constant when absent (e.g. in-process test requests).
fn peer_ip(req: &Request) -> String {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "local".to_string())
}

/// The client IP used to key rate limiting, login lockout and audit attribution.
///
/// Trust model (Priority 3): a client-supplied `X-Forwarded-For` is **never**
/// trusted by default (`trusted_hops == 0`) — the socket peer address is used, so
/// a forged header cannot bypass IP limits. Only when the operator declares that
/// the API sits behind exactly `trusted_hops` trusted proxies (and is reachable
/// *only* through them) do we read the client from `X-Forwarded-For`, taking the
/// entry the innermost trusted proxy appended (`len - trusted_hops`) and ignoring
/// the spoofable prefix. A malformed/short header falls back to the peer address.
pub(crate) fn client_ip(req: &Request, trusted_hops: u32) -> String {
    if trusted_hops == 0 {
        return peer_ip(req);
    }
    let hops = trusted_hops as usize;
    if let Some(xff) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        let ips: Vec<&str> = xff
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if ips.len() >= hops {
            return ips[ips.len() - hops].to_string();
        }
    }
    peer_ip(req)
}

/// The shared 429 response for any auth throttle.
fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": "rate limited",
            "hint": "te veel pogingen; probeer het straks opnieuw",
        })),
    )
        .into_response()
}

/// Rate limiting for auth endpoints. A flat per-endpoint limit throttles all
/// traffic; login additionally has a failure-based lockout so repeated *bad*
/// signatures lock the IP without penalising a successful login. Runs before the
/// handler (and before body parsing). Over any limit ⇒ `429`.
pub(crate) async fn rate_limit_mw(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let limits = state.auth_limits;
    let window = std::time::Duration::from_secs(60);
    let lock_window = std::time::Duration::from_secs(limits.login_lock_secs);
    let path = req.uri().path().to_string();
    let ip = client_ip(&req, state.trusted_proxy_hops);

    // Flat per-endpoint rate limit.
    let flat = match path.as_str() {
        "/v1/auth/enroll" => Some(limits.enroll_per_min),
        "/v1/auth/challenge" => Some(limits.challenge_per_min),
        "/v1/auth/login" => Some(limits.login_per_min),
        _ => None,
    };
    if let Some(max) = flat {
        if !state.rate_limiter.check(&format!("{path}:{ip}"), max, window) {
            tracing::warn!(%ip, %path, "auth rate limit hit");
            return too_many_requests();
        }
    }

    // Login-specific failure lockout — repeated bad signatures lock the IP.
    let is_login = path == "/v1/auth/login";
    let fail_key = format!("loginfail:{ip}");
    if is_login
        && state.rate_limiter.failures_in_window(&fail_key, lock_window)
            >= limits.login_max_failures
    {
        tracing::warn!(%ip, "login locked out after repeated failures");
        return too_many_requests();
    }

    let resp = next.run(req).await;

    // Count genuine auth failures (401); a success wipes the penalty.
    if is_login {
        if resp.status() == StatusCode::UNAUTHORIZED {
            let n = state.rate_limiter.note_failure(&fail_key, lock_window);
            tracing::warn!(%ip, failures = n, "failed login attempt");
        } else if resp.status().is_success() {
            state.rate_limiter.clear_failures(&fail_key);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    #[test]
    fn client_ip_ignores_forwarded_header_without_trusted_proxy() {
        let req = Request::builder()
            .header("x-forwarded-for", "6.6.6.6")
            .body(Body::empty())
            .unwrap();
        // hops=0 → never trust the header; no ConnectInfo peer → "local".
        assert_eq!(client_ip(&req, 0), "local");
    }

    #[test]
    fn client_ip_trusts_only_the_innermost_proxy_hop() {
        // The client spoofs "9.9.9.9"; the single trusted proxy appended the peer.
        let req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9, 203.0.113.7")
            .body(Body::empty())
            .unwrap();
        assert_eq!(client_ip(&req, 1), "203.0.113.7");
    }

    #[test]
    fn client_ip_skips_trusted_hops_from_the_right() {
        let req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.9")
            .body(Body::empty())
            .unwrap();
        // 2 trusted hops → the client sits at len-2, spoofable prefix ignored.
        assert_eq!(client_ip(&req, 2), "203.0.113.7");
    }

    #[test]
    fn client_ip_falls_back_when_header_too_short() {
        let req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        // hops=2 but only one entry → fall back to the peer ("local").
        assert_eq!(client_ip(&req, 2), "local");
    }

    #[test]
    fn allows_up_to_max_then_blocks() {
        let rl = RateLimiter::new();
        let w = Duration::from_secs(60);
        assert!(rl.check("k", 3, w));
        assert!(rl.check("k", 3, w));
        assert!(rl.check("k", 3, w));
        assert!(!rl.check("k", 3, w)); // 4th over the limit of 3
        // A different key has its own independent budget.
        assert!(rl.check("other", 3, w));
    }

    #[test]
    fn window_reset_allows_again() {
        let rl = RateLimiter::new();
        let w = Duration::from_millis(40);
        assert!(rl.check("k", 1, w));
        assert!(!rl.check("k", 1, w)); // 2nd within the window is blocked
        std::thread::sleep(Duration::from_millis(55));
        assert!(rl.check("k", 1, w)); // window elapsed → allowed again
    }

    #[test]
    fn failures_accumulate_and_clear() {
        let rl = RateLimiter::new();
        let w = Duration::from_secs(60);
        assert_eq!(rl.failures_in_window("ip", w), 0);
        assert_eq!(rl.note_failure("ip", w), 1);
        assert_eq!(rl.note_failure("ip", w), 2);
        assert_eq!(rl.failures_in_window("ip", w), 2);
        rl.clear_failures("ip"); // a success wipes the penalty
        assert_eq!(rl.failures_in_window("ip", w), 0);
    }
}
