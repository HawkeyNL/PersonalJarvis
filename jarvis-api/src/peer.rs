//! Who a request came from. Every client-IP and HTTPS decision routes
//! through here so no handler reads forwarding headers on its own.
//!
//! With `JARVIS_API_UNIX_SOCKET` set, forwarding headers are honoured only
//! for a connection on that socket whose kernel-reported (`SO_PEERCRED`) uid
//! is the reverse proxy's; the TCP listener is never trusted. Without the
//! socket, the legacy `JARVIS_TRUSTED_PROXY_*` peer allowlist applies.

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

/// How many trailing `X-Forwarded-For` entries this connection may assert;
/// 0 means forwarding headers are ignored.
fn trusted_hops(
    peer: Option<Peer>,
    unix_proxy_uid: Option<u32>,
    tcp_hops: u32,
    tcp_proxies: &[IpAddr],
) -> u32 {
    match (peer, unix_proxy_uid) {
        // The socket is reachable only by the single local Caddy proxy.
        (Some(Peer::Unix { uid: Some(uid) }), Some(proxy)) if uid == proxy => 1,
        (Some(Peer::Tcp(addr)), None) if tcp_proxies.contains(&addr.ip()) => tcp_hops,
        _ => 0,
    }
}

/// The client address behind `hops` trusted proxies. A trusted proxy that
/// sent no usable header yields `None`: the client is unknown, and callers
/// fail closed (bootstrap) or share one throttling bucket (rate limits).
fn forwarded_client(peer: Option<Peer>, headers: &HeaderMap, hops: u32) -> Option<IpAddr> {
    let direct = match peer {
        Some(Peer::Tcp(addr)) => Some(addr.ip()),
        _ => None,
    };
    if hops == 0 {
        return direct;
    }
    let ips = headers
        .get("x-forwarded-for")?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<IpAddr>, _>>()
        .ok()?;
    ips.len().checked_sub(hops as usize).map(|index| ips[index])
}

fn peer_hops(extensions: &Extensions, state: &AppState) -> (Option<Peer>, u32) {
    let peer = extensions.get::<ConnectInfo<Peer>>().map(|info| info.0);
    let hops = trusted_hops(
        peer,
        state.unix_proxy_uid,
        state.trusted_proxy_hops,
        &state.trusted_proxy_ips,
    );
    (peer, hops)
}

/// The client IP used for rate limiting, login lockout, bootstrap network
/// policy and audit attribution.
pub(crate) fn client_ip(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> Option<IpAddr> {
    let (peer, hops) = peer_hops(extensions, state);
    forwarded_client(peer, headers, hops)
}

/// In production TLS ends at Caddy; only a trusted proxy may assert that the
/// original request used HTTPS.
pub(crate) fn arrived_via_https_proxy(
    extensions: &Extensions,
    headers: &HeaderMap,
    state: &AppState,
) -> bool {
    peer_hops(extensions, state).1 > 0
        && headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

/// Resolve the reverse proxy's uid once at startup.
pub fn proxy_uid() -> anyhow::Result<u32> {
    // SAFETY: getpwnam is given a valid NUL-terminated name and its result is
    // read immediately, before any other passwd lookup on this thread.
    let entry = unsafe { libc::getpwnam(PROXY_USER.as_ptr()) };
    if entry.is_null() {
        bail!("reverse proxy account {PROXY_USER:?} is missing");
    }
    Ok(unsafe { (*entry).pw_uid })
}

/// Bind the API socket inside its systemd-managed runtime directory. The
/// directory's group (setgid) decides who may connect; the socket itself is
/// group read/write only. Trust still comes from the peer uid, not the mode.
pub fn bind_api_socket(path: &Path) -> anyhow::Result<UnixListener> {
    let parent = path.parent().context("API socket path has no parent")?;
    let dir = fs::symlink_metadata(parent).context("API socket directory unavailable")?;
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if !dir.is_dir() || dir.uid() != euid || dir.mode() & 0o007 != 0 {
        bail!("unsafe API socket directory");
    }
    match fs::symlink_metadata(path) {
        Ok(info) if info.file_type().is_socket() && info.uid() == euid => fs::remove_file(path)?,
        Ok(_) => bail!("unsafe existing API socket path"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
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

    fn loopback() -> Vec<IpAddr> {
        vec!["127.0.0.1".parse().unwrap()]
    }

    #[test]
    fn unix_socket_trusts_only_the_proxy_uid() {
        assert_eq!(trusted_hops(unix(PROXY), Some(PROXY), 0, &[]), 1);
        assert_eq!(
            trusted_hops(unix(PROXY + 1), Some(PROXY), 1, &loopback()),
            0
        );
        assert_eq!(
            trusted_hops(Some(Peer::Unix { uid: None }), Some(PROXY), 0, &[]),
            0
        );
    }

    #[test]
    fn tcp_is_never_trusted_once_the_socket_is_enabled() {
        assert_eq!(trusted_hops(tcp(LOOPBACK), Some(PROXY), 1, &loopback()), 0);
    }

    #[test]
    fn tcp_keeps_the_legacy_allowlist_without_the_socket() {
        assert_eq!(trusted_hops(tcp(LOOPBACK), None, 1, &loopback()), 1);
        assert_eq!(trusted_hops(tcp("10.0.0.2:5000"), None, 1, &loopback()), 0);
        assert_eq!(trusted_hops(tcp(LOOPBACK), None, 0, &loopback()), 0);
        assert_eq!(trusted_hops(None, None, 1, &loopback()), 0);
    }

    #[test]
    fn untrusted_peers_are_keyed_by_their_own_address() {
        let headers = xff("6.6.6.6");
        assert_eq!(
            forwarded_client(tcp(LOOPBACK), &headers, 0),
            Some("127.0.0.1".parse().unwrap())
        );
        assert_eq!(forwarded_client(unix(PROXY + 1), &headers, 0), None);
        assert_eq!(forwarded_client(None, &headers, 0), None);
    }

    #[test]
    fn trusted_hops_select_the_client_before_the_proxy_chain() {
        let headers = xff("9.9.9.9, 203.0.113.7, 10.0.0.9");
        assert_eq!(
            forwarded_client(unix(PROXY), &headers, 1),
            Some("10.0.0.9".parse().unwrap())
        );
        assert_eq!(
            forwarded_client(tcp(LOOPBACK), &headers, 2),
            Some("203.0.113.7".parse().unwrap())
        );
    }

    #[test]
    fn trusted_proxy_without_a_usable_header_yields_no_client() {
        for headers in [HeaderMap::new(), xff("not-an-ip"), xff("9.9.9.9")] {
            assert_eq!(forwarded_client(tcp(LOOPBACK), &headers, 2), None);
        }
    }

    #[tokio::test]
    async fn bound_socket_reports_the_connecting_uid() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let path = dir.path().join("api.sock");
        // A stale socket from a crashed process is replaced.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let listener = bind_api_socket(&path).unwrap();
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
        assert!(bind_api_socket(&path).is_err());
        assert!(path.exists());
    }

    #[test]
    fn world_accessible_socket_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(bind_api_socket(&dir.path().join("api.sock")).is_err());
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
            let hops = trusted_hops(Some(peer), Some(proxy), 0, &[]);
            format!("{:?}", forwarded_client(Some(peer), &headers, hops))
        }
        async fn request(dir: &Path, proxy: u32) -> String {
            let path = dir.join(format!("{proxy}.sock"));
            let listener = bind_api_socket(&path).unwrap();
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

        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let euid = unsafe { libc::geteuid() };
        assert!(request(dir.path(), euid)
            .await
            .ends_with("Some(203.0.113.7)"));
        assert!(request(dir.path(), euid + 1).await.ends_with("None"));
    }
}
