//! In-process, per-IP rate limiting for auth-sensitive endpoints (Priority 2 of
//! the security hardening brief).
//!
//! Single-node only: the counters live in memory, so they reset on restart and
//! do not coordinate across processes. That is sufficient for the current
//! single-node Home Node deployment (`docs/AI_HOME_NODE_GUI_PLAN.md`); a shared
//! store (e.g. Redis) would only be needed if the API is ever run as multiple
//! horizontally-scaled instances — deliberately avoided for now.

use std::collections::HashMap;
use std::net::IpAddr;
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
    pub authenticated_per_min: u32,
    pub llm_per_min: u32,
}

impl Default for AuthLimits {
    fn default() -> Self {
        Self {
            enroll_per_min: 10,
            challenge_per_min: 30,
            login_per_min: 20,
            login_max_failures: 5,
            login_lock_secs: 300,
            authenticated_per_min: 300,
            llm_per_min: 20,
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

const MAX_TRACKED_KEYS: usize = 4096;

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
        if map.len() >= MAX_TRACKED_KEYS {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
            if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(key) {
                // Fail closed instead of allowing an attacker to grow the
                // in-process limiter without bound via unique source keys.
                return false;
            }
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
        if map.len() >= MAX_TRACKED_KEYS {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
            if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(key) {
                return 0;
            }
        }
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
fn peer_ip(req: &Request) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
}

/// The client IP used to key rate limiting, login lockout and audit attribution.
///
/// Trust model (Priority 3): a client-supplied `X-Forwarded-For` is **never**
/// trusted by default. We use a forwarding header only when the connection peer
/// is explicitly allowlisted as a trusted proxy and the operator configured the
/// exact proxy-hop count. The proxy must overwrite incoming forwarding headers;
/// otherwise no application can distinguish a client-supplied value. A malformed
/// or short header falls back to the direct peer address.
pub(crate) fn client_ip(req: &Request, trusted_hops: u32, trusted_peers: &[IpAddr]) -> String {
    let peer = peer_ip(req);
    let peer_is_trusted = peer.is_some_and(|ip| trusted_peers.contains(&ip));
    if trusted_hops == 0 || !peer_is_trusted {
        return peer.map_or_else(|| "local".to_string(), |ip| ip.to_string());
    }
    let hops = trusted_hops as usize;
    if let Some(xff) = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    {
        let ips: Option<Vec<IpAddr>> = xff
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok();
        if let Some(ips) = ips.filter(|ips| ips.len() >= hops) {
            return ips[ips.len() - hops].to_string();
        }
    }
    peer.map_or_else(|| "local".to_string(), |ip| ip.to_string())
}

/// The shared 429 response for any auth throttle.
pub(crate) fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, "60")],
        Json(json!({
            "error": "rate limited",
            "hint": "te veel pogingen; probeer het straks opnieuw",
        })),
    )
        .into_response()
}

/// Rate-limit an authenticated device. IP limits still protect anonymous
/// traffic; this independent key prevents a single valid device from evading
/// cost and workload controls by changing networks.
pub(crate) fn allow_authenticated_device(
    state: &AppState,
    device_id: uuid::Uuid,
    profile: &str,
    per_min: u32,
) -> bool {
    state.rate_limiter.check(
        &format!("{profile}:device:{device_id}"),
        per_min,
        Duration::from_secs(60),
    )
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
    let ip = client_ip(
        &req,
        state.trusted_proxy_hops,
        state.trusted_proxy_ips.as_slice(),
    );

    // Flat per-endpoint rate limit.
    let flat = match path.as_str() {
        "/v1/auth/enroll" => Some(limits.enroll_per_min),
        "/v1/auth/challenge" => Some(limits.challenge_per_min),
        "/v1/auth/login" => Some(limits.login_per_min),
        _ => None,
    };
    if let Some(max) = flat {
        if !state
            .rate_limiter
            .check(&format!("{path}:{ip}"), max, window)
        {
            tracing::warn!(%ip, %path, "auth rate limit hit");
            return too_many_requests();
        }
    }

    // Login-specific failure lockout — repeated bad signatures lock the IP.
    let is_login = path == "/v1/auth/login";
    let fail_key = format!("loginfail:{ip}");
    if is_login
        && state
            .rate_limiter
            .failures_in_window(&fail_key, lock_window)
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
        assert_eq!(client_ip(&req, 0, &[]), "local");
    }

    #[test]
    fn client_ip_ignores_header_from_an_untrusted_peer() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9, 203.0.113.7")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "198.51.100.8:443".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let trusted: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(client_ip(&req, 1, &[trusted]), "198.51.100.8");
    }

    #[test]
    fn client_ip_trusts_only_an_allowlisted_proxy_peer() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.9")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "10.0.0.9:443".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let trusted: IpAddr = "10.0.0.9".parse().unwrap();
        // 2 trusted hops → use the entry before the trusted proxy chain.
        assert_eq!(client_ip(&req, 2, &[trusted]), "203.0.113.7");
    }

    #[test]
    fn client_ip_falls_back_when_header_too_short() {
        let req = Request::builder()
            .header("x-forwarded-for", "9.9.9.9")
            .body(Body::empty())
            .unwrap();
        // hops=2 but only one entry → fall back to the peer ("local").
        assert_eq!(client_ip(&req, 2, &[]), "local");
    }

    #[test]
    fn client_ip_falls_back_when_header_is_malformed() {
        let mut req = Request::builder()
            .header("x-forwarded-for", "not-an-ip")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(
            "10.0.0.9:443".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let trusted: IpAddr = "10.0.0.9".parse().unwrap();
        assert_eq!(client_ip(&req, 1, &[trusted]), "10.0.0.9");
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
