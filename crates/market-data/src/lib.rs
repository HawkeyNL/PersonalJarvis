//! Read-only market-data clients for the on-chain memecoin trader.
//!
//! Every client here only *reads* public data: DexScreener pairs, RugCheck
//! token reports, Solana JSON-RPC account reads and Jupiter quotes. Nothing in
//! this crate signs, builds or sends a transaction, and no swap or execution
//! endpoint is called. [`snapshot`] maps the raw reads into
//! [`jarvis_onchain::types::TokenSnapshot`] for the deterministic core.
//!
//! All responses are untrusted input:
//!
//! - requests go only to [`ALLOWED_HOSTS`], over https with verified TLS, no
//!   redirects and no proxy taken from the environment;
//! - connect and request time and the body size are bounded;
//! - each provider has its own pacer, and a 429 turns into a typed
//!   [`MarketDataError::RateLimited`] instead of a long sleep;
//! - errors never carry a URL, so a key in an RPC URL cannot leak into logs;
//! - a value the sources do not give is mapped to the *strict* side (see
//!   [`snapshot`]), never to the permissive default.

use std::fmt;

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};
use zeroize::Zeroizing;

pub mod dexscreener;
mod http;
pub mod jupiter;
pub mod rpc;
pub mod rugcheck;
pub mod snapshot;
pub mod token2022;

/// The only hosts any client in this crate may talk to. A client refuses to
/// build for any other host.
// ponytail: fixed RPC providers; add a host here when the owner picks another
// provider (plus the matching egress-proxy rule).
pub const ALLOWED_HOSTS: &[&str] = &[
    "api.dexscreener.com",
    "api.rugcheck.xyz",
    "api.jup.ag",
    "api.mainnet-beta.solana.com",
    "mainnet.helius-rpc.com",
    "solana-mainnet.g.alchemy.com",
];

/// Data provider, named in errors so a rate limit or failure can be traced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    DexScreener,
    RugCheck,
    Jupiter,
    /// One of the two configured Solana RPC providers (`0` primary, `1` secondary).
    SolanaRpc(u8),
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DexScreener => f.write_str("DexScreener"),
            Self::RugCheck => f.write_str("RugCheck"),
            Self::Jupiter => f.write_str("Jupiter"),
            Self::SolanaRpc(index) => write!(f, "Solana RPC #{index}"),
        }
    }
}

/// Errors from the market-data clients. None of them carries a URL or a key.
#[derive(Debug, thiserror::Error)]
pub enum MarketDataError {
    #[error("URL must be a credential-free https URL")]
    InvalidUrl,
    #[error("host is not on the market-data allowlist")]
    HostNotAllowed,
    #[error("invalid request parameter: {0}")]
    InvalidInput(&'static str),
    #[error("{provider} request failed")]
    Http {
        provider: Provider,
        #[source]
        source: reqwest::Error,
    },
    #[error("{provider} answered HTTP {status}")]
    Status { provider: Provider, status: u16 },
    #[error("{provider} response exceeds the size limit")]
    TooLarge { provider: Provider },
    #[error("{provider} response is not valid JSON for the expected shape")]
    Decode { provider: Provider },
    #[error("{provider} rate limit hit; retry in {retry_after_secs} s")]
    RateLimited {
        provider: Provider,
        retry_after_secs: u64,
    },
    #[error("too many queued {provider} requests")]
    Busy { provider: Provider },
    #[error("{provider} returned JSON-RPC error {code}")]
    Rpc { provider: Provider, code: i64 },
    #[error("{provider} returned invalid data: {reason}")]
    InvalidData {
        provider: Provider,
        reason: &'static str,
    },
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("Jupiter found no route")]
    NoRoute,
    #[error("RPC providers disagree on {0}")]
    ProviderDisagreement(&'static str),
}

/// A provider API key. Our copy is zeroized on drop and never shown by
/// `Debug`; the per-request header copy is marked sensitive.
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    /// Wrap a key. Refuses empty keys and keys that are not a valid header value.
    pub fn new(key: String) -> Result<Self, MarketDataError> {
        let key = Zeroizing::new(key);
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(MarketDataError::InvalidInput("API key"));
        }
        Ok(Self(key))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Check that `address` looks like a Solana address (base58, 32..=44 chars)
/// before it is put into a URL path or request.
pub(crate) fn validate_address(address: &str) -> Result<(), MarketDataError> {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if (32..=44).contains(&address.len()) && address.bytes().all(|b| BASE58.contains(&b)) {
        Ok(())
    } else {
        Err(MarketDataError::InvalidInput("address"))
    }
}

/// Deserialize an optional decimal given as a JSON number or string. JSON
/// numbers go through their shortest text form, so `1e-7` and `12.5` both
/// parse; the source's own float rounding is not undone.
pub(crate) fn opt_decimal<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
    use serde::de::Error;
    let text = match Option::<serde_json::Value>::deserialize(d)? {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::String(s)) => s,
        Some(_) => return Err(D::Error::custom("expected a decimal number")),
    };
    text.parse::<Decimal>()
        .or_else(|_| Decimal::from_scientific(&text))
        .map(Some)
        .map_err(|_| D::Error::custom("decimal out of range"))
}

/// Deserialize a `u64` given as a decimal string (token amounts in raw units).
pub(crate) fn u64_string<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::Error;
    let text = String::deserialize(d)?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(D::Error::custom("expected an unsigned integer string"));
    }
    text.parse()
        .map_err(|_| D::Error::custom("integer out of range"))
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A local plain-HTTP server answering every request with `response`.
    /// Returns its base URL and the raw bytes of the last request seen.
    pub async fn serve(response: Vec<u8>) -> (String, Arc<Mutex<Vec<u8>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let last = seen.clone();
        let response = Arc::new(response);
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let response = response.clone();
                let last = last.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0; 4096];
                    while let Ok(n @ 1..) = socket.read(&mut buf).await {
                        request.extend_from_slice(&buf[..n]);
                        if let Some(end) = header_end(&request) {
                            let body_len = content_length(&request[..end]);
                            if request.len() >= end + body_len {
                                *last.lock().unwrap() = std::mem::take(&mut request);
                                if socket.write_all(&response).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });
        (base, seen)
    }

    fn header_end(request: &[u8]) -> Option<usize> {
        request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
    }

    fn content_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0)
    }

    pub fn response(status: &str, extra_headers: &str, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra_headers}content-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    pub fn ok_json(body: &str) -> Vec<u8> {
        response("200 OK", "", body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_debug_is_redacted() {
        let key = ApiKey::new("super-secret-key".to_owned()).unwrap();
        assert!(!format!("{key:?}").contains("super-secret"));
        assert!(ApiKey::new(String::new()).is_err());
        assert!(ApiKey::new("bad\nkey".to_owned()).is_err());
    }

    #[test]
    fn addresses_are_validated() {
        assert!(validate_address("So11111111111111111111111111111111111111112").is_ok());
        for bad in [
            "",
            "short",
            "So1111111111111111111111111111111111111111/",
            "0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl",
        ] {
            assert!(validate_address(bad).is_err(), "{bad}");
        }
    }

    #[derive(Deserialize)]
    struct Num {
        #[serde(default, deserialize_with = "opt_decimal")]
        v: Option<Decimal>,
    }

    #[test]
    fn decimals_parse_from_numbers_and_strings() {
        let parse = |json: &str| serde_json::from_str::<Num>(json).map(|n| n.v);
        assert_eq!(
            parse(r#"{"v":12.5}"#).unwrap(),
            Some("12.5".parse().unwrap())
        );
        assert_eq!(
            parse(r#"{"v":"0.001"}"#).unwrap(),
            Some("0.001".parse().unwrap())
        );
        assert_eq!(
            parse(r#"{"v":1e-7}"#).unwrap(),
            Some("0.0000001".parse().unwrap())
        );
        assert_eq!(parse(r#"{"v":null}"#).unwrap(), None);
        assert_eq!(parse(r#"{}"#).unwrap(), None);
        assert!(parse(r#"{"v":1e300}"#).is_err());
        assert!(parse(r#"{"v":true}"#).is_err());
        assert!(parse(r#"{"v":"abc"}"#).is_err());
    }
}
