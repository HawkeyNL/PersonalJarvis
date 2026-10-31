//! First-party OAuth 1.0a client for the IBKR Web API (read-only).
//!
//! Flow: a live session token (LST) is minted with a Diffie-Hellman exchange
//! over an RSA-SHA256 signed request, validated against IBKR's signature, then
//! every request is signed with HMAC-SHA256 keyed by the LST. `/iserver/*`
//! calls additionally need a brokerage session (`ssodh/init`), which the
//! client opens on demand. Paper accounts use the same host with paper
//! credentials.
//!
//! The public API only exposes fixed read endpoints: there is no generic
//! request method, and no order, reply, what-if, cancel or account-switch call.
//!
//! Expected setup: the portal shows the access token secret RSA-encrypted
//! (base64). It is decrypted once, at credential setup and outside Core, e.g.
//! `base64 -d secret.b64 | openssl pkeyutl -decrypt -inkey
//! private_encryption.pem | xxd -p -c0`, and only that lowercase hex (the
//! base-string "prepend") is stored in a root-only file. Core never loads the
//! encryption key and does no RSA decryption at runtime.

mod crypto;
mod types;

use std::{
    path::PathBuf,
    sync::Mutex as StdMutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use num_bigint::BigUint;
use reqwest::{header, Method, StatusCode};
use ring::signature::RsaKeyPair;
use serde::{de::DeserializeOwned, Deserialize};
use tokio::{sync::Mutex, time::Instant};
use zeroize::Zeroizing;

pub use types::{
    Account, Bar, History, Ledger, LedgerEntry, MarketDataAvailability, Position, SecdefMatch,
    SecdefSection, Snapshot,
};

use crate::{AuthStatus, IbkrError};

const BASE_URL: &str = "https://api.ibkr.com/v1/api";
const USER_AGENT: &str = concat!("jarvis-ibkr/", env!("CARGO_PKG_VERSION"));
/// Re-mint the LST this long before IBKR's stated expiry.
const LST_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
const GLOBAL_GAP: Duration = Duration::from_millis(100);
const ACCOUNTS_GAP: Duration = Duration::from_secs(5);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(10 * 60);
/// At most one LST mint per this interval, so a failing handshake or a
/// signature IBKR keeps rejecting cannot turn into a mint storm.
const MINT_COOLDOWN: Duration = Duration::from_secs(30);
/// Upper bound on the server-stated LST lifetime (IBKR issues about 24 h).
const MAX_LST_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Refuse to queue a request that would wait longer than this for its slot.
const MAX_QUEUE_WAIT: Duration = Duration::from_secs(30);
const SNAPSHOT_PREFLIGHT_DELAY: Duration = Duration::from_secs(1);
const MAX_SNAPSHOT_CONIDS: usize = 50;
/// 100 positions per page, so this bounds one call at 5000 positions.
const MAX_POSITION_PAGES: u32 = 50;
// ponytail: compete=false never kicks the owner's TWS/mobile session, but the
// init may then report "not authenticated" while another session is active.
const SSODH_INIT_BODY: &str = r#"{"publish":true,"compete":false}"#;

/// Paths of the credential files issued through the IBKR OAuth self-service
/// portal. Each file holds exactly one value (PEMs as written by openssl).
#[derive(Debug, Clone)]
pub struct OAuthCredentialFiles {
    pub consumer_key: PathBuf,
    pub access_token: PathBuf,
    /// The decrypted access token secret as hex (see the module docs).
    pub access_token_secret_prepend_hex: PathBuf,
    /// RSA private signature key, PKCS#8 or PKCS#1 PEM.
    pub signature_key: PathBuf,
    pub dh_param: PathBuf,
}

/// Parsed OAuth credentials. Secret material is zeroized on drop and never
/// shown by `Debug`.
pub struct OAuthCredentials {
    consumer_key: Zeroizing<String>,
    access_token: Zeroizing<String>,
    /// Decrypted access token secret.
    secret: Zeroizing<Vec<u8>>,
    signing_key: RsaKeyPair,
    dh_prime: BigUint,
    realm: String,
}

impl std::fmt::Debug for OAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredentials")
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}

fn read_secret(path: &PathBuf, what: &'static str) -> Result<Zeroizing<String>, IbkrError> {
    std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|_| IbkrError::Credential(what))
}

/// PKCS#8 (`BEGIN PRIVATE KEY`, OpenSSL 3) or PKCS#1 (`BEGIN RSA PRIVATE
/// KEY`, OpenSSL 1.x) RSA key; ring requires 2048 to 8192 bits.
fn signing_key(pem: &str) -> Result<RsaKeyPair, IbkrError> {
    let key = if let Some(der) = crypto::pem_body(pem, "PRIVATE KEY") {
        RsaKeyPair::from_pkcs8(&der)
    } else if let Some(der) = crypto::pem_body(pem, "RSA PRIVATE KEY") {
        RsaKeyPair::from_der(&der)
    } else {
        return Err(IbkrError::Credential("signature key"));
    };
    key.map_err(|_| IbkrError::Credential("signature key"))
}

fn is_token(value: &str, max: usize, allowed: impl Fn(char) -> bool) -> bool {
    (1..=max).contains(&value.len()) && value.chars().all(allowed)
}

impl OAuthCredentials {
    /// Load and validate all credential files. `realm` is `limited_poa` for individual accounts (live and
    /// paper). Blocking file I/O: call once at startup or via `spawn_blocking`.
    pub fn load(files: &OAuthCredentialFiles, realm: &str) -> Result<Self, IbkrError> {
        if !is_token(realm, 32, |c| c.is_ascii_lowercase() || c == '_') {
            return Err(IbkrError::Credential("realm"));
        }
        let consumer_key = read_secret(&files.consumer_key, "consumer key")?;
        let consumer_key = Zeroizing::new(consumer_key.trim().to_string());
        // The portal stores the key uppercased; a lowercase key fails at IBKR.
        if !is_token(&consumer_key, 32, |c| {
            c.is_ascii_uppercase() || c.is_ascii_digit()
        }) {
            return Err(IbkrError::Credential("consumer key"));
        }
        let access_token = read_secret(&files.access_token, "access token")?;
        let access_token = Zeroizing::new(access_token.trim().to_string());
        if !is_token(&access_token, 128, |c| c.is_ascii_alphanumeric()) {
            return Err(IbkrError::Credential("access token"));
        }
        const PREPEND: &str = "access token secret prepend";
        let prepend = read_secret(&files.access_token_secret_prepend_hex, PREPEND)?;
        let secret = Zeroizing::new(
            hex::decode(prepend.trim()).map_err(|_| IbkrError::Credential(PREPEND))?,
        );
        if secret.is_empty() {
            return Err(IbkrError::Credential(PREPEND));
        }
        let signing_key = signing_key(&read_secret(&files.signature_key, "signature key")?)?;
        let dh_prime = crypto::dh_prime(&read_secret(&files.dh_param, "DH parameters")?)?;
        Ok(Self {
            consumer_key,
            access_token,
            secret,
            signing_key,
            dh_prime,
            realm: realm.to_string(),
        })
    }
}

/// Spaces requests out and enforces the back-off after a 429. Slots are
/// reserved under a short synchronous lock; the wait happens outside it.
// ponytail: one global gap; per-endpoint limits beyond /portfolio/accounts
// (e.g. the 5 concurrent history requests) are not modelled.
struct Pacer {
    state: StdMutex<PacerState>,
}

struct PacerState {
    next: Instant,
    accounts_next: Instant,
    blocked_until: Option<Instant>,
}

impl Pacer {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            state: StdMutex::new(PacerState {
                next: now,
                accounts_next: now,
                blocked_until: None,
            }),
        }
    }

    fn check(state: &PacerState, now: Instant) -> Result<(), IbkrError> {
        match state.blocked_until.filter(|until| *until > now) {
            Some(until) => Err(IbkrError::RateLimited {
                retry_after_secs: (until - now).as_secs().max(1),
            }),
            None => Ok(()),
        }
    }

    /// Fails while a 429 back-off is active; checked again after waiting.
    fn ensure_open(&self) -> Result<(), IbkrError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::check(&state, Instant::now())
    }

    fn reserve(&self, accounts: bool) -> Result<Instant, IbkrError> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::check(&state, now)?;
        let mut at = state.next.max(now);
        if accounts {
            at = at.max(state.accounts_next);
        }
        if at - now > MAX_QUEUE_WAIT {
            return Err(IbkrError::Busy);
        }
        if accounts {
            state.accounts_next = at + ACCOUNTS_GAP;
        }
        state.next = at + GLOBAL_GAP;
        Ok(at)
    }

    fn block(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.blocked_until = Some(Instant::now() + RATE_LIMIT_BACKOFF);
    }
}

struct Session {
    lst: Zeroizing<Vec<u8>>,
    refresh_at: Instant,
    generation: u64,
}

#[derive(Default)]
struct SessionState {
    current: Option<Session>,
    generation: u64,
    last_mint: Option<Instant>,
}

#[derive(Deserialize)]
struct LstResponse {
    diffie_hellman_response: String,
    live_session_token_signature: String,
    live_session_token_expiration: i64,
}

#[derive(Deserialize)]
struct Tickle {
    #[serde(default)]
    iserver: Option<TickleIserver>,
}

#[derive(Deserialize)]
struct TickleIserver {
    #[serde(rename = "authStatus", default)]
    auth_status: AuthStatus,
}

fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn valid_account(account_id: &str) -> Result<&str, IbkrError> {
    if is_token(account_id, 32, |c| c.is_ascii_alphanumeric()) {
        Ok(account_id)
    } else {
        Err(IbkrError::InvalidInput("account id"))
    }
}

fn valid_conid(conid: i64) -> Result<i64, IbkrError> {
    if conid > 0 {
        Ok(conid)
    } else {
        Err(IbkrError::InvalidInput("conid"))
    }
}

/// `<1-4 digits><unit>`, e.g. `1y`, `30min`.
fn valid_span<'a>(
    value: &'a str,
    units: &[&str],
    what: &'static str,
) -> Result<&'a str, IbkrError> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let unit = &value[digits..];
    if (1..=4).contains(&digits) && !value.starts_with('0') && units.contains(&unit) {
        Ok(value)
    } else {
        Err(IbkrError::InvalidInput(what))
    }
}

/// Read-only IBKR Web API client authenticated with first-party OAuth 1.0a.
/// Build it once and share it: it caches the LST and paces all requests.
pub struct IbkrOAuthClient {
    base: String,
    http: reqwest::Client,
    credentials: OAuthCredentials,
    /// Single-flight LST cache: minting happens while this lock is held.
    session: Mutex<SessionState>,
    /// LST generation the brokerage session was opened for (0 = none).
    brokerage: Mutex<u64>,
    pacer: Pacer,
}

impl IbkrOAuthClient {
    pub fn new(credentials: OAuthCredentials) -> Result<Self, IbkrError> {
        let http = crate::hardened_http()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self::with_parts(BASE_URL.to_string(), http, credentials))
    }

    fn with_parts(base: String, http: reqwest::Client, credentials: OAuthCredentials) -> Self {
        Self {
            base,
            http,
            credentials,
            session: Mutex::new(SessionState::default()),
            brokerage: Mutex::new(0),
            pacer: Pacer::new(),
        }
    }

    /// Accounts visible to these credentials. IBKR wants this called before
    /// other `/portfolio/{account}` requests; it is paced to one per 5 s.
    pub async fn accounts(&self) -> Result<Vec<Account>, IbkrError> {
        self.get_json("/portfolio/accounts", &[], true).await
    }

    /// All positions of an account, reading pages until an empty one.
    pub async fn positions(&self, account_id: &str) -> Result<Vec<Position>, IbkrError> {
        let account_id = valid_account(account_id)?;
        let mut all = Vec::new();
        for page in 0..MAX_POSITION_PAGES {
            let batch: Vec<Position> = self
                .get_json(
                    &format!("/portfolio/{account_id}/positions/{page}"),
                    &[],
                    false,
                )
                .await?;
            if batch.is_empty() {
                return Ok(all);
            }
            all.extend(batch);
        }
        Err(IbkrError::TooLarge)
    }

    /// Cash and value per currency (`BASE` is the account base currency).
    pub async fn ledger(&self, account_id: &str) -> Result<Ledger, IbkrError> {
        let account_id = valid_account(account_id)?;
        self.get_json(&format!("/portfolio/{account_id}/ledger"), &[], false)
            .await
    }

    /// Contracts matching a ticker symbol.
    pub async fn search(&self, symbol: &str) -> Result<Vec<SecdefMatch>, IbkrError> {
        if !is_token(symbol, 16, |c| {
            c.is_ascii_alphanumeric() || c == '.' || c == '-'
        }) {
            return Err(IbkrError::InvalidInput("symbol"));
        }
        self.ensure_brokerage().await?;
        self.get_json(
            "/iserver/secdef/search",
            &[("symbol", symbol.to_string())],
            false,
        )
        .await
    }

    /// Market data snapshot for up to 50 contracts. The first request for a
    /// contract only opens the subscription, so it is repeated once after a
    /// short delay when any contract came back without data.
    pub async fn snapshot(&self, conids: &[i64]) -> Result<Vec<Snapshot>, IbkrError> {
        if conids.is_empty() || conids.len() > MAX_SNAPSHOT_CONIDS {
            return Err(IbkrError::InvalidInput("conids"));
        }
        let list = conids
            .iter()
            .map(|conid| valid_conid(*conid).map(|c| c.to_string()))
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        let query = [
            ("conids", list),
            ("fields", types::SNAPSHOT_FIELDS.to_string()),
        ];
        self.ensure_brokerage().await?;
        let path = "/iserver/marketdata/snapshot";
        let mut raw: Vec<types::RawSnapshot> = self.get_json(path, &query, false).await?;
        let ready = |raw: &[types::RawSnapshot]| {
            conids
                .iter()
                .all(|c| raw.iter().any(|r| r.conid == *c && r.has_data()))
        };
        if !ready(&raw) {
            tokio::time::sleep(SNAPSHOT_PREFLIGHT_DELAY).await;
            raw = self.get_json(path, &query, false).await?;
        }
        Ok(raw
            .into_iter()
            .map(types::RawSnapshot::into_snapshot)
            .collect())
    }

    /// Historical bars, regular trading hours only. `period` like `1y`, `6m`,
    /// `30d`; `bar` like `1d`, `1h`, `5min`.
    pub async fn history(&self, conid: i64, period: &str, bar: &str) -> Result<History, IbkrError> {
        let conid = valid_conid(conid)?;
        let period = valid_span(period, &["min", "h", "d", "w", "m", "y"], "period")?;
        let bar = valid_span(bar, &["min", "h", "d", "w", "m"], "bar")?;
        self.ensure_brokerage().await?;
        self.get_json(
            "/iserver/marketdata/history",
            &[
                ("bar", bar.to_string()),
                ("conid", conid.to_string()),
                ("outsideRth", "false".to_string()),
                ("period", period.to_string()),
            ],
            false,
        )
        .await
    }

    /// Keep the brokerage session alive; call about once a minute. When the
    /// session is gone the next `/iserver` call re-opens it.
    pub async fn tickle(&self) -> Result<AuthStatus, IbkrError> {
        let tickle: Tickle = serde_json::from_slice(
            &self
                .signed(Method::POST, "/tickle", &[], None, false)
                .await?,
        )
        .map_err(IbkrError::Decode)?;
        let status = tickle.iserver.map(|i| i.auth_status).unwrap_or_default();
        if !status.authenticated {
            *self.brokerage.lock().await = 0;
        }
        Ok(status)
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        accounts: bool,
    ) -> Result<T, IbkrError> {
        let body = self
            .signed(Method::GET, path, query, None, accounts)
            .await?;
        serde_json::from_slice(&body).map_err(IbkrError::Decode)
    }

    /// Open the brokerage session for the current LST if not yet done:
    /// `ssodh/init`, then `/iserver/accounts` (required before market data).
    async fn ensure_brokerage(&self) -> Result<(), IbkrError> {
        let mut opened_for = self.brokerage.lock().await;
        let generation = self.session().await?.1;
        if *opened_for == generation {
            return Ok(());
        }
        let init = self
            .signed(
                Method::POST,
                "/iserver/auth/ssodh/init",
                &[],
                Some(SSODH_INIT_BODY),
                false,
            )
            .await?;
        let status: AuthStatus = serde_json::from_slice(&init).map_err(IbkrError::Decode)?;
        if !status.authenticated {
            return Err(IbkrError::NotAuthenticated);
        }
        self.signed(Method::GET, "/iserver/accounts", &[], None, false)
            .await?;
        *opened_for = generation;
        Ok(())
    }

    /// The current LST bytes and generation, minting a new one when absent or
    /// close to expiry. Concurrent callers wait for a single mint.
    async fn session(&self) -> Result<(Zeroizing<Vec<u8>>, u64), IbkrError> {
        let mut state = self.session.lock().await;
        let now = Instant::now();
        if let Some(current) = state.current.as_ref().filter(|s| now < s.refresh_at) {
            return Ok((current.lst.clone(), current.generation));
        }
        if let Some(next) = state.last_mint.map(|at| at + MINT_COOLDOWN) {
            if now < next {
                return Err(IbkrError::MintCooldown {
                    retry_after_secs: (next - now).as_secs().max(1),
                });
            }
        }
        state.last_mint = Some(now);
        state.current = None;
        let (lst, refresh_at) = self.mint().await?;
        state.generation += 1;
        let generation = state.generation;
        let current = state.current.insert(Session {
            lst,
            refresh_at,
            generation,
        });
        Ok((current.lst.clone(), current.generation))
    }

    async fn mint(&self) -> Result<(Zeroizing<Vec<u8>>, Instant), IbkrError> {
        let creds = &self.credentials;
        let a = crypto::dh_private();
        let challenge = BigUint::from(2u8)
            .modpow(&a, &creds.dh_prime)
            .to_str_radix(16);
        let url = format!("{}/oauth/live_session_token", self.base);
        let nonce = crypto::random_hex(16);
        let timestamp = unix_now().as_secs().to_string();
        let mut params = vec![
            ("diffie_hellman_challenge", challenge.as_str()),
            ("oauth_consumer_key", creds.consumer_key.as_str()),
            ("oauth_nonce", nonce.as_str()),
            ("oauth_signature_method", "RSA-SHA256"),
            ("oauth_timestamp", timestamp.as_str()),
            ("oauth_token", creds.access_token.as_str()),
        ];
        let prepend = Zeroizing::new(hex::encode(&*creds.secret));
        let base = Zeroizing::new(crypto::base_string(&prepend, "POST", &url, &params));
        let signature = crypto::rsa_sha256_signature(&creds.signing_key, &base)?;
        params.push(("oauth_signature", &signature));
        let authorization = crypto::authorization_header(&creds.realm, &params);
        let request = self
            .http
            .post(url)
            .header(header::AUTHORIZATION, authorization)
            // hyper omits it for an empty body; IBKR then answers 411.
            .header(header::CONTENT_LENGTH, "0")
            .body("");
        let response: LstResponse = serde_json::from_slice(&self.execute(request, false).await?)
            .map_err(IbkrError::Decode)?;

        let lst = crypto::live_session_token(
            &response.diffie_hellman_response,
            &a,
            &creds.dh_prime,
            &creds.secret,
        )?;
        if !crypto::lst_signature_matches(
            &lst,
            &creds.consumer_key,
            &response.live_session_token_signature,
        ) {
            return Err(IbkrError::Handshake(
                "live session token signature mismatch",
            ));
        }
        let lifetime = u64::try_from(response.live_session_token_expiration)
            .ok()
            .and_then(|ms| Duration::from_millis(ms).checked_sub(unix_now()))
            .and_then(|left| left.min(MAX_LST_LIFETIME).checked_sub(LST_REFRESH_MARGIN))
            .ok_or(IbkrError::Handshake("live session token expires too soon"))?;
        Ok((lst, Instant::now() + lifetime))
    }

    /// Sign and send a request with the current LST. A 401 expires the LST it
    /// was signed with, so the next call mints a fresh one.
    async fn signed(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        json_body: Option<&'static str>,
        accounts: bool,
    ) -> Result<Vec<u8>, IbkrError> {
        let (lst, generation) = self.session().await?;
        let creds = &self.credentials;
        let url = format!("{}{path}", self.base);
        let nonce = crypto::random_hex(16);
        let timestamp = unix_now().as_secs().to_string();
        // Query values are validated to digits, letters, `.`, `-` and `,`, so
        // the URL carries them verbatim (commas are legal in a query) and the
        // base string encodes them once, like IBKR's example. Including them
        // in the base string follows IBKR's documentation; unverified live.
        let mut params: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        params.extend([
            ("oauth_consumer_key", creds.consumer_key.as_str()),
            ("oauth_nonce", nonce.as_str()),
            ("oauth_signature_method", "HMAC-SHA256"),
            ("oauth_timestamp", timestamp.as_str()),
            ("oauth_token", creds.access_token.as_str()),
        ]);
        let base = crypto::base_string("", method.as_str(), &url, &params);
        let signature = crypto::hmac_sha256_signature(&lst, &base);
        params.retain(|(key, _)| key.starts_with("oauth_"));
        params.push(("oauth_signature", &signature));
        let authorization = crypto::authorization_header(&creds.realm, &params);

        let full_url = if query.is_empty() {
            url
        } else {
            let query: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{url}?{}", query.join("&"))
        };
        let is_post = method == Method::POST;
        let mut request = self
            .http
            .request(method, full_url)
            .header(header::AUTHORIZATION, authorization);
        request = match json_body {
            Some(body) => request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body),
            None if is_post => request.header(header::CONTENT_LENGTH, "0").body(""),
            None => request,
        };
        let result = self.execute(request, accounts).await;
        if matches!(result, Err(IbkrError::Unauthorized)) {
            // Only the LST this request used: a newer one minted meanwhile
            // stays. The next mint gets a new generation, so the brokerage
            // session is re-opened for it.
            let mut state = self.session.lock().await;
            if let Some(session) = state
                .current
                .as_mut()
                .filter(|s| s.generation == generation)
            {
                session.refresh_at = Instant::now();
            }
        }
        result
    }

    async fn execute(
        &self,
        request: reqwest::RequestBuilder,
        accounts: bool,
    ) -> Result<Vec<u8>, IbkrError> {
        tokio::time::sleep_until(self.pacer.reserve(accounts)?).await;
        self.pacer.ensure_open()?;
        let response = request.send().await?;
        match response.status() {
            StatusCode::TOO_MANY_REQUESTS => {
                self.pacer.block();
                Err(IbkrError::RateLimited {
                    retry_after_secs: RATE_LIMIT_BACKOFF.as_secs(),
                })
            }
            StatusCode::UNAUTHORIZED => Err(IbkrError::Unauthorized),
            _ => crate::read_capped(response.error_for_status()?).await,
        }
    }
}

#[cfg(test)]
mod tests;
