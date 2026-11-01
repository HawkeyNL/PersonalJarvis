//! Read-only IBKR adapter over the IBKR Web API.
//!
//! [`oauth`] is a first-party OAuth 1.0a client that talks to `api.ibkr.com`
//! directly. [`broker`] runs it in a dedicated credential-holding process
//! behind a local Unix socket; Core only ever talks to that socket.

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod broker;
pub mod oauth;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for one IBKR response body, enforced while reading.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Errors from the IBKR adapter. None of them carries a URL: the request path
/// contains the account id and must not reach logs or API responses.
#[derive(Debug, thiserror::Error)]
pub enum IbkrError {
    #[error("IBKR request failed")]
    Http(#[source] reqwest::Error),
    #[error("IBKR response exceeds {MAX_BODY_BYTES} bytes")]
    TooLarge,
    #[error("IBKR response is not valid JSON")]
    Decode(#[source] serde_json::Error),
    #[error("IBKR session is not authenticated")]
    NotAuthenticated,
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

/// Session/authentication status reported by IBKR.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AuthStatus {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub competing: bool,
}

/// HTTP client builder for the IBKR client: https only, verified TLS
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Plain-HTTP server answering every request with `response`.
    async fn serve(response: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/portfolio/DU7654321/positions/0",
            listener.local_addr().unwrap()
        );
        let response = std::sync::Arc::new(response);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let response = response.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 1024];
                    while let Ok(n @ 1..) = socket.read(&mut buf).await {
                        request.extend_from_slice(&buf[..n]);
                        if request.ends_with(b"\r\n\r\n") {
                            let _ = socket.write_all(&response).await;
                            return;
                        }
                    }
                });
            }
        });
        url
    }

    /// One GET read the way the OAuth client reads (plain HTTP for the test).
    async fn get(url: &str) -> Result<Vec<u8>, IbkrError> {
        let response = reqwest::Client::new().get(url).send().await?;
        read_capped(response.error_for_status()?).await
    }

    #[tokio::test]
    async fn hanging_upstream_times_out_without_leaking_the_url() {
        // Accepts TCP but never answers the TLS handshake.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let http = hardened_http().build().unwrap();
        let started = std::time::Instant::now();
        let error: IbkrError = http
            .get(format!(
                "https://127.0.0.1:{port}/portfolio/DU7654321/positions/0"
            ))
            .send()
            .await
            .unwrap_err()
            .into();
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
        let url =
            serve(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n".to_vec())
                .await;
        let error = get(&url).await.unwrap_err();
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
        let error = get(&serve(declared.into_bytes()).await).await.unwrap_err();
        assert!(matches!(error, IbkrError::TooLarge), "{error:?}");

        // Undeclared (chunked) length: refused once the cap is crossed.
        let mut chunked = format!(
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n",
            MAX_BODY_BYTES + 1
        )
        .into_bytes();
        chunked.resize(chunked.len() + MAX_BODY_BYTES + 1, b' ');
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let error = get(&serve(chunked).await).await.unwrap_err();
        assert!(matches!(error, IbkrError::TooLarge), "{error:?}");

        let ok = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n[]".to_vec();
        assert_eq!(get(&serve(ok).await).await.unwrap(), b"[]");
    }

    #[test]
    fn parses_auth_status() {
        let json = r#"{"authenticated":true,"competing":false,"connected":true,"MAC":"..."}"#;
        let status: AuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.authenticated);
        assert!(status.connected);
    }
}
