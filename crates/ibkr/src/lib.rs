//! Read-only IBKR adapter over the Client Portal Web API (see ADR-013).
//!
//! The interactive login (browser SSO + 2FA) happens in the IBKR Client Portal
//! Gateway; this client only performs read-only GETs against the gateway.
//! Broker-provided market values are surfaced as `f64` (display only) — our own
//! money math uses Decimal elsewhere.
//!
//! [`oauth`] is the token-only replacement: a first-party OAuth 1.0a client that
//! talks to `api.ibkr.com` directly, without the gateway. [`broker`] runs it
//! in a dedicated credential-holding process behind a local Unix socket.

use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub mod broker;
pub mod oauth;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for one gateway response body, enforced while reading.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Errors from the IBKR adapter. None of them carries a URL: the request path
/// contains the account id and must not reach logs or API responses.
#[derive(Debug, thiserror::Error)]
pub enum IbkrError {
    #[error("gateway base URL must be a credential-free https URL")]
    InvalidBaseUrl,
    #[error("IBKR request failed")]
    Http(#[source] reqwest::Error),
    #[error("IBKR response exceeds {MAX_BODY_BYTES} bytes")]
    TooLarge,
    #[error("IBKR response is not valid JSON")]
    Decode(#[source] serde_json::Error),
    #[error("IBKR session is not authenticated")]
    NotAuthenticated,
    #[error("no account available")]
    NoAccount,
    #[error("IBKR OAuth credential is invalid: {0}")]
    Credential(&'static str),
    #[error("IBKR OAuth handshake failed: {0}")]
    Handshake(&'static str),
    #[error("IBKR rejected the request as unauthorized")]
    Unauthorized,
    #[error("IBKR rate limit hit; retry in {retry_after_secs} s")]
    RateLimited { retry_after_secs: u64 },
    #[error("too many queued IBKR requests")]
    Busy,
    #[error("IBKR session token unavailable; retry in {retry_after_secs} s")]
    MintCooldown { retry_after_secs: u64 },
    #[error("invalid IBKR request parameter: {0}")]
    InvalidInput(&'static str),
}

impl From<reqwest::Error> for IbkrError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}

/// Session/authentication status of the gateway.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AuthStatus {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub competing: bool,
}

/// An IBKR account visible in the current session.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "accountTitle", default)]
    pub title: Option<String>,
}

/// A single position (read-only; broker-provided values).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Position {
    #[serde(default)]
    pub conid: i64,
    #[serde(rename = "contractDesc", default)]
    pub symbol: String,
    #[serde(default)]
    pub position: f64,
    #[serde(rename = "avgCost", default)]
    pub avg_cost: f64,
    #[serde(rename = "mktPrice", default)]
    pub mkt_price: f64,
    #[serde(rename = "mktValue", default)]
    pub mkt_value: f64,
    #[serde(default)]
    pub currency: String,
}

/// Read-only client for the IBKR Client Portal Web API. Build it once and
/// share it, so requests reuse the connection pool.
pub struct IbkrClient {
    base: String,
    http: reqwest::Client,
}

impl IbkrClient {
    /// Create a client for the given gateway base URL (e.g.
    /// `https://localhost:5000/v1/api`). Only https is accepted and the
    /// gateway certificate is verified.
    pub fn new(base_url: &str) -> Result<Self, IbkrError> {
        let parsed = reqwest::Url::parse(base_url).map_err(|_| IbkrError::InvalidBaseUrl)?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(IbkrError::InvalidBaseUrl);
        }
        let http = hardened_http().build()?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Session/auth status. Fails if the gateway is unreachable.
    pub async fn auth_status(&self) -> Result<AuthStatus, IbkrError> {
        self.get_json("/iserver/auth/status").await
    }

    /// Accounts visible in the current session.
    pub async fn accounts(&self) -> Result<Vec<Account>, IbkrError> {
        self.get_json("/portfolio/accounts").await
    }

    /// Positions for an account (first page).
    pub async fn positions(&self, account_id: &str) -> Result<Vec<Position>, IbkrError> {
        self.get_json(&format!("/portfolio/{account_id}/positions/0"))
            .await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, IbkrError> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await?
            .error_for_status()?;
        let body = read_capped(response).await?;
        serde_json::from_slice(&body).map_err(IbkrError::Decode)
    }
}

/// HTTP client builder shared by both IBKR clients: https only, verified TLS
/// (the rustls default) and bounded connect/request time.
fn hardened_http() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
}

/// Read a response body, refusing more than [`MAX_BODY_BYTES`] while reading.
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, IbkrError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(IbkrError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(IbkrError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Plain-HTTP keep-alive server answering every request with `response`.
    /// Returns the base URL and the number of accepted connections.
    async fn serve(response: Vec<u8>) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/api", listener.local_addr().unwrap());
        let response = Arc::new(response);
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let response = response.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while let Ok(n @ 1..) = socket.read(&mut buf).await {
                        request.extend_from_slice(&buf[..n]);
                        if request.ends_with(b"\r\n\r\n") {
                            request.clear();
                            if socket.write_all(&response).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        (base, accepted)
    }

    /// A client over plain HTTP, so the local test server needs no TLS.
    fn plain_client(base: String) -> IbkrClient {
        IbkrClient {
            base,
            http: reqwest::Client::new(),
        }
    }

    fn ok_json(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn refuses_non_https_or_credentialed_base_urls() {
        for base in [
            "http://localhost:5000/v1/api",
            "ftp://localhost/v1/api",
            "localhost:5000/v1/api",
            "not a url",
            "https://user:pass@localhost:5000/v1/api",
            "https://localhost:5000/v1/api?x=1",
            "https://localhost:5000/v1/api#x",
        ] {
            assert!(
                matches!(IbkrClient::new(base), Err(IbkrError::InvalidBaseUrl)),
                "{base}"
            );
        }
        let client = IbkrClient::new("https://localhost:5000/v1/api/").unwrap();
        assert_eq!(client.base, "https://localhost:5000/v1/api");
    }

    #[tokio::test]
    async fn hanging_gateway_times_out_without_leaking_the_url() {
        // Accepts TCP but never answers the TLS handshake.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let client = IbkrClient::new(&format!("https://127.0.0.1:{port}/v1/api")).unwrap();
        let started = std::time::Instant::now();
        let error = client.positions("DU7654321").await.unwrap_err();
        assert!(started.elapsed() < CONNECT_TIMEOUT + Duration::from_secs(3));
        assert!(
            matches!(&error, IbkrError::Http(e) if e.is_timeout()),
            "{error:?}"
        );
        let text = format!("{error} {error:?}");
        for leaked in ["127.0.0.1", &port.to_string(), "DU7654321", "positions"] {
            assert!(!text.contains(leaked), "{text}");
        }
    }

    #[tokio::test]
    async fn status_errors_do_not_leak_the_url() {
        let (base, _) =
            serve(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n".to_vec())
                .await;
        let error = plain_client(base).positions("DU7654321").await.unwrap_err();
        let text = format!("{error} {error:?}");
        assert!(
            matches!(&error, IbkrError::Http(e) if e.is_status()),
            "{text}"
        );
        for leaked in ["127.0.0.1", "DU7654321", "positions"] {
            assert!(!text.contains(leaked), "{text}");
        }
    }

    #[tokio::test]
    async fn response_bodies_are_capped_while_reading() {
        // Declared too large: refused before reading.
        let declared = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let (base, _) = serve(declared.into_bytes()).await;
        let error = plain_client(base).auth_status().await.unwrap_err();
        assert!(matches!(error, IbkrError::TooLarge), "{error:?}");

        // Undeclared (chunked) length: refused once the cap is crossed.
        let mut chunked = format!(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_BODY_BYTES + 1
        )
        .into_bytes();
        chunked.resize(chunked.len() + MAX_BODY_BYTES + 1, b' ');
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let (base, _) = serve(chunked).await;
        let error = plain_client(base).auth_status().await.unwrap_err();
        assert!(matches!(error, IbkrError::TooLarge), "{error:?}");

        let (base, _) = serve(ok_json(r#"{"authenticated":true}"#)).await;
        assert!(
            plain_client(base)
                .auth_status()
                .await
                .unwrap()
                .authenticated
        );
    }

    #[tokio::test]
    async fn one_shared_client_reuses_its_connection() {
        let (base, accepted) = serve(ok_json("[]")).await;
        let client = Arc::new(plain_client(base));
        for shared in [client.clone(), client] {
            assert!(shared.accounts().await.unwrap().is_empty());
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parses_auth_status() {
        let json = r#"{"authenticated":true,"competing":false,"connected":true,"MAC":"..."}"#;
        let status: AuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.authenticated);
        assert!(status.connected);
    }

    #[test]
    fn parses_accounts() {
        let json = r#"[{"id":"DU1234567","accountId":"DU1234567","accountTitle":"Paper"}]"#;
        let accounts: Vec<Account> = serde_json::from_str(json).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].account_id, "DU1234567");
        assert_eq!(accounts[0].title.as_deref(), Some("Paper"));
    }

    #[test]
    fn parses_positions() {
        let json = r#"[{"conid":265598,"contractDesc":"AAPL","position":12.0,"avgCost":178.5,"mktPrice":190.1,"mktValue":2281.2,"currency":"USD"}]"#;
        let positions: Vec<Position> = serde_json::from_str(json).unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "AAPL");
        assert_eq!(positions[0].conid, 265598);
        assert_eq!(positions[0].currency, "USD");
    }
}
