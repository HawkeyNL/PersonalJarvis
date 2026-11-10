//! GitHub poller for roadmap links (see `docs/ROADMAPS.md`).
//!
//! Every ten minutes Core reads, with the owner's read-only token, the
//! issues (which include pull requests) and the latest releases of each
//! allowlisted repository that a roadmap links to, caches each linked PR's,
//! issue's or release's state on the link and moves items forward through
//! [`Draft::github_update`], as author `github`. It talks only to
//! `https://api.github.com`, follows no redirect to another host and never
//! fetches a `url` link. The token comes only from the systemd credential
//! [`TOKEN_CREDENTIAL`]; without it, or without an allowlist, the poller is
//! off.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, ETAG, IF_NONE_MATCH, RETRY_AFTER, USER_AGENT,
};
use reqwest::{redirect, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::Notify;

use jarvis_store::Database;

use super::{github_repo, release_tag, store, Draft, Link, LinkKind, RoadmapError, Snapshot};

pub const API_BASE: &str = "https://api.github.com";
/// Comma-separated `owner/name` repositories the poller may read.
pub const REPOS_ENV: &str = "JARVIS_GITHUB_REPOS";
/// The systemd credential (in `$CREDENTIALS_DIRECTORY`) with the token.
pub const TOKEN_CREDENTIAL: &str = "github_token";
const POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 1024 * 1024;
const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// Between two owner-requested syncs.
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const MAX_REPOS: usize = 20;
/// Issue rows carry their bodies, so 50 can exceed [`MAX_BODY`].
const ISSUE_PAGE: usize = 30;
const RELEASE_PAGE: usize = 10;
/// Single fetches (PRs, issues, releases, compares) per pass; the rest
/// waits for the next pass.
const MAX_FETCHES: usize = 100;
/// Entries per in-memory cache before it starts over.
const MAX_CACHE: usize = 1000;
/// ETag answers: bytes kept in all, and per answer.
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CACHED_BODY: usize = 256 * 1024;
/// Overlap of the issues window with the previous pass, for clock skew
/// between Core and GitHub.
const SINCE_OVERLAP: Duration = Duration::from_secs(5 * 60);
const TOO_LARGE: &str = "GitHub answer is larger than 1 MiB";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PollState {
    NotConfigured,
    Starting,
    Ok,
    Error,
    RateLimited,
    /// GitHub refused the token (401); the poller stopped until a restart.
    AuthFailed,
}

#[derive(Debug, Clone, Serialize)]
pub struct PollStatus {
    pub state: PollState,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_poll_at: Option<OffsetDateTime>,
    /// A fixed message, never GitHub or database detail (that is logged).
    pub last_error: Option<&'static str>,
}

/// Why an owner-requested sync is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncRefused {
    NotConfigured,
    /// Configured, but the poller could not start.
    NotRunning,
    AuthFailed,
    TooSoon,
}

/// Shared by the poller, the routes and the link allowlist check.
pub struct GithubHandle {
    /// The allowlist as configured; empty when not set.
    repos: Vec<String>,
    configured: bool,
    /// Set when [`start`] failed: no poller runs to wake.
    start_failed: AtomicBool,
    status: RwLock<PollStatus>,
    wake: Notify,
    last_sync: Mutex<Option<Instant>>,
}

impl Default for GithubHandle {
    fn default() -> Self {
        Self::new(Vec::new(), false, None)
    }
}

impl GithubHandle {
    fn new(repos: Vec<String>, has_token: bool, error: Option<&'static str>) -> Self {
        let configured = has_token && !repos.is_empty();
        Self {
            repos,
            configured,
            start_failed: AtomicBool::new(false),
            status: RwLock::new(PollStatus {
                state: if configured {
                    PollState::Starting
                } else {
                    PollState::NotConfigured
                },
                last_poll_at: None,
                last_error: error,
            }),
            wake: Notify::new(),
            last_sync: Mutex::new(None),
        }
    }

    pub fn configured(&self) -> bool {
        self.configured
    }

    pub fn repos(&self) -> &[String] {
        &self.repos
    }

    pub fn status(&self) -> PollStatus {
        self.status
            .read()
            .map(|status| status.clone())
            .unwrap_or(PollStatus {
                state: PollState::Error,
                last_poll_at: None,
                last_error: Some("status unavailable"),
            })
    }

    fn set(&self, status: PollStatus) {
        if let Ok(mut slot) = self.status.write() {
            *slot = status;
        }
    }

    /// The allowlisted spelling of `repo` (GitHub names ignore case).
    fn allowlisted(&self, repo: &str) -> Option<&str> {
        self.repos
            .iter()
            .find(|allowed| allowed.eq_ignore_ascii_case(repo))
            .map(String::as_str)
    }

    /// Once the allowlist is set, GitHub links must name one of its
    /// repositories; `url` links are never checked or fetched.
    pub fn check_links<'a>(
        &self,
        links: impl IntoIterator<Item = &'a Link>,
    ) -> Result<(), &'static str> {
        let refused = links.into_iter().any(|link| {
            link.repo
                .as_deref()
                .is_some_and(|repo| self.allowlisted(repo).is_none())
        });
        if !self.repos.is_empty() && refused {
            return Err("a link's repo must be one of JARVIS_GITHUB_REPOS");
        }
        Ok(())
    }

    /// Wake the poller now, at most once per [`SYNC_INTERVAL`].
    pub fn request_sync(&self) -> Result<(), SyncRefused> {
        if !self.configured {
            return Err(SyncRefused::NotConfigured);
        }
        if self.start_failed.load(Ordering::Acquire) {
            return Err(SyncRefused::NotRunning);
        }
        if self.status().state == PollState::AuthFailed {
            return Err(SyncRefused::AuthFailed);
        }
        let mut last = self.last_sync.lock().map_err(|_| SyncRefused::TooSoon)?;
        if last.is_some_and(|at| at.elapsed() < SYNC_INTERVAL) {
            return Err(SyncRefused::TooSoon);
        }
        *last = Some(Instant::now());
        self.wake.notify_one();
        Ok(())
    }
}

/// The allowlist: comma-separated `owner/name`, at most 20, no duplicates.
pub fn parse_repos(value: &str) -> Result<Vec<String>, &'static str> {
    let mut repos: Vec<String> = Vec::new();
    for repo in value.split(',').map(str::trim).filter(|r| !r.is_empty()) {
        if !github_repo(repo) {
            return Err("JARVIS_GITHUB_REPOS must be comma-separated owner/name repositories");
        }
        if !repos.iter().any(|known| known.eq_ignore_ascii_case(repo)) {
            repos.push(repo.to_string());
        }
    }
    if repos.len() > MAX_REPOS {
        return Err("JARVIS_GITHUB_REPOS names more than 20 repositories");
    }
    Ok(repos)
}

/// The token from the systemd credentials directory, as a sensitive
/// `Authorization` value; `None` when it is not there. Never logged.
fn read_token(credentials: Option<&Path>) -> Result<Option<HeaderValue>, &'static str> {
    let Some(dir) = credentials else {
        return Ok(None);
    };
    let path = dir.join(TOKEN_CREDENTIAL);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("GitHub token is unreadable"),
    };
    if !metadata.is_file() || metadata.len() > 1024 {
        return Err("GitHub token is not a small regular file");
    }
    let text = std::fs::read_to_string(&path).map_err(|_| "GitHub token is unreadable")?;
    let token = text.trim();
    if !(20..=255).contains(&token.len())
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("GitHub token has an unexpected format");
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "GitHub token has an unexpected format")?;
    value.set_sensitive(true);
    Ok(Some(value))
}

/// The handle for [`REPOS_ENV`] and the credentials directory, and the token
/// when both are usable. A bad value is logged and leaves the poller off.
pub fn configure(
    repos: &str,
    credentials: Option<&Path>,
) -> (Arc<GithubHandle>, Option<HeaderValue>) {
    let (repos, repos_error) = match parse_repos(repos) {
        Ok(repos) => (repos, None),
        Err(reason) => {
            tracing::error!(reason, "GitHub roadmap sync disabled");
            (Vec::new(), Some(reason))
        }
    };
    let (token, token_error) = match read_token(credentials) {
        Ok(token) => (token, None),
        Err(reason) => {
            tracing::error!(reason, "GitHub roadmap sync disabled");
            (None, Some(reason))
        }
    };
    let handle = GithubHandle::new(repos, token.is_some(), repos_error.or(token_error));
    let token = token.filter(|_| handle.configured);
    (Arc::new(handle), token)
}

/// Spawn the poller: one pass right away, then every [`POLL_INTERVAL`] or
/// when the owner asks for a sync.
pub fn start(
    db: Database,
    handle: Arc<GithubHandle>,
    token: HeaderValue,
) -> Result<(), &'static str> {
    let poller = Url::parse(API_BASE)
        .map_err(|_| "GitHub API base is invalid")
        .and_then(|base| Poller::new(base, token, handle.repos.clone()))
        .inspect_err(|reason| {
            handle.start_failed.store(true, Ordering::Release);
            handle.set(PollStatus {
                state: PollState::Error,
                last_poll_at: None,
                last_error: Some(reason),
            })
        })?;
    tokio::spawn(poller.run(db, handle));
    tracing::info!("GitHub roadmap sync running (read-only)");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollError {
    /// 401: the token is wrong, expired or revoked.
    Auth,
    RateLimited(Duration),
    Failed(&'static str),
}

/// An issue or pull request as the issues API lists it.
#[derive(Debug, Deserialize)]
struct IssueRow {
    number: u64,
    state: String,
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    pull_request: Option<PullRef>,
}

#[derive(Debug, Deserialize)]
struct PullRef {
    #[serde(default, with = "time::serde::rfc3339::option")]
    merged_at: Option<OffsetDateTime>,
}

#[derive(Debug, Deserialize)]
struct Pull {
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    merged_at: Option<OffsetDateTime>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    published_at: Option<OffsetDateTime>,
}

#[derive(Debug, Deserialize)]
struct Compare {
    status: String,
}

/// A merged PR's merge commit, which never changes.
#[derive(Debug, Clone)]
struct Merge {
    sha: String,
    at: OffsetDateTime,
}

/// What one link points at, with the allowlisted repo spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Target {
    Pr(String, u64),
    Issue(String, u64),
    Release(String, String),
}

impl Target {
    fn repo(&self) -> &str {
        match self {
            Self::Pr(repo, _) | Self::Issue(repo, _) | Self::Release(repo, _) => repo,
        }
    }
}

/// The target of a GitHub link in an allowlisted repo; `None` for `url`
/// links and other repos, which are never fetched.
fn target(link: &Link, repos: &[String]) -> Option<Target> {
    let repo = repos
        .iter()
        .find(|allowed| {
            link.repo
                .as_deref()
                .is_some_and(|r| allowed.eq_ignore_ascii_case(r))
        })?
        .clone();
    match link.kind {
        LinkKind::Pr => Some(Target::Pr(repo, link.number?)),
        LinkKind::Issue => Some(Target::Issue(repo, link.number?)),
        LinkKind::Release => Some(Target::Release(repo, link.tag.clone()?)),
        LinkKind::Url => None,
    }
}

/// The cached state of every target the roadmaps link to. Links that
/// disagree on a target count as not known yet, so it is fetched and every
/// link gets the same state.
fn cached_states(roadmaps: &[Snapshot], repos: &[String]) -> HashMap<Target, Option<String>> {
    let mut cached: HashMap<Target, Option<String>> = HashMap::new();
    for link in roadmaps
        .iter()
        .flat_map(|snapshot| &snapshot.items)
        .flat_map(|item| &item.links)
    {
        if let Some(target) = target(link, repos) {
            cached
                .entry(target)
                .and_modify(|state| {
                    if *state != link.state {
                        *state = None;
                    }
                })
                .or_insert_with(|| link.state.clone());
        }
    }
    cached
}

fn issue_state(row: &IssueRow) -> &'static str {
    match (row.state.as_str(), row.state_reason.as_deref()) {
        ("open", _) => "open",
        ("closed", Some("completed")) => "completed",
        _ => "closed",
    }
}

/// A PR's state from its issue row; an issue under a `pr` link is not found.
fn row_pr_state(row: &IssueRow) -> &'static str {
    match (&row.pull_request, row.state.as_str()) {
        (None, _) => "not_found",
        (Some(_), "open") if row.draft == Some(true) => "draft",
        (Some(_), "open") => "open",
        (Some(pull), _) if pull.merged_at.is_some() => "merged",
        _ => "closed",
    }
}

fn pull_state(pull: &Pull) -> &'static str {
    match pull.state.as_str() {
        "open" if pull.draft => "draft",
        "open" => "open",
        _ if pull.merged_at.is_some() => "merged",
        _ => "closed",
    }
}

fn release_state(release: &Release) -> &'static str {
    if release.draft {
        "unreleased"
    } else if release.prerelease {
        "prerelease"
    } else {
        "published"
    }
}

/// How long GitHub asks us to wait, from `Retry-After` or an exhausted
/// `X-RateLimit-Remaining` with its reset time; 1 s to 1 h.
fn rate_limit_wait(headers: &HeaderMap, now: OffsetDateTime) -> Option<Duration> {
    let header = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<i64>().ok();
    let seconds = header(RETRY_AFTER.as_str()).or_else(|| {
        (header("x-ratelimit-remaining")? == 0)
            .then(|| header("x-ratelimit-reset"))
            .flatten()
            .map(|reset| reset - now.unix_timestamp())
    })?;
    Some(Duration::from_secs(seconds.clamp(1, 3600) as u64))
}

/// Delay before the next pass after `failures` failed ones in a row.
fn next_delay(failures: u32) -> Duration {
    POLL_INTERVAL
        .saturating_mul(1 << failures.min(3))
        .min(MAX_BACKOFF)
}

/// The state to show and the delay before the next pass, after one pass's
/// outcome and `failures` failed passes in a row before it.
fn settle(
    outcome: Result<Option<&'static str>, PollError>,
    failures: &mut u32,
) -> (PollState, Option<&'static str>, Duration) {
    match outcome {
        Ok(None) => {
            *failures = 0;
            (PollState::Ok, None, POLL_INTERVAL)
        }
        // One repo or roadmap failed: the others stay on time.
        Ok(Some(reason)) => {
            *failures = 0;
            (PollState::Error, Some(reason), POLL_INTERVAL)
        }
        Err(PollError::Failed(reason)) => {
            *failures = failures.saturating_add(1);
            (PollState::Error, Some(reason), next_delay(*failures))
        }
        Err(PollError::RateLimited(wait)) => (
            PollState::RateLimited,
            Some("GitHub rate limit reached"),
            wait.clamp(POLL_INTERVAL, MAX_BACKOFF),
        ),
        Err(PollError::Auth) => (
            PollState::AuthFailed,
            Some("GitHub refused the token; replace it and restart Core"),
            Duration::ZERO,
        ),
    }
}

struct Poller {
    client: reqwest::Client,
    base: Url,
    token: HeaderValue,
    repos: Vec<String>,
    /// ETag and body per URL, for conditional requests.
    etags: HashMap<Url, (HeaderValue, Vec<u8>)>,
    etag_bytes: usize,
    merges: HashMap<(String, u64), Merge>,
    /// Whether a release tag contains a merge commit: (repo, sha, tag).
    contained: HashMap<(String, String, String), bool>,
    /// Start of the last complete pass over a repo's issues.
    since: HashMap<String, OffsetDateTime>,
    fetches: usize,
}

impl Poller {
    fn new(base: Url, token: HeaderValue, repos: Vec<String>) -> Result<Self, &'static str> {
        let origin = base.origin();
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            // Same-origin redirects (a renamed repository) only: the token
            // never goes to another host.
            .redirect(redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() < 3 && attempt.url().origin() == origin {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .no_proxy()
            .build()
            .map_err(|_| "GitHub client unavailable")?;
        Ok(Self {
            client,
            base,
            token,
            repos,
            etags: HashMap::new(),
            etag_bytes: 0,
            merges: HashMap::new(),
            contained: HashMap::new(),
            since: HashMap::new(),
            fetches: 0,
        })
    }

    async fn run(mut self, db: Database, handle: Arc<GithubHandle>) {
        let mut failures: u32 = 0;
        let mut last: Option<(PollState, Option<&'static str>)> = None;
        loop {
            let outcome = self.pass(&db).await;
            let (state, error, delay) = settle(outcome, &mut failures);
            handle.set(PollStatus {
                state,
                last_poll_at: Some(OffsetDateTime::now_utc()),
                last_error: error,
            });
            if last != Some((state, error)) {
                match error {
                    None => tracing::info!("GitHub roadmap sync ok"),
                    Some(reason) => tracing::warn!(reason, "GitHub roadmap sync failed"),
                }
                last = Some((state, error));
            }
            match state {
                // Cached link states stay as they are.
                PollState::AuthFailed => return,
                PollState::RateLimited => tokio::time::sleep(delay).await,
                _ => {
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = handle.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// One pass over every roadmap that is not archived. `Ok(Some(_))` when
    /// some repo or write failed; what succeeded is kept.
    async fn pass(&mut self, db: &Database) -> Result<Option<&'static str>, PollError> {
        let roadmaps = load(db).await?;
        let cached = cached_states(&roadmaps, &self.repos);
        let (states, mut error, windows) = self.states(&cached).await?;
        // A failed repo has no window; the others may still move on.
        let mut stored = true;
        for snapshot in roadmaps {
            match write(db, snapshot, &self.repos, &states).await {
                Ok(written) => stored &= written,
                Err(reason) => {
                    stored = false;
                    error.get_or_insert(reason);
                }
            }
        }
        // The issues window moves on only once everything it showed is
        // stored; otherwise the next pass reads the same window again.
        if stored {
            self.since.extend(windows);
        }
        Ok(error)
    }

    /// The current state of every target GitHub told us about this pass,
    /// the first repo error, and the new issues window of each repo whose
    /// targets are all up to date. When every repo with targets failed, the
    /// pass failed as a whole, so the poller backs off.
    async fn states(
        &mut self,
        cached: &HashMap<Target, Option<String>>,
    ) -> Result<
        (
            HashMap<Target, String>,
            Option<&'static str>,
            Vec<(String, OffsetDateTime)>,
        ),
        PollError,
    > {
        self.fetches = 0;
        if self.contained.len() > MAX_CACHE {
            self.contained.clear();
        }
        if self.merges.len() > MAX_CACHE {
            self.merges.clear();
        }
        let mut states = HashMap::new();
        let mut error = None;
        let mut windows = Vec::new();
        let mut synced = 0;
        for repo in self.repos.clone() {
            let targets: Vec<(&Target, &Option<String>)> =
                cached.iter().filter(|(t, _)| t.repo() == repo).collect();
            if targets.is_empty() {
                continue;
            }
            match self.repo_states(&repo, &targets, &mut states).await {
                Ok(window) => {
                    synced += 1;
                    windows.extend(window.map(|started| (repo, started)));
                }
                Err(PollError::Failed(reason)) => {
                    tracing::warn!(repo, reason, "GitHub repository not synced");
                    error.get_or_insert(reason);
                }
                Err(other) => return Err(other),
            }
        }
        match error {
            Some(reason) if synced == 0 => Err(PollError::Failed(reason)),
            _ => Ok((states, error, windows)),
        }
    }

    /// The states of one repo's targets; the start of this pass when every
    /// target is up to date, so the next pass may ask only for what changed
    /// since.
    async fn repo_states(
        &mut self,
        repo: &str,
        targets: &[(&Target, &Option<String>)],
        states: &mut HashMap<Target, String>,
    ) -> Result<Option<OffsetDateTime>, PollError> {
        let started = OffsetDateTime::now_utc();
        let mut url = self.url(repo, &["issues"]);
        url.query_pairs_mut()
            .append_pair("state", "all")
            .append_pair("sort", "updated")
            .append_pair("direction", "desc")
            .append_pair("per_page", &ISSUE_PAGE.to_string());
        if let Some(since) = self.since.get(repo) {
            let since = (*since - SINCE_OVERLAP)
                .format(&Rfc3339)
                .map_err(|_| PollError::Failed("invalid time"))?;
            url.query_pairs_mut().append_pair("since", &since);
        }
        let not_shared = PollError::Failed("repository not found or not shared with the token");
        // `since` changes every pass, so this list is not cached by ETag. A
        // list too large to read leaves every target to a single fetch.
        let (rows, covered) = match self.get::<Vec<IssueRow>>(url, false).await {
            Ok(Some(rows)) => {
                // Everything updated since the last pass is in the window.
                let covered = rows.len() < ISSUE_PAGE && self.since.contains_key(repo);
                (rows, covered)
            }
            Ok(None) => return Err(not_shared),
            Err(PollError::Failed(TOO_LARGE)) => (Vec::new(), false),
            Err(error) => return Err(error),
        };
        let rows: HashMap<u64, IssueRow> = rows.into_iter().map(|row| (row.number, row)).collect();
        let mut url = self.url(repo, &["releases"]);
        url.query_pairs_mut()
            .append_pair("per_page", &RELEASE_PAGE.to_string());
        let releases: Vec<Release> = self.get(url, true).await?.ok_or(not_shared)?;
        let all_releases = releases.len() < RELEASE_PAGE;

        for (target, cached) in targets {
            let state = match target {
                Target::Issue(_, number) => match rows.get(number) {
                    Some(row) => Some(issue_state(row).to_string()),
                    None if covered && cached.is_some() => None,
                    None => self.issue(repo, *number).await?,
                },
                Target::Pr(_, number) => {
                    let state = match rows.get(number) {
                        Some(row) => Some(row_pr_state(row).to_string()),
                        None if covered && cached.is_some() => (*cached).clone(),
                        None => self.pull(repo, *number).await?,
                    };
                    match state.as_deref() {
                        // A release does not stop containing the merge.
                        Some("merged") if cached.as_deref() == Some("released") => {
                            Some("released".to_string())
                        }
                        Some("merged") if self.released(repo, *number, &releases).await? => {
                            Some("released".to_string())
                        }
                        _ => state,
                    }
                }
                Target::Release(_, tag) => match releases.iter().find(|r| r.tag_name == *tag) {
                    Some(release) => Some(release_state(release).to_string()),
                    None if all_releases => Some("unreleased".to_string()),
                    None if cached.as_deref() == Some("published") => None,
                    None => self.release(repo, tag).await?,
                },
            };
            if let Some(state) = state {
                states.insert((*target).clone(), state);
            }
        }
        // ponytail: targets left over for lack of fetches keep their cached
        // state and the window stays open, so they are retried next pass (in
        // random order); a priority queue if roadmaps ever link far more than
        // MAX_FETCHES items.
        Ok((self.fetches <= MAX_FETCHES).then_some(started))
    }

    fn url(&self, repo: &str, path: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .push("repos")
                .extend(repo.split('/'))
                .extend(path);
        }
        url
    }

    /// Keep an ETag answer, within [`MAX_CACHED_BODY`] and
    /// [`MAX_CACHE_BYTES`].
    fn remember(&mut self, url: Url, etag: HeaderValue, body: &[u8]) {
        if body.len() > MAX_CACHED_BODY {
            return;
        }
        // ponytail: a replaced answer still counts; the cache just starts
        // over a little sooner.
        if self.etag_bytes + body.len() > MAX_CACHE_BYTES {
            self.etags.clear();
            self.etag_bytes = 0;
        }
        self.etag_bytes += body.len();
        self.etags.insert(url, (etag, body.to_vec()));
    }

    /// Count one single fetch; `false` when this pass has used them all.
    fn budget(&mut self) -> bool {
        self.fetches += 1;
        self.fetches <= MAX_FETCHES
    }

    async fn issue(&mut self, repo: &str, number: u64) -> Result<Option<String>, PollError> {
        if !self.budget() {
            return Ok(None);
        }
        let url = self.url(repo, &["issues", &number.to_string()]);
        let row: Option<IssueRow> = self.get(url, true).await?;
        Ok(Some(
            row.as_ref().map_or("not_found", issue_state).to_string(),
        ))
    }

    async fn pull(&mut self, repo: &str, number: u64) -> Result<Option<String>, PollError> {
        if !self.budget() {
            return Ok(None);
        }
        let url = self.url(repo, &["pulls", &number.to_string()]);
        let Some(pull) = self.get::<Pull>(url, true).await? else {
            return Ok(Some("not_found".to_string()));
        };
        if let (Some(at), Some(sha)) = (pull.merged_at, &pull.merge_commit_sha) {
            if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                let merge = Merge {
                    sha: sha.clone(),
                    at,
                };
                self.merges.insert((repo.to_string(), number), merge);
            }
        }
        Ok(Some(pull_state(&pull).to_string()))
    }

    async fn release(&mut self, repo: &str, tag: &str) -> Result<Option<String>, PollError> {
        if !self.budget() {
            return Ok(None);
        }
        let url = self.url(repo, &["releases", "tags", tag]);
        let release: Option<Release> = self.get(url, true).await?;
        Ok(Some(
            release
                .as_ref()
                .map_or("unreleased", release_state)
                .to_string(),
        ))
    }

    /// Whether a published release among the latest contains the PR's merge
    /// commit. A merged PR's merge commit is looked up once.
    async fn released(
        &mut self,
        repo: &str,
        number: u64,
        releases: &[Release],
    ) -> Result<bool, PollError> {
        let key = (repo.to_string(), number);
        if !self.merges.contains_key(&key) {
            self.pull(repo, number).await?;
        }
        let Some(merge) = self.merges.get(&key).cloned() else {
            return Ok(false);
        };
        for release in releases.iter().filter(|release| {
            release_state(release) == "published"
                && release_tag(&release.tag_name)
                && release.published_at.is_none_or(|at| at >= merge.at)
        }) {
            let key = (
                repo.to_string(),
                merge.sha.clone(),
                release.tag_name.clone(),
            );
            let contained = match self.contained.get(&key) {
                Some(contained) => *contained,
                None => {
                    if !self.budget() {
                        return Ok(false);
                    }
                    let basehead = format!("{}...{}", release.tag_name, merge.sha);
                    let mut url = self.url(repo, &["compare", &basehead]);
                    url.query_pairs_mut().append_pair("per_page", "1");
                    // Tag first: when it contains the merge, the answer has no
                    // commits and no files, so a large answer means it does not.
                    let contained = match self.get::<Compare>(url, false).await {
                        Ok(Some(compare)) => {
                            matches!(compare.status.as_str(), "behind" | "identical")
                        }
                        Ok(None) | Err(PollError::Failed(TOO_LARGE)) => false,
                        Err(error) => return Err(error),
                    };
                    self.contained.insert(key, contained);
                    contained
                }
            };
            if contained {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// GET a JSON answer; `Ok(None)` for 404 or 410. With `cache`, an ETag answer is
    /// kept and asked for again with `If-None-Match`.
    async fn get<T: DeserializeOwned>(
        &mut self,
        url: Url,
        cache: bool,
    ) -> Result<Option<T>, PollError> {
        let mut request = self
            .client
            .get(url.clone())
            .header(AUTHORIZATION, self.token.clone())
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(USER_AGENT, "jarvis-core");
        if let Some((etag, _)) = self.etags.get(&url).filter(|_| cache) {
            request = request.header(IF_NONE_MATCH, etag.clone());
        }
        let mut response = request.send().await.map_err(|error| {
            PollError::Failed(if error.is_timeout() {
                "GitHub timed out"
            } else {
                "GitHub unreachable"
            })
        })?;
        let status = response.status();
        let body = match status {
            StatusCode::NOT_MODIFIED => match self.etags.get(&url).filter(|_| cache) {
                Some((_, body)) => body.clone(),
                None => return Err(PollError::Failed("GitHub answered 304 unasked")),
            },
            status if status.is_success() => {
                let etag = response.headers().get(ETAG).cloned();
                let body = read_bounded(&mut response).await?;
                if let (true, Some(etag)) = (cache, etag) {
                    self.remember(url, etag, &body);
                }
                body
            }
            // 410: a deleted issue.
            StatusCode::NOT_FOUND | StatusCode::GONE => return Ok(None),
            StatusCode::UNAUTHORIZED => return Err(PollError::Auth),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                let wait = rate_limit_wait(response.headers(), OffsetDateTime::now_utc());
                return Err(match (wait, status) {
                    (Some(wait), _) => PollError::RateLimited(wait),
                    (None, StatusCode::TOO_MANY_REQUESTS) => {
                        PollError::RateLimited(Duration::from_secs(60))
                    }
                    _ => PollError::Failed("GitHub refused access (403)"),
                });
            }
            status if status.is_redirection() => {
                return Err(PollError::Failed("GitHub redirect refused"))
            }
            _ => return Err(PollError::Failed("GitHub answered with an error")),
        };
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|_| PollError::Failed("GitHub answer not understood"))
    }
}

async fn read_bounded(response: &mut reqwest::Response) -> Result<Vec<u8>, PollError> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_BODY as u64)
    {
        return Err(PollError::Failed(TOO_LARGE));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        PollError::Failed(if error.is_timeout() {
            "GitHub timed out"
        } else {
            "GitHub answer broke off"
        })
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(PollError::Failed(TOO_LARGE));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, RoadmapError>>,
) -> Result<T, &'static str> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "GitHub roadmap sync: database call failed");
            Err("roadmaps unavailable")
        }
        Err(_) => Err("roadmaps unavailable"),
    }
}

/// Every roadmap that is not archived, with its items.
async fn load(db: &Database) -> Result<Vec<Snapshot>, PollError> {
    let mut snapshots = Vec::new();
    for roadmap in bounded(store::list(db, false))
        .await
        .map_err(PollError::Failed)?
    {
        if let Some(snapshot) = bounded(store::get(db, &roadmap.id))
            .await
            .map_err(PollError::Failed)?
        {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

/// Apply the states to one roadmap in one revision. When the owner changed
/// it meanwhile nothing is written (`Ok(false)`); the next pass reads the
/// same issues window again and builds on the new revision.
async fn write(
    db: &Database,
    snapshot: Snapshot,
    repos: &[String],
    states: &HashMap<Target, String>,
) -> Result<bool, &'static str> {
    let updates: Vec<(String, Vec<Option<String>>)> = snapshot
        .items
        .iter()
        .map(|item| {
            let found = item
                .links
                .iter()
                .map(|link| target(link, repos).and_then(|target| states.get(&target).cloned()));
            (item.id.clone(), found.collect::<Vec<_>>())
        })
        .filter(|(_, found)| found.iter().any(Option::is_some))
        .collect();
    let mut draft = Draft::new(snapshot);
    for (id, found) in &updates {
        draft
            .github_update(id, found)
            .map_err(|_| "roadmap item vanished")?;
    }
    match tokio::time::timeout(DB_TIMEOUT, store::commit_github(db, &draft)).await {
        Ok(Ok(())) => Ok(true),
        Ok(Err(RoadmapError::Conflict | RoadmapError::Archived | RoadmapError::NotFound)) => {
            Ok(false)
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "GitHub roadmap sync: write failed");
            Err("roadmap write failed")
        }
        Err(_) => Err("roadmap write timed out"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    type Handler = Arc<dyn Fn(&str) -> String + Send + Sync>;

    /// A local HTTP/1.1 server: `handler` gets the request head (request
    /// line and headers) and returns the whole raw response. Every head is
    /// recorded.
    async fn server(handler: Handler) -> (Url, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let handler = handler.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0];
                    while !head.ends_with(b"\r\n\r\n") {
                        if socket.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let response = handler(&head);
                    log.lock().unwrap().push(head);
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (base, seen)
    }

    fn reply(status: &str, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n{body}",
            body.len()
        )
    }

    fn path(head: &str) -> &str {
        head.split(' ').nth(1).unwrap_or_default()
    }

    fn poller(base: Url) -> Poller {
        let mut token = HeaderValue::from_static("Bearer test-token-0123456789");
        token.set_sensitive(true);
        Poller::new(base, token, vec!["Owner/Repo".into()]).unwrap()
    }

    fn pr(number: u64) -> Target {
        Target::Pr("Owner/Repo".into(), number)
    }

    #[tokio::test]
    async fn etag_answers_are_reused_and_errors_are_typed() {
        let (base, seen) = server(Arc::new(|head: &str| {
            let lower = head.to_ascii_lowercase();
            match path(head) {
                "/etag" if lower.contains("if-none-match: \"v1\"") => {
                    reply("304 Not Modified", "etag: \"v1\"\r\n", "")
                }
                "/etag" => reply("200 OK", "etag: \"v1\"\r\n", r#"{"status":"behind"}"#),
                "/gone" => reply("404 Not Found", "", "{}"),
                "/deleted" => reply("410 Gone", "", "{}"),
                "/auth" => reply("401 Unauthorized", "", "{}"),
                "/limited" => reply(
                    "403 Forbidden",
                    "x-ratelimit-remaining: 0\r\nx-ratelimit-reset: 0\r\n",
                    "{}",
                ),
                "/slow-down" => reply("429 Too Many Requests", "retry-after: 120\r\n", "{}"),
                "/forbidden" => reply("403 Forbidden", "", "{}"),
                "/big" => reply("200 OK", "", &format!("\"{}\"", "a".repeat(MAX_BODY))),
                _ => reply("500 Internal Server Error", "", "{}"),
            }
        }))
        .await;
        let mut poller = poller(base.clone());
        let at = |p: &str| base.join(p).unwrap();
        for _ in 0..2 {
            let compare: Option<Compare> = poller.get(at("/etag"), true).await.unwrap();
            assert_eq!(compare.unwrap().status, "behind");
        }
        {
            let heads = seen.lock().unwrap();
            assert!(!heads[0].to_ascii_lowercase().contains("if-none-match"));
            assert!(heads[1]
                .to_ascii_lowercase()
                .contains("if-none-match: \"v1\""));
            // The token goes in the header, never in the request line.
            assert!(heads[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token-0123456789"));
            assert!(!heads[0].lines().next().unwrap().contains("test-token"));
        }
        assert_eq!(
            poller
                .get::<Compare>(at("/gone"), true)
                .await
                .unwrap()
                .map(|c| c.status),
            None
        );
        assert!(poller
            .get::<Compare>(at("/deleted"), true)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            poller.get::<Compare>(at("/auth"), true).await.err(),
            Some(PollError::Auth)
        );
        assert_eq!(
            poller.get::<Compare>(at("/limited"), true).await.err(),
            Some(PollError::RateLimited(Duration::from_secs(1)))
        );
        assert_eq!(
            poller.get::<Compare>(at("/slow-down"), true).await.err(),
            Some(PollError::RateLimited(Duration::from_secs(120)))
        );
        assert_eq!(
            poller.get::<Compare>(at("/forbidden"), true).await.err(),
            Some(PollError::Failed("GitHub refused access (403)"))
        );
        assert_eq!(
            poller.get::<String>(at("/big"), true).await.err(),
            Some(PollError::Failed(TOO_LARGE))
        );
        assert_eq!(
            poller.get::<Compare>(at("/other"), true).await.err(),
            Some(PollError::Failed("GitHub answered with an error"))
        );
    }

    #[tokio::test]
    async fn redirects_to_another_host_are_refused() {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let (elsewhere, _) = server(Arc::new(move |_: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            reply("200 OK", "", r#"{"status":"behind"}"#)
        }))
        .await;
        let location = elsewhere.join("/stolen").unwrap();
        let (base, _) = server(Arc::new(move |head: &str| match path(head) {
            "/moved" => reply("301 Moved Permanently", "location: /here\r\n", ""),
            "/here" => reply("200 OK", "", r#"{"status":"identical"}"#),
            _ => reply("302 Found", &format!("location: {location}\r\n"), ""),
        }))
        .await;
        let mut poller = poller(base.clone());
        let same: Option<Compare> = poller
            .get(base.join("/moved").unwrap(), false)
            .await
            .unwrap();
        assert_eq!(same.unwrap().status, "identical");
        assert_eq!(
            poller
                .get::<Compare>(base.join("/away").unwrap(), false)
                .await
                .err(),
            Some(PollError::Failed("GitHub redirect refused"))
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn states_come_from_the_lists_single_fetches_and_compares() {
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let (base, seen) = server(Arc::new(|head: &str| {
            let p = path(head);
            let ok = |body: String| reply("200 OK", "", &body);
            if p.starts_with("/repos/Owner/Repo/issues?") {
                return ok(r#"[
                    {"number":1,"state":"open","draft":true,"pull_request":{"merged_at":null}},
                    {"number":2,"state":"closed","pull_request":{"merged_at":"2026-11-01T10:00:00Z"}},
                    {"number":3,"state":"closed","pull_request":{"merged_at":null}},
                    {"number":4,"state":"closed","state_reason":"completed"},
                    {"number":5,"state":"closed","state_reason":"not_planned"},
                    {"number":6,"state":"open"}
                ]"#.into());
            }
            match p {
                "/repos/Owner/Repo/releases?per_page=10" => ok(r#"[
                    {"tag_name":"v2","draft":false,"prerelease":false,"published_at":"2026-11-02T10:00:00Z"},
                    {"tag_name":"v2-rc","draft":false,"prerelease":true,"published_at":"2026-11-01T12:00:00Z"}
                ]"#.into()),
                "/repos/Owner/Repo/pulls/2" => ok(format!(
                    r#"{{"state":"closed","draft":false,"merged_at":"2026-11-01T10:00:00Z","merge_commit_sha":"{SHA}"}}"#
                )),
                "/repos/Owner/Repo/pulls/9" => ok(r#"{"state":"open","draft":false}"#.into()),
                p if p == format!("/repos/Owner/Repo/compare/v2...{SHA}?per_page=1") => {
                    ok(r#"{"status":"behind"}"#.into())
                }
                _ => reply("404 Not Found", "", "{}"),
            }
        }))
        .await;
        let mut poller = poller(base);
        let release = |tag: &str| Target::Release("Owner/Repo".into(), tag.into());
        let issue = |n: u64| Target::Issue("Owner/Repo".into(), n);
        let cached: HashMap<Target, Option<String>> = [
            pr(1),
            pr(2),
            pr(3),
            issue(4),
            issue(5),
            issue(6),
            pr(9),
            pr(404),
            release("v2"),
            release("v2-rc"),
            release("v3"),
        ]
        .into_iter()
        .map(|target| (target, None))
        .collect();
        let (states, error, windows) = poller.states(&cached).await.unwrap();
        assert_eq!(error, None);
        assert_eq!(windows.len(), 1);
        // `pass` moves the window on once every write landed; here by hand.
        poller.since.extend(windows);
        let state = |target: &Target| states.get(target).map(String::as_str);
        assert_eq!(state(&pr(1)), Some("draft"));
        assert_eq!(state(&pr(2)), Some("released"));
        assert_eq!(state(&pr(3)), Some("closed"));
        assert_eq!(state(&issue(4)), Some("completed"));
        assert_eq!(state(&issue(5)), Some("closed"));
        assert_eq!(state(&issue(6)), Some("open"));
        assert_eq!(state(&pr(9)), Some("open"));
        assert_eq!(state(&pr(404)), Some("not_found"));
        assert_eq!(state(&release("v2")), Some("published"));
        assert_eq!(state(&release("v2-rc")), Some("prerelease"));
        assert_eq!(state(&release("v3")), Some("unreleased"));

        // The next pass asks only for what changed since this one, reuses the
        // merge commit and the compare, and fetches nothing it knows.
        let cached: HashMap<Target, Option<String>> = states
            .iter()
            .map(|(target, state)| (target.clone(), Some(state.clone())))
            .collect();
        let before = seen.lock().unwrap().len();
        poller.states(&cached).await.unwrap();
        let heads = seen.lock().unwrap();
        let paths: Vec<&str> = heads[before..].iter().map(|head| path(head)).collect();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(paths[0].contains("since="), "{paths:?}");
        assert!(paths[1].starts_with("/repos/Owner/Repo/releases"));
    }

    #[tokio::test]
    async fn a_list_too_large_to_read_falls_back_to_single_fetches() {
        let (base, _) = server(Arc::new(|head: &str| match path(head) {
            p if p.starts_with("/repos/Owner/Repo/issues?") => {
                reply("200 OK", "", &format!("\"{}\"", "a".repeat(MAX_BODY)))
            }
            "/repos/Owner/Repo/releases?per_page=10" => reply("200 OK", "", "[]"),
            "/repos/Owner/Repo/pulls/1" => reply("200 OK", "", r#"{"state":"open"}"#),
            _ => reply("404 Not Found", "", "{}"),
        }))
        .await;
        let mut poller = poller(base);
        let cached = HashMap::from([(pr(1), Some("draft".to_string()))]);
        let (states, error, windows) = poller.states(&cached).await.unwrap();
        assert_eq!(error, None);
        assert_eq!(states.get(&pr(1)).map(String::as_str), Some("open"));
        assert_eq!(windows.len(), 1);
    }

    #[tokio::test]
    async fn a_pass_where_every_repo_fails_backs_off() {
        let (base, _) = server(Arc::new(|_: &str| {
            reply("500 Internal Server Error", "", "{}")
        }))
        .await;
        let mut poller = poller(base);
        let cached = HashMap::from([(pr(1), None)]);
        let mut failures = 0;
        for delay in [POLL_INTERVAL * 2, POLL_INTERVAL * 4, MAX_BACKOFF] {
            let outcome = poller.states(&cached).await.map(|(_, error, _)| error);
            assert_eq!(
                outcome,
                Err(PollError::Failed("GitHub answered with an error"))
            );
            assert_eq!(
                settle(outcome, &mut failures),
                (
                    PollState::Error,
                    Some("GitHub answered with an error"),
                    delay
                )
            );
        }
        // One repo failing while another syncs is no reason to back off.
        let (base, _) = server(Arc::new(|head: &str| match path(head) {
            p if p.starts_with("/repos/Other/Repo/issues?") => reply("200 OK", "", "[]"),
            "/repos/Other/Repo/releases?per_page=10" => reply("200 OK", "", "[]"),
            "/repos/Other/Repo/pulls/1" => reply("200 OK", "", r#"{"state":"open"}"#),
            _ => reply("500 Internal Server Error", "", "{}"),
        }))
        .await;
        let mut token = HeaderValue::from_static("Bearer test-token-0123456789");
        token.set_sensitive(true);
        let repos = vec!["Owner/Repo".to_string(), "Other/Repo".to_string()];
        let mut poller = Poller::new(base, token, repos).unwrap();
        let cached = HashMap::from([(pr(1), None), (Target::Pr("Other/Repo".into(), 1), None)]);
        let outcome = poller.states(&cached).await.map(|(_, error, _)| error);
        assert_eq!(outcome, Ok(Some("GitHub answered with an error")));
        assert_eq!(settle(outcome, &mut failures).2, POLL_INTERVAL);
        assert_eq!(failures, 0);
    }

    #[test]
    fn links_that_disagree_on_a_target_are_fetched_again() {
        let now = OffsetDateTime::now_utc();
        let link = |state: Option<&str>| Link {
            kind: LinkKind::Pr,
            repo: Some("Owner/Repo".into()),
            number: Some(1),
            tag: None,
            url: None,
            state: state.map(str::to_string),
        };
        let item = |state: Option<&str>| crate::roadmaps::Item {
            id: uuid::Uuid::now_v7().to_string(),
            title: "Item".into(),
            status: crate::roadmaps::Status::Planned,
            auto_status: None,
            phase: None,
            position: 0,
            notes: None,
            depends_on: Vec::new(),
            links: vec![link(state)],
            updated_at: now,
        };
        let snapshot = |items| Snapshot {
            roadmap: crate::roadmaps::Roadmap {
                id: uuid::Uuid::now_v7().to_string(),
                title: "Plan".into(),
                description: None,
                phases: Vec::new(),
                revision: 1,
                created_at: now,
                updated_at: now,
                archived_at: None,
            },
            items,
        };
        let repos = ["Owner/Repo".to_string()];
        let same = [snapshot(vec![item(Some("open")), item(Some("open"))])];
        assert_eq!(
            cached_states(&same, &repos)[&pr(1)].as_deref(),
            Some("open")
        );
        let differ = [
            snapshot(vec![item(Some("open"))]),
            snapshot(vec![item(None)]),
        ];
        assert_eq!(cached_states(&differ, &repos)[&pr(1)], None);
    }

    #[test]
    fn only_allowlisted_github_links_are_targets() {
        let repos = vec!["Owner/Repo".to_string()];
        let link = |kind: LinkKind, repo: Option<&str>| Link {
            kind,
            repo: repo.map(str::to_string),
            number: Some(7),
            tag: Some("v1".into()),
            url: Some("https://api.github.com/repos/Owner/Repo/pulls/7".into()),
            state: None,
        };
        assert_eq!(
            target(&link(LinkKind::Pr, Some("owner/repo")), &repos),
            Some(pr(7))
        );
        assert_eq!(
            target(&link(LinkKind::Release, Some("Owner/Repo")), &repos),
            Some(Target::Release("Owner/Repo".into(), "v1".into()))
        );
        assert_eq!(target(&link(LinkKind::Url, None), &repos), None);
        assert_eq!(
            target(&link(LinkKind::Url, Some("Owner/Repo")), &repos),
            None
        );
        assert_eq!(
            target(&link(LinkKind::Issue, Some("Other/Repo")), &repos),
            None
        );
    }

    #[test]
    fn allowlist_and_token_configure_the_poller() {
        assert_eq!(
            parse_repos(" HawkeyNL/PersonalJarvis, a/b ,,hawkeynl/personaljarvis").unwrap(),
            ["HawkeyNL/PersonalJarvis", "a/b"]
        );
        assert!(parse_repos("").unwrap().is_empty());
        for bad in ["owner", "a/b/c", "a/b,https://github.com/a/b", "-a/b"] {
            assert!(parse_repos(bad).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..21).map(|n| format!("o/r{n}")).collect();
        assert!(parse_repos(&many.join(",")).is_err());

        let dir = tempfile::tempdir().unwrap();
        // No credentials directory, or no token in it: off, nothing to start.
        for credentials in [None, Some(dir.path())] {
            let (handle, token) = configure("a/b", credentials);
            assert!(token.is_none());
            assert!(!handle.configured());
            assert_eq!(handle.status().state, PollState::NotConfigured);
            assert_eq!(handle.request_sync(), Err(SyncRefused::NotConfigured));
        }
        let path = dir.path().join(TOKEN_CREDENTIAL);
        std::fs::write(&path, "not a token\n").unwrap();
        let (handle, token) = configure("a/b", Some(dir.path()));
        assert!(token.is_none());
        assert_eq!(
            handle.status().last_error,
            Some("GitHub token has an unexpected format")
        );
        std::fs::write(&path, "github_pat_0123456789abcdefABCDEF\n").unwrap();
        // A token without an allowlist stays off too.
        assert!(configure("", Some(dir.path())).1.is_none());
        let (handle, token) = configure("a/b", Some(dir.path()));
        let token = token.unwrap();
        assert!(token.is_sensitive());
        assert_eq!(token, "Bearer github_pat_0123456789abcdefABCDEF");
        assert_eq!(handle.status().state, PollState::Starting);
        assert_eq!(handle.request_sync(), Ok(()));
        assert_eq!(handle.request_sync(), Err(SyncRefused::TooSoon));
        // A poller that failed to start is never woken.
        let (handle, _) = configure("a/b", Some(dir.path()));
        handle.start_failed.store(true, Ordering::Release);
        assert_eq!(handle.request_sync(), Err(SyncRefused::NotRunning));
    }

    #[test]
    fn links_must_name_an_allowlisted_repo_once_it_is_set() {
        let link = |repo: Option<&str>| Link {
            kind: if repo.is_some() {
                LinkKind::Pr
            } else {
                LinkKind::Url
            },
            repo: repo.map(str::to_string),
            number: repo.map(|_| 1),
            tag: None,
            url: repo.is_none().then(|| "https://example.com".to_string()),
            state: None,
        };
        let open = GithubHandle::default();
        assert!(open.check_links(&[link(Some("x/y"))]).is_ok());
        let (set, _) = configure("a/b", None);
        assert!(set.check_links(&[link(Some("A/B")), link(None)]).is_ok());
        assert!(set.check_links(&[link(Some("x/y"))]).is_err());
    }

    #[test]
    fn rate_limit_waits_and_backoff_are_bounded() {
        let now = OffsetDateTime::from_unix_timestamp(1_000).unwrap();
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, HeaderValue::from_static(value));
            }
            map
        };
        let wait = |pairs: &[(&'static str, &'static str)]| rate_limit_wait(&headers(pairs), now);
        assert_eq!(
            wait(&[("retry-after", "30")]),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            wait(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1600")
            ]),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            wait(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "99999")
            ]),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(
            wait(&[
                ("x-ratelimit-remaining", "5"),
                ("x-ratelimit-reset", "1600")
            ]),
            None
        );
        assert_eq!(wait(&[]), None);
        assert_eq!(next_delay(0), POLL_INTERVAL);
        assert_eq!(next_delay(1), POLL_INTERVAL * 2);
        assert_eq!(next_delay(10), MAX_BACKOFF);
    }
}
