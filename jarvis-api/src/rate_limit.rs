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
    extract::{Request, State},
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

type Windows = HashMap<String, (Instant, u32)>;

/// Fixed-window request counter keyed by an arbitrary string (e.g. `"path:ip"`).
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<Windows>,
    /// Keys anonymous traffic must never evict or crowd out: authenticated
    /// devices and global budgets. Bounded by the number of such keys.
    reserved: Mutex<Windows>,
    /// Consecutive *failed* attempts per key, for failure-based lockout — kept
    /// separate from `hits` so a successful call can clear a caller's penalty.
    failures: Mutex<Windows>,
}

const MAX_TRACKED_KEYS: usize = 4096;

/// Count one event for `key` and return the running count in its window.
/// A full table first drops expired windows, then the oldest one: refusing
/// new keys instead would let anyone with enough source addresses lock every
/// other caller out. Keys are per route class and source, so evicting only
/// hands an attacker what a fresh source address would already give them.
fn count(map: &Mutex<Windows>, key: &str, window: Duration) -> u32 {
    let now = Instant::now();
    // A poisoned lock is not a security event; recover the guard and carry on.
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= MAX_TRACKED_KEYS && !map.contains_key(key) {
        map.retain(|_, (start, _)| now.duration_since(*start) < window);
        // ponytail: O(n) scan only while the table is full; a real LRU if this shows up in profiles.
        if map.len() >= MAX_TRACKED_KEYS {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, (start, _))| *start)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
    }
    let entry = map.entry(key.to_string()).or_insert((now, 0));
    if now.duration_since(entry.0) >= window {
        *entry = (now, 0); // window elapsed → start a fresh count
    }
    entry.1 = entry.1.saturating_add(1);
    entry.1
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one hit for `key`; return `true` while it stays within `max` hits
    /// per `window`, `false` once the caller has exceeded the limit.
    pub fn check(&self, key: &str, max: u32, window: Duration) -> bool {
        count(&self.hits, key, window) <= max
    }

    /// [`Self::check`] for keys that anonymous floods must not evict.
    pub fn check_reserved(&self, key: &str, max: u32, window: Duration) -> bool {
        count(&self.reserved, key, window) <= max
    }

    /// Record one failed attempt for `key` within `window`; return the running
    /// failure count. Used for failure-based lockout (e.g. bad login signatures).
    pub fn note_failure(&self, key: &str, window: Duration) -> u32 {
        count(&self.failures, key, window)
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

/// The throttling key for a client address. One IPv6 subscriber usually
/// holds at least a /64, so keying single IPv6 addresses would hand an
/// attacker 2^64 fresh budgets.
pub(crate) fn limit_key(ip: Option<IpAddr>) -> String {
    match ip {
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!(
                    "{}/64",
                    std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0)
                )
            }
        },
        Some(ip) => ip.to_string(),
        None => "local".to_string(),
    }
}

/// The shared 429 response for any auth throttle.
pub(crate) fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, "60")],
        Json(json!({
            "error": "rate limited",
            "hint": "too many attempts; try again later",
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
    state.rate_limiter.check_reserved(
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
    let ip = limit_key(crate::peer::client_ip(
        req.extensions(),
        req.headers(),
        &state,
    ));

    // Flat per-endpoint rate limit.
    // Keyed by route class, never by the raw path: prefix routes carry a
    // caller-chosen tail, and one key per tail would let a single source mint
    // keys until eviction resets its own counters.
    let flat = match path.as_str() {
        "/v1/auth/enroll" => Some(("enroll", limits.enroll_per_min)),
        "/v1/auth/bootstrap" => Some(("bootstrap", limits.enroll_per_min.min(3))),
        "/v1/auth/pairing/requests" => Some(("pairing", limits.enroll_per_min.min(5))),
        "/v1/auth/challenge" => Some(("challenge", limits.challenge_per_min)),
        "/v1/auth/login" => Some(("login", limits.login_per_min)),
        _ if path.starts_with("/v1/auth/account/") => Some(("account", 5)),
        _ if path.starts_with("/v1/devices/") && path.ends_with("/revoke-request") => {
            Some(("revoke", 5))
        }
        _ if path.starts_with("/v1/auth/pairing/requests/") && path.ends_with("/status") => {
            Some(("pairing-status", limits.challenge_per_min))
        }
        _ => None,
    };
    if let Some((class, max)) = flat {
        if !state
            .rate_limiter
            .check(&format!("{class}:{ip}"), max, window)
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

    #[test]
    fn a_full_table_admits_new_callers_and_never_touches_devices() {
        let rl = RateLimiter::new();
        let w = Duration::from_secs(60);
        assert!(rl.check_reserved("authenticated:device:owner", 1, w));
        for n in 0..MAX_TRACKED_KEYS {
            assert!(rl.check(&format!("flood:{n}"), 1, w));
        }
        // A fresh caller still gets its own budget, and the table stays bounded.
        assert!(rl.check("owner-login", 1, w));
        assert!(!rl.check("owner-login", 1, w));
        assert!(rl.hits.lock().unwrap().len() <= MAX_TRACKED_KEYS);
        // Anonymous churn neither evicts nor resets an owner device's window.
        assert!(!rl.check_reserved("authenticated:device:owner", 1, w));
    }

    #[test]
    fn ipv6_callers_share_their_slash_64() {
        let key = |ip: &str| limit_key(Some(ip.parse().unwrap()));
        assert_eq!(key("2001:db8:1:2:aaaa::1"), "2001:db8:1:2::/64");
        assert_eq!(key("2001:db8:1:2:ffff:1:2:3"), key("2001:db8:1:2::9"));
        assert_ne!(key("2001:db8:1:2::1"), key("2001:db8:1:3::1"));
        assert_eq!(key("::ffff:203.0.113.7"), "203.0.113.7");
        assert_eq!(key("203.0.113.7"), "203.0.113.7");
        assert_eq!(limit_key(None), "local");
    }
}
