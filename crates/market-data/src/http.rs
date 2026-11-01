//! Hardened HTTP plumbing shared by every client in this crate.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{header, StatusCode, Url};
use serde::de::DeserializeOwned;

use crate::{MarketDataError, Provider, ALLOWED_HOSTS};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound for one response body, enforced while reading.
pub(crate) const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// A request that would wait longer than this for its pacer slot fails with
/// [`MarketDataError::Busy`] instead of sleeping. Above the largest provider
/// gap, so one queued request always fits.
const MAX_QUEUE_WAIT: Duration = Duration::from_secs(5);
/// Back-off after a 429 without a usable `Retry-After`.
pub(crate) const DEFAULT_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Parse `url` and refuse anything but a credential-free https URL on an
/// allowlisted host.
pub(crate) fn checked_url(url: &str) -> Result<Url, MarketDataError> {
    let parsed = Url::parse(url).map_err(|_| MarketDataError::InvalidUrl)?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.port().is_some()
    {
        return Err(MarketDataError::InvalidUrl);
    }
    match parsed.host_str() {
        Some(host) if ALLOWED_HOSTS.contains(&host) => Ok(parsed),
        _ => Err(MarketDataError::HostNotAllowed),
    }
}

/// https only, verified TLS (the rustls default), no redirects, no proxy from
/// the environment, bounded connect and request time.
fn hardened_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
}

/// One provider's HTTP client and pacer.
pub(crate) struct Http {
    provider: Provider,
    client: reqwest::Client,
    pacer: Pacer,
}

impl Http {
    pub(crate) fn new(provider: Provider, gap: Duration) -> Result<Self, MarketDataError> {
        Self::build(provider, gap, hardened_builder())
    }

    /// Same hardening but plain HTTP, for the local test server.
    #[cfg(test)]
    pub(crate) fn plain(provider: Provider, gap: Duration) -> Self {
        Self::build(provider, gap, hardened_builder().https_only(false)).unwrap()
    }

    fn build(
        provider: Provider,
        gap: Duration,
        builder: reqwest::ClientBuilder,
    ) -> Result<Self, MarketDataError> {
        let client = builder.build().map_err(|e| http_error(provider, e))?;
        Ok(Self {
            provider,
            client,
            pacer: Pacer::new(gap),
        })
    }

    pub(crate) fn provider(&self) -> Provider {
        self.provider
    }

    pub(crate) fn get(&self, url: Url) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header(header::ACCEPT, "application/json")
    }

    pub(crate) fn post(&self, url: Url) -> reqwest::RequestBuilder {
        self.client
            .post(url)
            .header(header::ACCEPT, "application/json")
    }

    /// Pace, send, map 429 and other non-2xx statuses, read a capped body and
    /// decode it as `T`.
    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, MarketDataError> {
        let provider = self.provider;
        tokio::time::sleep_until(self.pacer.reserve(provider)?.into()).await;
        self.pacer.ensure_open(provider)?;
        let response = request.send().await.map_err(|e| http_error(provider, e))?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(self.rate_limited(retry_after(&response)));
        }
        if !status.is_success() {
            return Err(MarketDataError::Status {
                provider,
                status: status.as_u16(),
            });
        }
        let body = read_capped(provider, response).await?;
        // The serde error is dropped: its message quotes response text.
        serde_json::from_slice(&body).map_err(|_| MarketDataError::Decode { provider })
    }

    /// Block this provider for `backoff` and return the matching error.
    pub(crate) fn rate_limited(&self, backoff: Duration) -> MarketDataError {
        self.pacer.block(backoff);
        MarketDataError::RateLimited {
            provider: self.provider,
            retry_after_secs: backoff.as_secs(),
        }
    }
}

fn http_error(provider: Provider, error: reqwest::Error) -> MarketDataError {
    MarketDataError::Http {
        provider,
        source: error.without_url(),
    }
}

/// `Retry-After` in whole seconds, clamped to `1..=MAX_BACKOFF`. HTTP dates
/// and junk fall back to [`DEFAULT_BACKOFF`].
fn retry_after(response: &reqwest::Response) -> Duration {
    response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|secs| Duration::from_secs(secs).clamp(Duration::from_secs(1), MAX_BACKOFF))
        .unwrap_or(DEFAULT_BACKOFF)
}

/// Read a response body, refusing more than [`MAX_BODY_BYTES`] while reading.
async fn read_capped(
    provider: Provider,
    mut response: reqwest::Response,
) -> Result<Vec<u8>, MarketDataError> {
    let too_large = MarketDataError::TooLarge { provider };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(too_large);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| http_error(provider, e))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(too_large);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Spaces one provider's requests `gap` apart and enforces the back-off after
/// a 429. Slots are reserved under a short synchronous lock; the wait happens
/// outside it and is bounded by [`MAX_QUEUE_WAIT`].
struct Pacer {
    gap: Duration,
    state: Mutex<PacerState>,
}

struct PacerState {
    next: Instant,
    blocked_until: Option<Instant>,
}

impl Pacer {
    fn new(gap: Duration) -> Self {
        Self {
            gap,
            state: Mutex::new(PacerState {
                next: Instant::now(),
                blocked_until: None,
            }),
        }
    }

    fn check(state: &PacerState, now: Instant, provider: Provider) -> Result<(), MarketDataError> {
        match state.blocked_until.filter(|until| *until > now) {
            Some(until) => Err(MarketDataError::RateLimited {
                provider,
                retry_after_secs: (until - now).as_secs().max(1),
            }),
            None => Ok(()),
        }
    }

    fn ensure_open(&self, provider: Provider) -> Result<(), MarketDataError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::check(&state, Instant::now(), provider)
    }

    fn reserve(&self, provider: Provider) -> Result<Instant, MarketDataError> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::check(&state, now, provider)?;
        let at = state.next.max(now);
        if at - now > MAX_QUEUE_WAIT {
            return Err(MarketDataError::Busy { provider });
        }
        state.next = at + self.gap;
        Ok(at)
    }

    fn block(&self, backoff: Duration) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.blocked_until = Some(Instant::now() + backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, response, serve};

    const P: Provider = Provider::DexScreener;

    fn url(base: &str) -> Url {
        Url::parse(&format!("{base}/x")).unwrap()
    }

    #[test]
    fn only_allowlisted_https_hosts_pass() {
        assert!(checked_url("https://api.dexscreener.com/tokens/v1").is_ok());
        for (bad, host) in [
            ("http://api.dexscreener.com/x", false),
            ("https://user:pw@api.dexscreener.com/x", false),
            ("https://api.dexscreener.com:8443/x", false),
            ("https://api.dexscreener.com/x#f", false),
            ("not a url", false),
            ("https://evil.example/x", true),
            ("https://api.dexscreener.com.evil.example/x", true),
            ("https://127.0.0.1/x", true),
        ] {
            let error = checked_url(bad).unwrap_err();
            assert_eq!(
                matches!(error, MarketDataError::HostNotAllowed),
                host,
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn pacer_spaces_requests_and_blocks_after_429() {
        let pacer = Pacer::new(Duration::from_millis(500));
        let first = pacer.reserve(P).unwrap();
        assert_eq!(
            pacer.reserve(P).unwrap() - first,
            Duration::from_millis(500)
        );
        for _ in 0..9 {
            pacer.reserve(P).unwrap();
        }
        assert!(matches!(
            pacer.reserve(P),
            Err(MarketDataError::Busy { .. })
        ));
        pacer.block(Duration::from_secs(60));
        assert!(matches!(
            pacer.reserve(P),
            Err(MarketDataError::RateLimited { retry_after_secs, .. }) if retry_after_secs > 50
        ));
    }

    #[tokio::test]
    async fn rate_limit_is_typed_and_blocks_the_provider() {
        let (base, _) = serve(response(
            "429 Too Many Requests",
            "retry-after: 7\r\n",
            "{}",
        ))
        .await;
        let http = Http::plain(P, Duration::ZERO);
        let error = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            MarketDataError::RateLimited {
                retry_after_secs: 7,
                ..
            }
        ));
        // The next call fails fast without touching the network.
        let again = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(again, MarketDataError::RateLimited { .. }));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (base, _) = serve(response(
            "302 Found",
            "location: http://127.0.0.1:1/elsewhere\r\n",
            "{}",
        ))
        .await;
        let http = Http::plain(P, Duration::ZERO);
        let error = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(error, MarketDataError::Status { status: 302, .. }));
    }

    #[tokio::test]
    async fn oversize_and_malformed_bodies_are_refused() {
        let http = Http::plain(P, Duration::ZERO);
        let big = "1".repeat(MAX_BODY_BYTES + 1);
        let (base, _) = serve(ok_json(&big)).await;
        let error = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(error, MarketDataError::TooLarge { .. }));

        // No content-length: the cap applies while streaming.
        let mut chunked = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
        let chunk = vec![b' '; 64 * 1024];
        for _ in 0..(MAX_BODY_BYTES / chunk.len() + 1) {
            chunked.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            chunked.extend_from_slice(&chunk);
            chunked.extend_from_slice(b"\r\n");
        }
        chunked.extend_from_slice(b"0\r\n\r\n");
        let (base, _) = serve(chunked).await;
        let error = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(error, MarketDataError::TooLarge { .. }));

        let (base, _) = serve(ok_json("{not json")).await;
        let error = http
            .json::<serde_json::Value>(http.get(url(&base)))
            .await
            .unwrap_err();
        assert!(matches!(error, MarketDataError::Decode { .. }));
    }

    #[tokio::test]
    async fn errors_do_not_carry_the_url() {
        // A closed local port: the connect fails without leaving the host.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let http = Http::plain(P, Duration::ZERO);
        let secret = Url::parse(&format!(
            "http://127.0.0.1:{port}/v2/secret-key?api-key=secret-key"
        ))
        .unwrap();
        let error = http
            .json::<serde_json::Value>(http.get(secret))
            .await
            .unwrap_err();
        let mut text = format!("{error} {error:?}");
        let mut source = std::error::Error::source(&error);
        while let Some(inner) = source {
            text.push_str(&format!(" {inner} {inner:?}"));
            source = inner.source();
        }
        assert!(!text.contains("secret-key"), "{text}");
    }
}
