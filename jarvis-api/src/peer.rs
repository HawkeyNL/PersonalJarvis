//! Who a request came from. Every client-IP and HTTPS decision routes
//! through here so no handler reads forwarding headers on its own.
//!
//! Forwarding headers are honoured only for a connection on the
//! `JARVIS_API_UNIX_SOCKET` whose kernel-reported (`SO_PEERCRED`) uid is the
//! reverse proxy's. A TCP peer is never trusted, whatever its address.

use std::ffi::CStr;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context};
use axum::{
    extract::connect_info::{ConnectInfo, Connected},
    http::{Extensions, HeaderMap},
    serve::IncomingStream,
};
use tokio::net::{TcpListener, UnixListener};

use crate::AppState;

/// The only local account whose Unix-socket connections may assert forwarding
/// headers: the Caddy reverse proxy.
const PROXY_USER: &CStr = c"caddy";

/// The transport peer of one connection, captured when it is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    Tcp(SocketAddr),
    /// Kernel-reported uid of the connecting process; `None` if unavailable.
    Unix {
        uid: Option<u32>,
    },
}

impl Connected<IncomingStream<'_, TcpListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        Peer::Tcp(*stream.remote_addr())
    }
}

impl Connected<IncomingStream<'_, UnixListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        Peer::Unix {
            uid: stream.io().peer_cred().ok().map(|cred| cred.uid()),
        }
    }
}

/// Whether this connection may assert forwarding headers: only the single
/// local Caddy proxy on the API socket.
fn is_trusted_proxy(peer: Option<Peer>, unix_proxy_uid: Option<u32>) -> bool {
    matches!(
        (peer, unix_proxy_uid),
        (Some(Peer::Unix { uid: Some(uid) }), Some(proxy)) if uid == proxy
    )
}

/// The client address. An untrusted peer is keyed by its own TCP address
/// (or `None` on the socket); a trusted proxy that sent no usable header also
/// yields `None`: the client is unknown, and callers fail closed (bootstrap)
/// or share one throttling bucket (rate limits).
fn forwarded_client(peer: Option<Peer>, headers: &HeaderMap, trusted: bool) -> Option<IpAddr> {
    if !trusted {
        return match peer {
            Some(Peer::Tcp(addr)) => Some(addr.ip()),
            _ => None,
        };
    }
    // Only the entry the trusted proxy appended matters; anything before it
    // is client-supplied and may be garbage without affecting the result.
    // Repeated header lines form one list in order (RFC 9110 §5.3), so a
    // client line sent ahead of the proxy's cannot displace its entry. An
    // unreadable line still counts as one (unparsable) entry.
    headers
        .get_all("x-forwarded-for")
        .iter()
        .flat_map(|line| line.to_str().unwrap_or("unparsable").split(','))
        .map(str::trim)
        .rfind(|s| !s.is_empty())?
        .parse()
        .ok()
}

fn peer_trust(extensions: &Extensions, state: &AppState) -> (Option<Peer>, bool) {
    let peer = extensions.get::<ConnectInfo<Peer>>().map(|info| info.0);
    (peer, is_trusted_proxy(peer, state.unix_proxy_uid))
}

/// The client IP used for rate limiting, login lockout, bootstrap network
/// policy and audit attribution.
pub(crate) fn client_ip(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> Option<IpAddr> {
    let (peer, trusted) = peer_trust(extensions, state);
    forwarded_client(peer, headers, trusted)
}

/// In production TLS ends at Caddy; only a trusted proxy may assert that the
/// original request used HTTPS.
pub(crate) fn arrived_via_https_proxy(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> bool {
    peer_trust(extensions, state).1
        && headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

/// The reverse proxy's identity, resolved once at startup.
#[derive(Clone, Copy, Debug)]
pub struct ProxyIdentity {
    pub uid: u32,
    pub gid: u32,
}

/// `(uid, primary gid)` of a local account, or `None` if it does not exist.
pub(crate) fn lookup_user(name: &CStr) -> Option<(u32, u32)> {
    // SAFETY: zeroed passwd is a valid out-parameter; getpwnam_r only writes
    // into it and `buf`, both of which outlive the call.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    (status == 0 && !result.is_null()).then_some((entry.pw_uid, entry.pw_gid))
}

/// Resolve the Caddy account. It must be a distinct identity from Core, or
/// any Core-owned connection would count as the proxy.
pub fn proxy_identity() -> anyhow::Result<ProxyIdentity> {
    let (uid, gid) = lookup_user(PROXY_USER)
        .with_context(|| format!("reverse proxy account {PROXY_USER:?} is missing"))?;
    // SAFETY: geteuid has no preconditions.
    if uid == unsafe { libc::geteuid() } {
        bail!("reverse proxy account must not be Core's own user");
    }
    Ok(ProxyIdentity { uid, gid })
}

/// Bind the API socket inside the directory the Core unit creates. The
/// directory must carry the proxy's group (setgid, so the socket inherits it)
/// and grant nothing to anyone else; the socket itself is group read/write
/// only. Trust still comes from the peer uid, not the mode.
pub fn bind_api_socket(path: &Path, proxy_gid: u32) -> anyhow::Result<UnixListener> {
    let parent = path.parent().context("API socket path has no parent")?;
    let dir = fs::symlink_metadata(parent).context("API socket directory unavailable")?;
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if !dir.is_dir() || dir.uid() != euid || dir.mode() & 0o027 != 0 {
        bail!("unsafe API socket directory: must be owned by Core with no group write or other access");
    }
    if dir.gid() != proxy_gid {
        bail!("API socket directory group is not the reverse proxy's group; check the jarvis-core unit's ExecStartPre install step");
    }
    if dir.mode() & 0o2000 == 0 {
        bail!("API socket directory must be setgid so the socket gets the reverse proxy's group; check the jarvis-core unit's ExecStartPre install step");
    }
    match fs::symlink_metadata(path) {
        Ok(info) if info.file_type().is_socket() && info.uid() == euid => fs::remove_file(path)?,
        Ok(_) => bail!("unsafe existing API socket path"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    // Without the proxy's group Caddy could not connect while Core still
    // looked ready on TCP; refuse to start instead.
    if fs::symlink_metadata(path)?.gid() != proxy_gid {
        bail!("API socket did not inherit the reverse proxy's group");
    }
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::get, Router};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const PROXY: u32 = 990;
    const LOOPBACK: &str = "127.0.0.1:41000";

    fn tcp(addr: &str) -> Option<Peer> {
        Some(Peer::Tcp(addr.parse().unwrap()))
    }

    fn unix(uid: u32) -> Option<Peer> {
        Some(Peer::Unix { uid: Some(uid) })
    }

    fn xff(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", value.parse().unwrap());
        headers
    }

    #[test]
    fn unix_socket_trusts_only_the_proxy_uid() {
        assert!(is_trusted_proxy(unix(PROXY), Some(PROXY)));
        assert!(!is_trusted_proxy(unix(PROXY + 1), Some(PROXY)));
        assert!(!is_trusted_proxy(
            Some(Peer::Unix { uid: None }),
            Some(PROXY)
        ));
        assert!(!is_trusted_proxy(unix(PROXY), None));
    }

    #[test]
    fn tcp_is_never_trusted() {
        for addr in [LOOPBACK, "[::1]:41000", "10.0.0.2:5000"] {
            assert!(!is_trusted_proxy(tcp(addr), Some(PROXY)));
            assert!(!is_trusted_proxy(tcp(addr), None));
        }
        assert!(!is_trusted_proxy(None, Some(PROXY)));
    }

    /// A loopback TCP peer that forges every forwarding header is keyed by
    /// its own address, even with the proxy socket configured.
    #[test]
    fn forged_forwarding_headers_from_tcp_are_ignored() {
        let headers = xff("10.23.45.10");
        for proxy in [Some(PROXY), None] {
            let trusted = is_trusted_proxy(tcp(LOOPBACK), proxy);
            assert_eq!(
                forwarded_client(tcp(LOOPBACK), &headers, trusted),
                Some("127.0.0.1".parse().unwrap())
            );
        }
    }

    #[test]
    fn untrusted_peers_are_keyed_by_their_own_address() {
        let headers = xff("6.6.6.6");
        assert_eq!(
            forwarded_client(tcp(LOOPBACK), &headers, false),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(forwarded_client(unix(PROXY + 1), &headers, false), None);
        assert_eq!(forwarded_client(None, &headers, false), None);
    }

    #[test]
    fn the_trusted_proxy_selects_the_entry_it_appended() {
        let headers = xff("9.9.9.9, 203.0.113.7, 10.0.0.9");
        assert_eq!(
            forwarded_client(unix(PROXY), &headers, true),
            Some("10.0.0.9".parse().unwrap())
        );
    }

    #[test]
    fn trusted_proxy_without_a_usable_header_yields_no_client() {
        for headers in [HeaderMap::new(), xff("not-an-ip"), xff(" , ")] {
            assert_eq!(forwarded_client(unix(PROXY), &headers, true), None);
        }
        // The selected entry itself must parse.
        let headers = xff("203.0.113.7, not-an-ip");
        assert_eq!(forwarded_client(unix(PROXY), &headers, true), None);
    }

    #[test]
    fn client_supplied_garbage_before_the_proxy_entry_is_ignored() {
        let headers = xff("not-an-ip, , 203.0.113.7");
        assert_eq!(
            forwarded_client(unix(PROXY), &headers, true),
            Some("203.0.113.7".parse().unwrap())
        );
    }

    #[test]
    fn the_last_entry_across_repeated_header_lines_is_the_proxys() {
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", "6.6.6.6, 9.9.9.9".parse().unwrap());
        headers.append(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"caf\xe9").unwrap(),
        );
        headers.append("x-forwarded-for", "203.0.113.7".parse().unwrap());
        // The proxy's entry is the last entry of the last line.
        assert_eq!(
            forwarded_client(unix(PROXY), &headers, true),
            Some("203.0.113.7".parse().unwrap())
        );
        // An unreadable last line yields no client.
        headers.append(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"caf\xe9").unwrap(),
        );
        assert_eq!(forwarded_client(unix(PROXY), &headers, true), None);
    }

    fn private_dir() -> (tempfile::TempDir, u32) {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o2750)).unwrap();
        let gid = fs::metadata(dir.path()).unwrap().gid();
        (dir, gid)
    }

    #[test]
    fn missing_accounts_are_reported_as_absent() {
        assert_eq!(lookup_user(c"root"), Some((0, 0)));
        assert_eq!(lookup_user(c"jarvis-no-such-account"), None);
    }

    #[tokio::test]
    async fn bound_socket_reports_the_connecting_uid() {
        let (dir, gid) = private_dir();
        let path = dir.path().join("api.sock");
        // A stale socket from a crashed process is replaced.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let listener = bind_api_socket(&path, gid).unwrap();
        let mode = fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o660);

        let client = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let euid = unsafe { libc::geteuid() };
        assert_eq!(server.peer_cred().unwrap().uid(), euid);
        drop(client);

        // A regular file at the socket path is never deleted.
        drop(listener);
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"").unwrap();
        assert!(bind_api_socket(&path, gid).is_err());
        assert!(path.exists());
    }

    #[test]
    fn unsafe_socket_directories_are_refused() {
        let (dir, gid) = private_dir();
        let path = dir.path().join("api.sock");
        // Without the proxy's group (a failed install -d) Core must not listen.
        assert!(bind_api_socket(&path, gid.wrapping_add(1)).is_err());
        // No setgid bit (the socket would get Core's group), or too open.
        for mode in [0o750, 0o2755, 0o2770] {
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).unwrap();
            assert!(bind_api_socket(&path, gid).is_err());
        }
        assert!(!path.exists());
    }

    /// End to end over a real socket: axum captures the peer uid and only the
    /// configured proxy uid may supply the client address.
    #[tokio::test]
    async fn router_over_unix_socket_trusts_only_the_proxy_uid() {
        async fn seen(
            State(proxy): State<u32>,
            ConnectInfo(peer): ConnectInfo<Peer>,
            headers: HeaderMap,
        ) -> String {
            let trusted = is_trusted_proxy(Some(peer), Some(proxy));
            format!("{:?}", forwarded_client(Some(peer), &headers, trusted))
        }
        async fn request(dir: &Path, proxy: u32) -> String {
            let path = dir.join(format!("{proxy}.sock"));
            let gid = fs::metadata(dir).unwrap().gid();
            let listener = bind_api_socket(&path, gid).unwrap();
            let app = Router::new().route("/", get(seen)).with_state(proxy);
            tokio::spawn(async move {
                axum::serve(listener, app.into_make_service_with_connect_info::<Peer>())
                    .await
                    .unwrap();
            });
            let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            stream
                .write_all(
                    b"GET / HTTP/1.1\r\nHost: core\r\nX-Forwarded-For: 203.0.113.7\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        }

        let (dir, _) = private_dir();
        let euid = unsafe { libc::geteuid() };
        assert!(request(dir.path(), euid)
            .await
            .ends_with("Some(203.0.113.7)"));
        assert!(request(dir.path(), euid + 1).await.ends_with("None"));
    }
}
