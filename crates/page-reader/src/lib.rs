//! Reads one public web page for Core, in its own process and identity.
//!
//! Core never fetches a page itself: it sends one URL over the local Unix
//! socket of `jarvis-page-reader` and gets back the page title and plain
//! text, or a fixed refusal code. The reader refuses anything but an https
//! URL on port 443 with a public domain name ([`check_url`]), resolves the
//! name itself, refuses any address that is not public ([`is_public`]),
//! pins the connection to the checked addresses and follows at most
//! [`fetch::MAX_REDIRECTS`] redirects, checking every hop again.
//!
//! Framing mirrors the other Jarvis brokers: one JSON request line of at
//! most [`MAX_REQUEST_BYTES`], answered by one JSON reply line of at most
//! [`MAX_REPLY_BYTES`], then the reader closes the connection.
//!
//! - Request: `{"v":1,"url":"https://…","deny_hosts":["…"]}`; `deny_hosts`
//!   is optional (at most [`MAX_DENY_HOSTS`]) and is refused, subdomains
//!   included, at every hop.
//! - Reply: `{"page":{"url":…,"title":…,"text":…,"truncated":…}}` or
//!   `{"refused":{"code":…}}`.

use std::{
    ffi::CString,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use url::{Host, Url};

pub mod extract;
pub mod fetch;
pub mod server;

/// systemd `RuntimeDirectory=jarvis-page-reader`.
pub const DEFAULT_SOCKET: &str = "/run/jarvis-page-reader/reader.sock";
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_REQUEST_BYTES: usize = 4 * 1024;
/// Title and text plus JSON escaping (at most six bytes per control
/// character, which [`extract`] removes, two per quote or backslash).
pub const MAX_REPLY_BYTES: usize = 256 * 1024;
pub const MAX_TITLE_CHARS: usize = 300;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_URL_BYTES: usize = 2048;
pub const MAX_DENY_HOSTS: usize = 4;
/// Everything upstream for one request: DNS, every hop and the body.
pub const UPSTREAM_DEADLINE: Duration = Duration::from_secs(15);
/// Core's wait, above the reader's own deadline so the reader answers first.
pub const CLIENT_DEADLINE: Duration = Duration::from_secs(20);

/// Top-level domains and special-use names that never name a public page.
const SPECIAL_SUFFIXES: [&str; 13] = [
    "localhost",
    "local",
    "localdomain",
    "internal",
    "intranet",
    "lan",
    "home",
    "corp",
    "private",
    "arpa",
    "onion",
    "test",
    "invalid",
];
/// A path segment that is exactly one of these (case, `-` and `_`
/// ignored, extension dropped) looks like an action a GET may trigger.
const ACTION_SEGMENTS: [&str; 22] = [
    "logout",
    "signout",
    "logoff",
    "unsubscribe",
    "subscribe",
    "optout",
    "delete",
    "remove",
    "confirm",
    "verify",
    "activate",
    "deactivate",
    "approve",
    "accept",
    "decline",
    "reject",
    "cancel",
    "revoke",
    "reset",
    "checkout",
    "purchase",
    "pay",
];
/// Query names that select an action.
const ACTION_QUERY_NAMES: [&str; 5] = ["action", "do", "cmd", "op", "method"];

/// One page request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub v: u32,
    pub url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_hosts: Vec<String>,
}

/// The page as plain text. `url` is the final URL after redirects, without
/// a fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Page(Page),
    Refused { code: Refusal },
}

/// Why no page was read. Fixed codes: never a URL, address or upstream text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    #[error("invalid page reader request")]
    InvalidRequest,
    #[error("URL refused: https on port 443 with a public domain only")]
    UrlRefused,
    #[error("URL refused: the path looks like an action")]
    ActionPath,
    #[error("the host resolves to an address that is not public")]
    AddressRefused,
    #[error("the host name could not be resolved")]
    DnsFailed,
    #[error("too many or invalid redirects")]
    Redirects,
    #[error("the page could not be fetched")]
    Upstream,
    #[error("the site answered with an error status")]
    Status,
    #[error("the page is not HTML or plain text")]
    ContentType,
    #[error("reading the page timed out")]
    Timeout,
    #[error("the page reader is busy")]
    Busy,
    /// Client side only: the socket could not be reached or answered badly.
    #[error("the page reader is unavailable")]
    Unavailable,
}

impl Refusal {
    /// Fixed name for logs and audit lines.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UrlRefused => "url_refused",
            Self::ActionPath => "action_path",
            Self::AddressRefused => "address_refused",
            Self::DnsFailed => "dns_failed",
            Self::Redirects => "redirects",
            Self::Upstream => "upstream",
            Self::Status => "status",
            Self::ContentType => "content_type",
            Self::Timeout => "timeout",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Parse and check a URL the reader may fetch: https, port 443, no user
/// info, a domain of at least two labels that is not special-use and not
/// one of `deny_hosts` or below it, and no action-like path or query. The
/// fragment is dropped. Runs in Core before sending and in the reader at
/// every hop.
pub fn check_url(url: &str, deny_hosts: &[String]) -> Result<Url, Refusal> {
    if url.len() > MAX_URL_BYTES {
        return Err(Refusal::UrlRefused);
    }
    let mut url = Url::parse(url).map_err(|_| Refusal::UrlRefused)?;
    let host = match url.host() {
        Some(Host::Domain(domain)) => domain.trim_end_matches('.').to_ascii_lowercase(),
        _ => return Err(Refusal::UrlRefused),
    };
    if url.scheme() != "https"
        || url.port().is_some_and(|port| port != 443)
        || !url.username().is_empty()
        || url.password().is_some()
        || !is_public_domain(&host)
        || deny_hosts.iter().any(|denied| {
            let denied = denied.trim_end_matches('.').to_ascii_lowercase();
            !denied.is_empty() && (host == denied || host.ends_with(&format!(".{denied}")))
        })
    {
        return Err(Refusal::UrlRefused);
    }
    if is_action(&url) {
        return Err(Refusal::ActionPath);
    }
    url.set_fragment(None);
    Ok(url)
}

/// At least two labels of ASCII letters, digits, `-` and `_` (the URL parser
/// has already applied IDNA), not starting or ending with `-`, a TLD that is
/// not all digits, and no special-use suffix.
fn is_public_domain(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        && !labels[labels.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && !SPECIAL_SUFFIXES
            .iter()
            .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

fn is_action(url: &Url) -> bool {
    let squash = |text: &str| -> String {
        text.chars()
            .filter(|c| !matches!(c, '-' | '_'))
            .collect::<String>()
            .to_lowercase()
    };
    let path_action = url.path_segments().is_some_and(|mut segments| {
        segments.any(|segment| {
            let decoded = percent_decode(segment);
            let stem = decoded.split('.').next().unwrap_or_default();
            ACTION_SEGMENTS.contains(&squash(stem).as_str())
        })
    });
    path_action
        || url
            .query_pairs()
            .any(|(name, _)| ACTION_QUERY_NAMES.contains(&squash(&name).as_str()))
}

fn percent_decode(segment: &str) -> String {
    url::form_urlencoded::parse(format!("x={segment}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

/// Whether an address is public: refuses private, loopback, link-local,
/// shared, documentation, benchmarking, multicast, reserved and other
/// special ranges. IPv6 must be global unicast (2000::/3) outside its
/// special blocks; the IPv4 address inside an IPv4-mapped, NAT64
/// (64:ff9b::/96) or 6to4 (2002::/16) address is checked as IPv4.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    const REFUSED: [(u32, u32); 15] = [
        (0x0000_0000, 8),  // 0.0.0.0/8 "this network"
        (0x0a00_0000, 8),  // 10.0.0.0/8 private
        (0x6440_0000, 10), // 100.64.0.0/10 shared (CGNAT)
        (0x7f00_0000, 8),  // 127.0.0.0/8 loopback
        (0xa9fe_0000, 16), // 169.254.0.0/16 link-local
        (0xac10_0000, 12), // 172.16.0.0/12 private
        (0xc000_0000, 24), // 192.0.0.0/24 IETF protocol assignments
        (0xc000_0200, 24), // 192.0.2.0/24 documentation
        (0xc058_6300, 24), // 192.88.99.0/24 6to4 relay anycast
        (0xc0a8_0000, 16), // 192.168.0.0/16 private
        (0xc612_0000, 15), // 198.18.0.0/15 benchmarking
        (0xc633_6400, 24), // 198.51.100.0/24 documentation
        (0xcb00_7100, 24), // 203.0.113.0/24 documentation
        (0xe000_0000, 4),  // 224.0.0.0/4 multicast
        (0xf000_0000, 4),  // 240.0.0.0/4 reserved and broadcast
    ];
    let bits = u32::from(ip);
    !REFUSED
        .iter()
        .any(|(net, len)| bits >> (32 - len) == net >> (32 - len))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    const REFUSED: [(u128, u32); 4] = [
        (0x2001_0000 << 96, 23), // IETF protocol assignments, Teredo
        (0x2001_0db8 << 96, 32), // documentation
        (0x3fff_0000 << 96, 20), // documentation
        (0x2002_0000 << 96, 16), // 6to4: checked through its IPv4 below
    ];
    let bits = u128::from(ip);
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    if bits >> 32 == 0x0064_ff9b << 64 {
        return is_public_v4(Ipv4Addr::from(bits as u32));
    }
    if bits >> 112 == 0x2002 {
        return is_public_v4(Ipv4Addr::from((bits >> 80) as u32));
    }
    bits >> 125 == 1
        && !REFUSED
            .iter()
            .any(|(net, len)| bits >> (128 - len) == net >> (128 - len))
}

/// uid of a user name. Call once at startup: `getpwnam` is not reentrant.
pub fn lookup_uid(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    // SAFETY: `name` is a valid C string; the result is read at once.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    (!entry.is_null()).then(|| unsafe { (*entry).pw_uid })
}

/// Typed client for the reader socket.
#[derive(Debug, Clone)]
pub struct PageReaderClient {
    socket: PathBuf,
    reader_uid: u32,
}

impl PageReaderClient {
    /// `reader_uid` is the reader's service uid: a socket served by any other
    /// process is refused before the URL is sent.
    pub fn new(socket: impl Into<PathBuf>, reader_uid: u32) -> Self {
        Self {
            socket: socket.into(),
            reader_uid,
        }
    }

    /// Read one page. `url` should already have passed [`check_url`].
    pub async fn read(&self, url: &str, deny_hosts: &[String]) -> Result<Page, Refusal> {
        let request = Request {
            v: PROTOCOL_VERSION,
            url: url.to_string(),
            deny_hosts: deny_hosts.to_vec(),
        };
        let mut line = serde_json::to_vec(&request).map_err(|_| Refusal::InvalidRequest)?;
        line.push(b'\n');
        if line.len() > MAX_REQUEST_BYTES {
            return Err(Refusal::InvalidRequest);
        }
        let reply = tokio::time::timeout(CLIENT_DEADLINE, async {
            let mut stream = UnixStream::connect(&self.socket).await.ok()?;
            if stream.peer_cred().ok()?.uid() != self.reader_uid {
                return None;
            }
            // A busy reader may answer and close before reading the request.
            let _ = stream.write_all(&line).await;
            let mut reply = Vec::new();
            BufReader::new(stream.take(MAX_REPLY_BYTES as u64 + 1))
                .read_until(b'\n', &mut reply)
                .await
                .ok()?;
            Some(reply)
        })
        .await
        .map_err(|_| Refusal::Timeout)?
        .filter(|reply| reply.len() <= MAX_REPLY_BYTES && reply.last() == Some(&b'\n'))
        .ok_or(Refusal::Unavailable)?;
        match serde_json::from_slice(&reply).map_err(|_| Refusal::Unavailable)? {
            Reply::Page(page)
                if page.title.chars().count() <= MAX_TITLE_CHARS
                    && page.text.len() <= MAX_TEXT_BYTES
                    && page.url.len() <= MAX_URL_BYTES =>
            {
                Ok(page)
            }
            Reply::Page(_) => Err(Refusal::Unavailable),
            Reply::Refused { code } => Err(code),
        }
    }
}

#[cfg(test)]
mod tests;
