//! Health of the IBKR portfolio sync for the owner: the sync task, the
//! broker unit, the broker's own status and the stored portfolio, plus a
//! list of problems with fixed texts and commands to copy. Commands are
//! never run or linked; every text that leaves Core is a constant here, and
//! codes the broker sends are mapped onto a fixed list.
//!
//! [`collect`] and [`problems`] work without HTTP (for a scheduled check);
//! the route serves [`body`] through [`cached`].

use std::future::Future;
use std::time::{Duration, Instant};

use jarvis_ibkr::broker::{BrokerClient, BrokerError};
use jarvis_ibkr::oauth::{BrokerStatus, Brokerage, LstState};
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_store::Database;

use super::{store, SyncMode, SyncStatus, SyncStatusHandle};
use crate::AppState;

/// Polling clients share one collection per window.
pub const CACHE_TTL: Duration = Duration::from_secs(5);
/// The broker answers `status` from memory; anything slower is a problem.
const BROKER_STATUS_TIMEOUT: Duration = Duration::from_secs(3);
const STORE_TIMEOUT: Duration = Duration::from_secs(10);
/// Three missed syncs at the normal interval.
const STALE_AFTER: time::Duration = time::Duration::minutes(15);
/// `sync_failing` is an error from this many failures in a row.
const FAILING_ERROR_AFTER: u32 = 3;
const MAX_PROBLEMS: usize = 8;
/// Codes of the broker's `status_code`; anything else is `unknown`.
const BROKER_CODES: [&str; 6] = [
    "auth_rejected",
    "not_authenticated",
    "rate_limited",
    "upstream",
    "busy",
    "invalid_input",
];

/// Facts of `jarvis-ibkr-broker.service`, from a fixed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerUnit {
    /// `active`, `activating`, `inactive`, `failed`, `deactivating`,
    /// `not_found` or `unknown`.
    pub service: &'static str,
    pub enabled_at_boot: Option<bool>,
    /// `present`, `missing` or `unknown`: the unit's credential condition at
    /// its last start.
    pub credentials: &'static str,
}

impl BrokerUnit {
    pub const UNKNOWN: Self = Self {
        service: "unknown",
        enabled_at_boot: None,
        credentials: "unknown",
    };
}

/// The broker's answer to `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerReply {
    /// The sync is not configured, so Core has no broker client.
    NotAsked,
    /// No answer in time, a socket of another uid, or a reply Core cannot
    /// read (also a newer broker reporting a state this Core does not know).
    Unreachable,
    /// The broker predates the status request.
    Outdated,
    Status(Box<BrokerStatus>),
}

/// Everything one status check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    pub checked_at: OffsetDateTime,
    pub sync: SyncStatus,
    pub unit: BrokerUnit,
    pub broker: BrokerReply,
    /// Time of the stored portfolio; `Err` when the database could not be
    /// read. Only read while the sync is on.
    pub stored_as_of: Result<Option<OffsetDateTime>, ()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub severity: Severity,
    pub summary: &'static str,
    /// Shown for the owner to copy; never run by Core or the app.
    pub commands: &'static [&'static str],
}

const ENV: &str = "Set JARVIS_IBKR_PORTFOLIO_SYNC_ENABLED=true in /etc/jarvis/core.env";
const RC: &str = "sudo systemctl restart jarvis-core.service";
const CS: &str = "sudo jarvis-credentials status ibkr-oauth";
const SET: &str = "sudo jarvis-credentials set ibkr-oauth";
const EN: &str = "sudo systemctl enable --now jarvis-ibkr-broker.service";
const RB: &str = "sudo systemctl reset-failed jarvis-ibkr-broker.service && sudo systemctl restart jarvis-ibkr-broker.service";
const LOG: &str = "sudo journalctl -u jarvis-ibkr-broker.service -n 50 --no-pager";
const CLOG: &str = "sudo jarvis logs core --lines 100";
const FIX_SOCKET: &str = "Fix JARVIS_IBKR_BROKER_SOCKET in /etc/jarvis/core.env: an absolute path, by default /run/jarvis-ibkr-broker/broker.sock";
const OWN_ACCOUNT: &str =
    "The jarvis-ibkr account must be its own user, neither root nor Core's user";

/// Every problem code with its summary and commands.
const CATALOG: [(&str, &str, &[&str]); 17] = [
    (
        "sync_disabled",
        "The IBKR portfolio sync is turned off.",
        &[ENV, RC],
    ),
    (
        "config_refused",
        "Core refused the IBKR sync configuration at startup.",
        &[SET, RC],
    ),
    (
        "broker_not_installed",
        "The IBKR broker service is not installed.",
        &["sudo jarvis update"],
    ),
    (
        "credentials_missing",
        "The IBKR broker has no OAuth credentials.",
        &[CS, SET, EN],
    ),
    (
        "broker_not_enabled",
        "The IBKR broker service does not start at boot.",
        &[CS, EN],
    ),
    (
        "broker_failed",
        "The IBKR broker service has failed.",
        &[LOG, SET, RB],
    ),
    (
        "broker_unreachable",
        "Core cannot reach the IBKR broker.",
        &[RB, LOG],
    ),
    (
        "broker_restart_needed",
        "The IBKR broker still runs an older version; restart it.",
        &["sudo systemctl restart jarvis-ibkr-broker.service"],
    ),
    (
        "auth_rejected",
        "IBKR rejected the broker's OAuth credentials.",
        &[
            "Renew the access token in the IBKR OAuth self-service portal",
            SET,
        ],
    ),
    (
        "token_expired",
        "The IBKR session token expired; the next request renews it.",
        &[],
    ),
    (
        "session_competing",
        "Another IBKR session competes with the broker's session.",
        &["Log out of other IBKR sessions (TWS, Client Portal, mobile)"],
    ),
    (
        "rate_limited",
        "IBKR is rate limiting the broker; it retries by itself.",
        &[],
    ),
    (
        "account_unsupported",
        "The IBKR account is not supported.",
        &["Jarvis supports one cash account with base currency EUR"],
    ),
    (
        "sync_failing",
        "The portfolio sync is failing.",
        &[CLOG, LOG],
    ),
    (
        "sync_stale",
        "The portfolio data is more than 15 minutes old.",
        &[CLOG],
    ),
    (
        "first_sync_pending",
        "The first portfolio sync has not finished yet.",
        &[],
    ),
    (
        "core_store_unavailable",
        "Core cannot read the stored portfolio.",
        &["sudo systemctl status jarvis-surrealdb.service"],
    ),
];

/// Collect the status now, without the route's cache.
pub async fn collect(state: &AppState) -> Collected {
    gather(
        &state.db,
        state.ibkr.as_deref(),
        &state.ibkr_status,
        crate::routes::services::SYSTEMCTL,
    )
    .await
}

async fn gather(
    db: &Database,
    client: Option<&BrokerClient>,
    handle: &SyncStatusHandle,
    systemctl: &str,
) -> Collected {
    let sync = handle.get();
    let on = sync.mode == SyncMode::On;
    let (unit, broker, stored_as_of) = tokio::join!(
        crate::routes::services::ibkr_broker_unit(systemctl),
        broker_reply(client),
        async {
            if on {
                stored_as_of(db).await
            } else {
                Ok(None)
            }
        },
    );
    Collected {
        checked_at: OffsetDateTime::now_utc(),
        sync,
        unit,
        broker,
        stored_as_of,
    }
}

async fn broker_reply(client: Option<&BrokerClient>) -> BrokerReply {
    let Some(client) = client else {
        return BrokerReply::NotAsked;
    };
    match tokio::time::timeout(BROKER_STATUS_TIMEOUT, client.status()).await {
        Ok(Ok(status)) => BrokerReply::Status(Box::new(status)),
        Ok(Err(BrokerError::InvalidRequest)) => BrokerReply::Outdated,
        Ok(Err(_)) | Err(_) => BrokerReply::Unreachable,
    }
}

async fn stored_as_of(db: &Database) -> Result<Option<OffsetDateTime>, ()> {
    match tokio::time::timeout(STORE_TIMEOUT, store::last_sync_at(db)).await {
        Ok(Ok(at)) => Ok(at),
        Ok(Err(error)) => {
            tracing::warn!(%error, "IBKR status read failed");
            Err(())
        }
        Err(_) => {
            tracing::warn!("IBKR status read timed out");
            Err(())
        }
    }
}

/// The route's body, collected at most once per [`CACHE_TTL`]. Concurrent
/// callers wait for the one collection in flight.
pub async fn cached<F>(handle: &SyncStatusHandle, collect: impl FnOnce() -> F) -> Value
where
    F: Future<Output = Collected>,
{
    let mut cache = handle.cache.lock().await;
    if let Some((at, value)) = cache.as_ref() {
        if at.elapsed() < CACHE_TTL {
            return value.clone();
        }
    }
    let value = body(&collect().await);
    *cache = Some((Instant::now(), value.clone()));
    value
}

fn sync_state(sync: &SyncStatus) -> &'static str {
    // The last attempt found no broker socket.
    let waiting = matches!(
        sync.last_error,
        Some(("broker_not_running", at)) if Some(at) == sync.last_attempt_at
    );
    match sync.mode {
        SyncMode::Off => "off",
        SyncMode::Refused(_) => "refused",
        SyncMode::On if sync.last_attempt_at.is_none() => "starting",
        SyncMode::On if waiting => "waiting_for_broker",
        SyncMode::On if sync.consecutive_failures == 0 => "ok",
        SyncMode::On => "failing",
    }
}

/// The last good sync: the stored one survives a Core restart.
fn last_success(collected: &Collected) -> Option<OffsetDateTime> {
    let stored = collected.stored_as_of.ok().flatten();
    collected.sync.last_success_at.max(stored)
}

/// What to fix, most severe first, at most [`MAX_PROBLEMS`].
pub fn problems(collected: &Collected) -> Vec<Problem> {
    use Severity::{Error, Info, Warn};
    let sync = &collected.sync;
    let mut found = Vec::new();
    match sync.mode {
        SyncMode::Off => return finish(vec![("sync_disabled", Info)]),
        SyncMode::Refused(_) => found.push(("config_refused", Error)),
        SyncMode::On => {}
    }

    let unit = collected.unit;
    if unit.service == "not_found" {
        found.push(("broker_not_installed", Error));
    } else {
        if unit.credentials == "missing" {
            found.push(("credentials_missing", Error));
        } else if unit.enabled_at_boot == Some(false) {
            found.push(("broker_not_enabled", Error));
        }
        if unit.service == "failed" {
            found.push(("broker_failed", Error));
        }
    }
    // Only a unit that is not running explains an unreachable broker.
    let unit_explains = matches!(unit.service, "not_found" | "inactive" | "failed")
        && found.iter().any(|(code, _)| *code != "config_refused");

    match &collected.broker {
        BrokerReply::NotAsked => {}
        BrokerReply::Unreachable if !unit_explains => found.push(("broker_unreachable", Error)),
        BrokerReply::Unreachable => {}
        BrokerReply::Outdated => found.push(("broker_restart_needed", Warn)),
        BrokerReply::Status(status) => {
            if status.lst.state == LstState::Rejected
                || status.last_mint.error.as_deref() == Some("auth_rejected")
            {
                found.push(("auth_rejected", Error));
            } else if status.lst.state == LstState::Expired {
                found.push(("token_expired", Info));
            }
            if status.brokerage == Brokerage::Competing {
                found.push(("session_competing", Warn));
            }
            if status.rate_limited_until.is_some() {
                found.push(("rate_limited", Warn));
            }
        }
    }

    if sync.mode == SyncMode::On {
        match sync_state(sync) {
            "failing" if matches!(sync.last_error, Some(("account_unsupported", _))) => {
                found.push(("account_unsupported", Error));
            }
            "failing" if sync.consecutive_failures >= FAILING_ERROR_AFTER => {
                found.push(("sync_failing", Error));
            }
            "failing" => found.push(("sync_failing", Warn)),
            "starting" if collected.stored_as_of == Ok(None) => {
                found.push(("first_sync_pending", Info));
            }
            _ => {}
        }
        if last_success(collected).is_some_and(|at| collected.checked_at - at > STALE_AFTER) {
            found.push(("sync_stale", Warn));
        }
        if collected.stored_as_of.is_err() {
            found.push(("core_store_unavailable", Error));
        }
    }
    let mut problems = finish(found);
    if let SyncMode::Refused(reason) = sync.mode {
        for problem in problems.iter_mut().filter(|p| p.code == "config_refused") {
            problem.commands = refused_commands(reason);
        }
    }
    problems
}

/// What to do about a [`super::ClientRefusal`] code.
fn refused_commands(reason: &str) -> &'static [&'static str] {
    match reason {
        "socket_path_invalid" => &[FIX_SOCKET, RC],
        "broker_account_unsafe" => &[OWN_ACCOUNT, CS, RC],
        _ => &[SET, RC],
    }
}

fn finish(mut found: Vec<(&'static str, Severity)>) -> Vec<Problem> {
    // Stable: equal severities keep the order above.
    found.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    found.truncate(MAX_PROBLEMS);
    found
        .into_iter()
        .filter_map(|(code, severity)| {
            let (code, summary, commands) = CATALOG.iter().find(|entry| entry.0 == code)?;
            Some(Problem {
                code,
                severity,
                summary,
                commands,
            })
        })
        .collect()
}

fn ts(at: OffsetDateTime) -> Value {
    at.format(&Rfc3339).map_or(Value::Null, Value::from)
}

fn opt_ts(at: Option<OffsetDateTime>) -> Value {
    at.map_or(Value::Null, ts)
}

/// Unix seconds from the broker.
fn unix_ts(secs: Option<u64>) -> Value {
    secs.and_then(|secs| i64::try_from(secs).ok())
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        .map_or(Value::Null, ts)
}

fn broker_code(code: &str) -> &'static str {
    BROKER_CODES
        .into_iter()
        .find(|known| *known == code)
        .unwrap_or("unknown")
}

fn lst_state(state: LstState) -> &'static str {
    match state {
        LstState::None => "none",
        LstState::Valid => "valid",
        LstState::RefreshDue => "refresh_due",
        LstState::Expired => "expired",
        LstState::Rejected => "rejected",
    }
}

fn brokerage(session: Brokerage) -> &'static str {
    match session {
        Brokerage::Open => "open",
        Brokerage::Closed => "closed",
        Brokerage::Competing => "competing",
    }
}

/// The status route's body. Every field is always present (`null` when not
/// known); times are RFC 3339.
pub fn body(collected: &Collected) -> Value {
    let problems = problems(collected);
    let sync = &collected.sync;
    let overall = match (sync.mode, problems.first()) {
        (SyncMode::Off, _) => "off",
        (_, Some(problem)) => problem.severity.as_str(),
        (_, None) => "ok",
    };
    let status = match &collected.broker {
        BrokerReply::Status(status) => Some(status.as_ref()),
        _ => None,
    };
    let reachable = match collected.broker {
        BrokerReply::NotAsked => Value::Null,
        BrokerReply::Unreachable => false.into(),
        BrokerReply::Outdated | BrokerReply::Status(_) => true.into(),
    };
    let token = status.map(|s| {
        json!({
            "state": lst_state(s.lst.state),
            "usable": matches!(s.lst.state, LstState::Valid | LstState::RefreshDue),
            "refreshed_at": unix_ts(s.lst.minted_at),
            "expires_at": unix_ts(s.lst.expires_at),
            "last_refresh_attempt_at": unix_ts(s.last_mint.at),
            "last_refresh_error": s.last_mint.error.as_deref().map(broker_code),
        })
    });
    let ibkr = status.map(|s| {
        json!({
            "last_ok_at": unix_ts(s.last_ok_at),
            "last_error": s.last_error.as_ref().map(|error| json!({
                "code": broker_code(&error.code),
                "at": unix_ts(Some(error.at)),
            })),
            "rate_limited_until": unix_ts(s.rate_limited_until),
            "brokerage_session": brokerage(s.brokerage),
        })
    });
    json!({
        "checked_at": ts(collected.checked_at),
        "overall": overall,
        "sync": {
            "enabled": sync.mode != SyncMode::Off,
            "state": sync_state(sync),
            "last_success_at": opt_ts(last_success(collected)),
            "last_attempt_at": opt_ts(sync.last_attempt_at),
            "next_attempt_at": opt_ts(sync.next_attempt_at),
            "consecutive_failures": sync.consecutive_failures,
            "last_error": sync.last_error.map(|(code, at)| json!({"code": code, "at": ts(at)})),
        },
        "broker": {
            "service": collected.unit.service,
            "enabled_at_boot": collected.unit.enabled_at_boot,
            "credentials": collected.unit.credentials,
            "reachable": reachable,
            "started_at": unix_ts(status.map(|s| s.started_at)),
        },
        "token": token,
        "ibkr": ibkr,
        "problems": problems
            .iter()
            .map(|p| json!({
                "code": p.code,
                "severity": p.severity.as_str(),
                "summary": p.summary,
                "commands": p.commands,
            }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jarvis_ibkr::oauth::{ErrorAt, LstStatus, MintStatus};

    use super::super::tests::{fake_broker, own_uid};
    use super::*;

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    const NOW: i64 = 1_800_000_000;

    fn broker_status() -> BrokerStatus {
        let now = NOW as u64;
        BrokerStatus {
            started_at: now - 3600,
            lst: LstStatus {
                state: LstState::Valid,
                minted_at: Some(now - 600),
                expires_at: Some(now + 3600),
            },
            last_mint: MintStatus {
                at: Some(now - 600),
                error: None,
            },
            last_ok_at: Some(now - 60),
            last_error: None,
            rate_limited_until: None,
            brokerage: Brokerage::Open,
        }
    }

    /// A healthy sync: on, synced a minute ago, broker up.
    fn healthy() -> Collected {
        Collected {
            checked_at: at(NOW),
            sync: SyncStatus {
                mode: SyncMode::On,
                last_attempt_at: Some(at(NOW - 60)),
                last_success_at: Some(at(NOW - 60)),
                last_error: None,
                consecutive_failures: 0,
                next_attempt_at: Some(at(NOW + 240)),
            },
            unit: BrokerUnit {
                service: "active",
                enabled_at_boot: Some(true),
                credentials: "present",
            },
            broker: BrokerReply::Status(Box::new(broker_status())),
            stored_as_of: Ok(Some(at(NOW - 60))),
        }
    }

    fn codes(collected: &Collected) -> Vec<(&'static str, Severity)> {
        problems(collected)
            .into_iter()
            .map(|p| (p.code, p.severity))
            .collect()
    }

    fn with(change: impl FnOnce(&mut Collected)) -> Vec<(&'static str, Severity)> {
        let mut collected = healthy();
        change(&mut collected);
        codes(&collected)
    }

    fn status(change: impl FnOnce(&mut BrokerStatus)) -> BrokerReply {
        let mut status = broker_status();
        change(&mut status);
        BrokerReply::Status(Box::new(status))
    }

    fn failing(code: &'static str, failures: u32) -> impl FnOnce(&mut Collected) {
        move |c| {
            c.sync.last_error = Some((code, at(NOW - 10)));
            c.sync.consecutive_failures = failures;
        }
    }

    #[test]
    fn every_problem_code_has_fixed_texts() {
        for (i, (code, summary, commands)) in CATALOG.iter().enumerate() {
            assert!(!summary.is_empty() && commands.len() <= 4, "{code}");
            assert!(CATALOG[i + 1..].iter().all(|other| other.0 != *code));
        }
    }

    #[test]
    fn problems_per_code() {
        use Severity::{Error, Info, Warn};
        assert!(codes(&healthy()).is_empty());

        assert_eq!(
            with(|c| {
                c.sync.mode = SyncMode::Off;
                c.unit.service = "not_found";
            }),
            [("sync_disabled", Info)]
        );
        assert_eq!(
            with(|c| {
                c.sync.mode = SyncMode::Refused("broker_account_missing");
                c.broker = BrokerReply::NotAsked;
            }),
            [("config_refused", Error)]
        );
        // A missing unit explains an unreachable broker.
        assert_eq!(
            with(|c| {
                c.unit = BrokerUnit {
                    service: "not_found",
                    enabled_at_boot: None,
                    credentials: "unknown",
                };
                c.broker = BrokerReply::Unreachable;
                c.sync.last_error = Some(("broker_not_running", at(NOW - 60)));
            }),
            [("broker_not_installed", Error)]
        );
        assert_eq!(
            with(|c| {
                c.unit.service = "inactive";
                c.unit.credentials = "missing";
                c.unit.enabled_at_boot = Some(false);
                c.broker = BrokerReply::Unreachable;
            }),
            [("credentials_missing", Error)]
        );
        assert_eq!(
            with(|c| c.unit.enabled_at_boot = Some(false)),
            [("broker_not_enabled", Error)]
        );
        // A running unit does not explain an unreachable broker.
        assert_eq!(
            with(|c| {
                c.unit.enabled_at_boot = Some(false);
                c.broker = BrokerReply::Unreachable;
            }),
            [("broker_not_enabled", Error), ("broker_unreachable", Error)]
        );
        assert_eq!(
            with(|c| {
                c.unit.service = "inactive";
                c.unit.enabled_at_boot = Some(false);
                c.broker = BrokerReply::Unreachable;
            }),
            [("broker_not_enabled", Error)]
        );
        assert_eq!(
            with(|c| {
                c.unit.service = "failed";
                c.broker = BrokerReply::Unreachable;
            }),
            [("broker_failed", Error)]
        );
        assert_eq!(
            with(|c| c.broker = BrokerReply::Unreachable),
            [("broker_unreachable", Error)]
        );
        assert_eq!(
            with(|c| c.broker = BrokerReply::Outdated),
            [("broker_restart_needed", Warn)]
        );
        assert_eq!(
            with(|c| c.broker = status(|s| s.lst.state = LstState::Rejected)),
            [("auth_rejected", Error)]
        );
        assert_eq!(
            with(|c| {
                c.broker = status(|s| {
                    s.lst.state = LstState::None;
                    s.last_mint.error = Some("auth_rejected".into());
                })
            }),
            [("auth_rejected", Error)]
        );
        assert_eq!(
            with(|c| c.broker = status(|s| s.lst.state = LstState::Expired)),
            [("token_expired", Info)]
        );
        // Inside the refresh margin is still fine.
        assert!(with(|c| c.broker = status(|s| s.lst.state = LstState::RefreshDue)).is_empty());
        assert_eq!(
            with(|c| c.broker = status(|s| s.brokerage = Brokerage::Competing)),
            [("session_competing", Warn)]
        );
        assert_eq!(
            with(|c| c.broker = status(|s| s.rate_limited_until = Some(NOW as u64 + 30))),
            [("rate_limited", Warn)]
        );
        assert_eq!(
            with(failing("account_unsupported", 5)),
            [("account_unsupported", Error)]
        );
        assert_eq!(with(failing("ibkr_failed", 2)), [("sync_failing", Warn)]);
        assert_eq!(with(failing("ibkr_failed", 3)), [("sync_failing", Error)]);
        // Waiting for a broker that is not there is the broker's problem.
        assert!(with(|c| {
            c.sync.last_error = Some(("broker_not_running", at(NOW - 60)));
        })
        .is_empty());
        assert_eq!(
            with(|c| {
                c.sync.last_success_at = Some(at(NOW - 16 * 60));
                c.stored_as_of = Ok(Some(at(NOW - 16 * 60)));
            }),
            [("sync_stale", Warn)]
        );
        // The stored sync counts after a Core restart.
        assert!(with(|c| {
            c.sync.last_success_at = Some(at(NOW - 16 * 60));
        })
        .is_empty());
        assert_eq!(
            with(|c| {
                c.sync.last_attempt_at = None;
                c.sync.last_success_at = None;
                c.stored_as_of = Ok(None);
            }),
            [("first_sync_pending", Info)]
        );
        assert_eq!(
            with(|c| c.stored_as_of = Err(())),
            [("core_store_unavailable", Error)]
        );
    }

    #[test]
    fn a_refused_configuration_says_what_to_fix() {
        let commands = |reason| {
            let mut collected = healthy();
            collected.sync = SyncStatus::new(SyncMode::Refused(reason), at(NOW));
            collected.broker = BrokerReply::NotAsked;
            let found = problems(&collected);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].code, "config_refused");
            found[0].commands
        };
        assert_eq!(commands("broker_account_missing"), [SET, RC]);
        assert_eq!(commands("socket_path_invalid"), [FIX_SOCKET, RC]);
        assert_eq!(commands("broker_account_unsafe"), [OWN_ACCOUNT, CS, RC]);
    }

    #[test]
    fn waiting_for_the_broker_is_not_a_failing_sync() {
        let mut collected = healthy();
        let next = Duration::from_secs(300);
        for i in 0..2 {
            collected
                .sync
                .record(Err("ibkr_failed"), at(NOW - 100 + i), next);
        }
        for i in 0..5 {
            collected
                .sync
                .record(Err("broker_not_running"), at(NOW - 50 + i), next);
        }
        assert_eq!(body(&collected)["sync"]["state"], "waiting_for_broker");
        assert_eq!(collected.sync.consecutive_failures, 0);
        assert!(codes(&collected).is_empty());
        // One more failure after the waits is a first failure, not a third.
        collected
            .sync
            .record(Err("ibkr_failed"), at(NOW - 10), next);
        assert_eq!(codes(&collected), [("sync_failing", Severity::Warn)]);
        assert_eq!(body(&collected)["sync"]["state"], "failing");
    }

    #[test]
    fn problems_are_ordered_by_severity_and_capped() {
        let mut collected = healthy();
        collected.unit.credentials = "missing";
        collected.unit.service = "failed";
        collected.broker = status(|s| {
            s.lst.state = LstState::Rejected;
            s.brokerage = Brokerage::Competing;
            s.rate_limited_until = Some(NOW as u64 + 30);
        });
        collected.sync.last_error = Some(("ibkr_auth", at(NOW - 10)));
        collected.sync.consecutive_failures = 1;
        collected.sync.last_success_at = Some(at(NOW - 3600));
        collected.stored_as_of = Err(());
        let found = codes(&collected);
        assert_eq!(found.len(), MAX_PROBLEMS);
        assert!(found.windows(2).all(|pair| pair[0].1 >= pair[1].1));
        assert_eq!(found[0], ("credentials_missing", Severity::Error));

        let many = CATALOG.iter().map(|entry| (entry.0, Severity::Info));
        let mut input: Vec<_> = many.collect();
        input.push(("core_store_unavailable", Severity::Error));
        let capped = finish(input);
        assert_eq!(capped.len(), MAX_PROBLEMS);
        assert_eq!(capped[0].code, "core_store_unavailable");
        assert_eq!(capped[1].code, "sync_disabled");
    }

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    fn is_rfc3339(value: &Value) -> bool {
        value
            .as_str()
            .is_some_and(|text| OffsetDateTime::parse(text, &Rfc3339).is_ok())
    }

    #[test]
    fn body_has_every_field_with_rfc3339_times() {
        let value = body(&healthy());
        assert_eq!(
            keys(&value),
            [
                "broker",
                "checked_at",
                "ibkr",
                "overall",
                "problems",
                "sync",
                "token"
            ]
        );
        assert_eq!(value["overall"], "ok");
        assert!(is_rfc3339(&value["checked_at"]));
        assert_eq!(
            keys(&value["sync"]),
            [
                "consecutive_failures",
                "enabled",
                "last_attempt_at",
                "last_error",
                "last_success_at",
                "next_attempt_at",
                "state"
            ]
        );
        assert_eq!(value["sync"]["state"], "ok");
        assert_eq!(value["sync"]["enabled"], true);
        assert!(value["sync"]["last_error"].is_null());
        assert!(is_rfc3339(&value["sync"]["next_attempt_at"]));
        assert_eq!(
            value["broker"],
            json!({
                "service": "active",
                "enabled_at_boot": true,
                "credentials": "present",
                "reachable": true,
                "started_at": "2027-01-15T07:00:00Z",
            })
        );
        assert_eq!(
            value["token"],
            json!({
                "state": "valid",
                "usable": true,
                "refreshed_at": "2027-01-15T07:50:00Z",
                "expires_at": "2027-01-15T09:00:00Z",
                "last_refresh_attempt_at": "2027-01-15T07:50:00Z",
                "last_refresh_error": null,
            })
        );
        assert_eq!(
            value["ibkr"],
            json!({
                "last_ok_at": "2027-01-15T07:59:00Z",
                "last_error": null,
                "rate_limited_until": null,
                "brokerage_session": "open",
            })
        );
        assert_eq!(value["problems"], json!([]));

        // Off: no broker asked, nulls present, overall off.
        let mut off = healthy();
        off.sync = SyncStatus::new(SyncMode::Off, at(NOW));
        off.broker = BrokerReply::NotAsked;
        off.stored_as_of = Ok(None);
        let value = body(&off);
        assert_eq!(value["overall"], "off");
        assert_eq!(value["sync"]["state"], "off");
        assert_eq!(value["sync"]["enabled"], false);
        for key in ["last_success_at", "last_attempt_at", "next_attempt_at"] {
            assert!(value["sync"].get(key).is_some_and(Value::is_null), "{key}");
        }
        assert!(value["broker"]["reachable"].is_null());
        assert!(value["broker"]["started_at"].is_null());
        assert!(value["token"].is_null() && value["ibkr"].is_null());
        assert_eq!(
            value["problems"],
            json!([{
                "code": "sync_disabled",
                "severity": "info",
                "summary": "The IBKR portfolio sync is turned off.",
                "commands": [ENV, RC],
            }])
        );

        let mut refused = off;
        refused.sync = SyncStatus::new(SyncMode::Refused("broker_account_unsafe"), at(NOW));
        let value = body(&refused);
        assert_eq!(value["overall"], "error");
        assert_eq!(value["sync"]["state"], "refused");
        assert_eq!(value["sync"]["enabled"], true);
        assert_eq!(value["sync"]["last_error"]["code"], "broker_account_unsafe");

        let warn = with(|c| c.broker = BrokerReply::Outdated);
        assert_eq!(warn[0].1, Severity::Warn);
        let mut outdated = healthy();
        outdated.broker = BrokerReply::Outdated;
        let value = body(&outdated);
        assert_eq!(value["overall"], "warn");
        assert_eq!(value["broker"]["reachable"], true);
        assert!(value["token"].is_null());
    }

    #[test]
    fn body_maps_broker_codes_to_a_fixed_list() {
        let mut collected = healthy();
        collected.broker = status(|s| {
            s.last_mint.error = Some("invalid_input".into());
            s.last_error = Some(ErrorAt {
                at: NOW as u64 - 5,
                code: "token=secret-value".into(),
            });
            s.started_at = u64::MAX;
        });
        let value = body(&collected);
        assert_eq!(value["token"]["last_refresh_error"], "invalid_input");
        assert_eq!(value["ibkr"]["last_error"]["code"], "unknown");
        assert!(is_rfc3339(&value["ibkr"]["last_error"]["at"]));
        // A time out of range is unknown, not an error.
        assert!(value["broker"]["started_at"].is_null());
        assert!(!value.to_string().contains("secret"));
    }

    fn on_handle() -> SyncStatusHandle {
        SyncStatusHandle::new(SyncMode::On)
    }

    #[tokio::test]
    async fn gather_reads_the_broker_status() {
        let reply = format!(
            r#"{{"ok":{}}}"#,
            serde_json::to_string(&broker_status()).unwrap()
        );
        let (_dir, socket) = fake_broker(&[("status", reply.as_str())]).await;
        let client = BrokerClient::new(&socket, own_uid());
        let db = Database::init();
        let collected = gather(&db, Some(&client), &on_handle(), "/nonexistent/systemctl").await;
        assert_eq!(
            collected.broker,
            BrokerReply::Status(Box::new(broker_status()))
        );
        assert_eq!(collected.unit, BrokerUnit::UNKNOWN);
        // A database Core cannot reach is reported, not an error.
        assert_eq!(collected.stored_as_of, Err(()));
        assert!(codes(&collected).contains(&("core_store_unavailable", Severity::Error)));
    }

    #[tokio::test]
    async fn gather_maps_an_old_or_foreign_or_newer_broker() {
        let db = Database::init();
        let ask = |socket: &std::path::Path, uid: u32| {
            let client = BrokerClient::new(socket, uid);
            let db = db.clone();
            async move {
                gather(&db, Some(&client), &on_handle(), "/nonexistent/systemctl")
                    .await
                    .broker
            }
        };
        // A broker without the status request.
        let (_old, socket) = fake_broker(&[]).await;
        assert_eq!(ask(&socket, own_uid()).await, BrokerReply::Outdated);
        let mut collected = healthy();
        collected.broker = BrokerReply::Outdated;
        assert_eq!(
            codes(&collected),
            [("broker_restart_needed", Severity::Warn)]
        );
        // A socket of another uid is refused before anything is sent.
        let reply = serde_json::to_string(&broker_status()).unwrap();
        let ok = format!(r#"{{"ok":{reply}}}"#);
        let (_foreign, socket) = fake_broker(&[("status", ok.as_str())]).await;
        assert_eq!(
            ask(&socket, own_uid().wrapping_add(1)).await,
            BrokerReply::Unreachable
        );
        // A state this Core does not know does not fail the check.
        let newer = ok.replace(r#""valid""#, r#""paused""#);
        assert_ne!(newer, ok);
        let (_newer, socket) = fake_broker(&[("status", newer.as_str())]).await;
        assert_eq!(ask(&socket, own_uid()).await, BrokerReply::Unreachable);
    }

    #[tokio::test]
    async fn gather_without_a_client_asks_no_broker_or_database() {
        let handle = SyncStatusHandle::default();
        let collected = gather(&Database::init(), None, &handle, "/nonexistent/systemctl").await;
        assert_eq!(collected.broker, BrokerReply::NotAsked);
        // Off: the database is not read.
        assert_eq!(collected.stored_as_of, Ok(None));
        assert_eq!(body(&collected)["overall"], "off");
    }

    #[tokio::test]
    async fn cached_collects_once_per_window() {
        let handle = on_handle();
        let calls = AtomicUsize::new(0);
        let collect = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            healthy()
        };
        let (first, second) = tokio::join!(cached(&handle, collect), cached(&handle, collect));
        assert_eq!(first, second);
        assert_eq!(cached(&handle, collect).await, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // After the window the next call collects again.
        handle.cache.lock().await.as_mut().unwrap().0 -= CACHE_TTL;
        cached(&handle, collect).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
