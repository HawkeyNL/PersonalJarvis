//! One page fetch: resolve, refuse non-public addresses, pin the connection,
//! follow redirects by hand and read a bounded body.

use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use reqwest::{
    dns::{Name, Resolve as ReqwestResolve, Resolving},
    header, redirect, StatusCode,
};
use url::Url;

use crate::{check_url, extract, is_public, Page, Refusal, UPSTREAM_DEADLINE};

pub const MAX_REDIRECTS: usize = 3;
/// Upper bound for one response body; the rest is not read.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
pub const USER_AGENT: &str = "JarvisPageReader/1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.9";

/// Name resolution for the reader, replaceable in tests.
pub trait Resolve: Send + Sync {
    fn resolve(&self, host: &str) -> impl Future<Output = io::Result<Vec<IpAddr>>> + Send;
}

/// The system resolver (systemd-resolved on the Home Node).
pub struct SystemResolver;

impl Resolve for SystemResolver {
    async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        Ok(tokio::net::lookup_host((host, 443))
            .await?
            .map(|addr| addr.ip())
            .collect())
    }
}

/// The only resolver the HTTP client gets: the checked addresses for the one
/// host of this hop, and an error for any other name, so the client can
/// never resolve a name itself.
struct Pinned {
    host: String,
    addrs: Vec<SocketAddr>,
}

impl ReqwestResolve for Pinned {
    fn resolve(&self, name: Name) -> Resolving {
        let addrs = (name.as_str() == self.host).then(|| self.addrs.clone());
        Box::pin(async move {
            let addrs = addrs.ok_or_else(|| io::Error::other("name is not pinned"))?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct Fetcher<R> {
    resolver: R,
    allow: fn(IpAddr) -> bool,
    deadline: Duration,
    /// Tests only: connect over plain http to this port instead of 443.
    #[cfg(test)]
    pub(crate) plain_http_port: Option<u16>,
}

impl<R: Resolve> Fetcher<R> {
    pub fn new(resolver: R) -> Self {
        Self {
            resolver,
            allow: is_public,
            deadline: UPSTREAM_DEADLINE,
            #[cfg(test)]
            plain_http_port: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(resolver: R, allow: fn(IpAddr) -> bool, port: u16) -> Self {
        Self {
            resolver,
            allow,
            deadline: Duration::from_secs(2),
            plain_http_port: Some(port),
        }
    }

    /// Fetch `url` (checked again here) as a [`Page`], within the upstream
    /// deadline.
    pub async fn fetch(&self, url: &str, deny_hosts: &[String]) -> Result<Page, Refusal> {
        tokio::time::timeout(self.deadline, self.follow(url, deny_hosts))
            .await
            .unwrap_or(Err(Refusal::Timeout))
    }

    async fn follow(&self, url: &str, deny_hosts: &[String]) -> Result<Page, Refusal> {
        let mut url = check_url(url, deny_hosts)?;
        for hop in 0..=MAX_REDIRECTS {
            let mut response = self.get(&url).await?;
            let status = response.status();
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                let next = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|location| location.to_str().ok())
                    .and_then(|location| url.join(location).ok())
                    .filter(|_| hop < MAX_REDIRECTS)
                    .ok_or(Refusal::Redirects)?;
                url = check_url(next.as_str(), deny_hosts)?;
                continue;
            }
            if !status.is_success() {
                return Err(Refusal::Status);
            }
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mut parts = content_type.split(';').map(str::trim);
            let essence = parts.next().unwrap_or_default();
            let html = match essence {
                "text/html" | "application/xhtml+xml" => true,
                "text/plain" => false,
                _ => return Err(Refusal::ContentType),
            };
            let charset = parts
                .filter_map(|part| part.strip_prefix("charset="))
                .map(|label| label.trim_matches(['"', '\'']).to_string())
                .next();
            let mut body = Vec::new();
            let mut cut = false;
            while let Some(chunk) = response.chunk().await.map_err(upstream)? {
                let room = MAX_BODY_BYTES - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if chunk.len() > room {
                    cut = true;
                    break;
                }
            }
            return Ok(extract::page(&url, &body, html, charset.as_deref(), cut));
        }
        Err(Refusal::Redirects)
    }

    /// One GET without redirects, cookies, auth or Referer, connected only to
    /// the addresses this hop resolved and checked.
    async fn get(&self, url: &Url) -> Result<reqwest::Response, Refusal> {
        let host = url.host_str().ok_or(Refusal::UrlRefused)?.to_string();
        let ips = self
            .resolver
            .resolve(&host)
            .await
            .map_err(|_| Refusal::DnsFailed)?;
        if ips.is_empty() {
            return Err(Refusal::DnsFailed);
        }
        if !ips.iter().all(|ip| (self.allow)(*ip)) {
            return Err(Refusal::AddressRefused);
        }
        let (target, https_only) = self.target(url);
        let pinned = Pinned {
            host,
            addrs: ips.into_iter().map(|ip| SocketAddr::new(ip, 443)).collect(),
        };
        let client = reqwest::Client::builder()
            .https_only(https_only)
            .redirect(redirect::Policy::none())
            .no_proxy()
            .referer(false)
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            .dns_resolver(Arc::new(pinned))
            .build()
            .map_err(|_| Refusal::Upstream)?;
        client
            .get(target)
            .header(header::ACCEPT, ACCEPT)
            .send()
            .await
            .map_err(upstream)
    }
}

impl<R> Fetcher<R> {
    /// Where to connect and whether https is required: the checked URL,
    /// except over plain http to a local port in tests.
    fn target(&self, url: &Url) -> (Url, bool) {
        #[cfg(test)]
        if let Some(port) = self.plain_http_port {
            let mut target = url.clone();
            let _ = target.set_scheme("http");
            let _ = target.set_port(Some(port));
            return (target, false);
        }
        (url.clone(), true)
    }
}

/// Never carries the reqwest error on: it may hold the URL.
fn upstream(error: reqwest::Error) -> Refusal {
    if error.is_timeout() {
        Refusal::Timeout
    } else {
        Refusal::Upstream
    }
}

#[cfg(test)]
pub(crate) mod tests;
