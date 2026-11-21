//! SSRF and limit tests: a fake resolver decides where each name points, and
//! a plain-HTTP server on 127.0.0.1 plays every site.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::*;

/// Answers for a name, in turn; the last one repeats.
#[derive(Default)]
pub(crate) struct FakeResolver {
    names: HashMap<String, Vec<Vec<IpAddr>>>,
    calls: AtomicUsize,
}

impl FakeResolver {
    pub(crate) fn with(mut self, host: &str, ips: &[&str]) -> Self {
        let ips = ips.iter().map(|ip| ip.parse().unwrap()).collect();
        self.names.entry(host.into()).or_default().push(ips);
        self
    }
}

impl Resolve for FakeResolver {
    async fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let answers = self
            .names
            .get(host)
            .ok_or_else(|| io::Error::other("NXDOMAIN"))?;
        Ok(answers[call.min(answers.len() - 1)].clone())
    }
}

/// A site on 127.0.0.1: answers each request with `route(path)` and keeps
/// every request head.
pub(crate) struct Site {
    pub(crate) port: u16,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
}

pub(crate) async fn site(route: fn(&str) -> Vec<u8>) -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0; 1024];
                while let Ok(n @ 1..) = socket.read(&mut buf).await {
                    head.extend_from_slice(&buf[..n]);
                    if head.ends_with(b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&head).to_string();
                        let path = head.split(' ').nth(1).unwrap_or_default().to_string();
                        seen.lock().unwrap().push(head);
                        let _ = socket.write_all(&route(&path)).await;
                        return;
                    }
                }
            });
        }
    });
    Site { port, requests }
}

pub(crate) fn html(body: &str) -> Vec<u8> {
    response("200 OK", "text/html; charset=utf-8", body.as_bytes())
}

fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn redirect(location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 302 Found\r\nlocation: {location}\r\nset-cookie: sid=secret\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    )
    .into_bytes()
}

fn loopback_or_public(ip: IpAddr) -> bool {
    ip.is_loopback() || is_public(ip)
}

fn local(site: &Site) -> Fetcher<FakeResolver> {
    let resolver = FakeResolver::default()
        .with("page.example.com", &["127.0.0.1"])
        .with("other.example.com", &["127.0.0.1"])
        .with("private.example.com", &["10.0.0.1"]);
    Fetcher::for_test(resolver, loopback_or_public, site.port)
}

fn routes(path: &str) -> Vec<u8> {
    match path {
        "/" => html("<title>Home</title><p>Hello</p>"),
        "/final" => html("<p>Final</p>"),
        "/to-final" => redirect("/final"),
        "/to-other" => redirect("https://other.example.com/final"),
        "/to-private" => redirect("https://private.example.com/"),
        "/to-http" => redirect("http://page.example.com/final"),
        "/to-ip" => redirect("https://127.0.0.1/"),
        "/to-ipv6" => redirect("https://[::1]/"),
        "/to-localhost" => redirect("https://localhost/"),
        "/to-port" => redirect("https://page.example.com:8080/"),
        "/to-denied" => redirect("https://api.jarvis.example.net/"),
        "/to-logout" => redirect("/logout"),
        "/to-self" => redirect("/to-self"),
        "/chain3" => redirect("/chain2"),
        "/chain2" => redirect("/chain1"),
        "/chain1" => redirect("/final"),
        "/chain4" => redirect("/chain3"),
        "/no-location" => b"HTTP/1.1 302 Found\r\ncontent-length: 0\r\n\r\n".to_vec(),
        "/missing" => response("404 Not Found", "text/html", b"gone"),
        "/not-modified" => b"HTTP/1.1 304 Not Modified\r\n\r\n".to_vec(),
        "/server-error" => response("500 Internal Server Error", "text/html", b"x"),
        "/json" => response("200 OK", "application/json", b"{}"),
        "/png" => response("200 OK", "image/png", b"\x89PNG"),
        "/no-type" => b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi".to_vec(),
        "/text" => response("200 OK", "text/plain; charset=utf-8", b"plain"),
        "/upper" => response(
            "200 OK",
            "Text/HTML; Charset=\"windows-1252\"",
            b"<p>caf\xe9</p>",
        ),
        "/huge" => response("200 OK", "text/html", &vec![b'a'; 3 * MAX_BODY_BYTES / 2]),
        _ => response("404 Not Found", "text/plain", b""),
    }
}

async fn fetch(fetcher: &Fetcher<FakeResolver>, path: &str) -> Result<Page, Refusal> {
    let deny = ["jarvis.example.net".to_string()];
    fetcher
        .fetch(&format!("https://page.example.com{path}"), &deny)
        .await
}

#[tokio::test]
async fn reads_a_page_through_the_pinned_address_without_credentials() {
    let site = site(routes).await;
    // page.example.com does not point at 127.0.0.1 in real DNS: the request
    // can only arrive here through the pinned address.
    let page = fetch(&local(&site), "/").await.unwrap();
    assert_eq!(page.title, "Home");
    assert_eq!(page.text, "Hello");
    assert_eq!(page.url, "https://page.example.com/");
    let requests = site.requests.lock().unwrap();
    let head = requests[0].to_lowercase();
    assert!(head.starts_with("get / http/1.1\r\n"), "{head}");
    assert!(
        head.contains(&format!("host: page.example.com:{}\r\n", site.port)),
        "{head}"
    );
    assert!(
        head.contains("user-agent: jarvispagereader/1\r\n"),
        "{head}"
    );
    assert!(head.contains("accept: text/html"), "{head}");
    for header in [
        "cookie:",
        "authorization:",
        "referer:",
        "proxy-",
        "x-forwarded",
    ] {
        assert!(!head.contains(header), "{head}");
    }
}

#[tokio::test]
async fn refuses_any_non_public_address_before_connecting() {
    let site = site(routes).await;
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.5.4",
        "192.168.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "::ffff:127.0.0.1",
        "::ffff:192.168.1.1",
        "64:ff9b::a00:1",
        "2002:a00:1::1",
        "ff02::1",
    ] {
        // One bad address among good ones is enough to refuse.
        let resolver = FakeResolver::default().with("page.example.com", &["8.8.8.8", ip]);
        let fetcher = Fetcher::for_test(resolver, is_public, site.port);
        assert_eq!(
            fetch(&fetcher, "/").await,
            Err(Refusal::AddressRefused),
            "{ip}"
        );
    }
    assert!(site.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unresolvable_names_are_refused() {
    let site = site(routes).await;
    let empty = FakeResolver::default().with("page.example.com", &[]);
    let fetcher = Fetcher::for_test(empty, loopback_or_public, site.port);
    assert_eq!(fetch(&fetcher, "/").await, Err(Refusal::DnsFailed));
    let unknown = Fetcher::for_test(FakeResolver::default(), loopback_or_public, site.port);
    assert_eq!(fetch(&unknown, "/").await, Err(Refusal::DnsFailed));
    assert!(site.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn every_redirect_hop_is_checked_again() {
    let site = site(routes).await;
    let fetcher = local(&site);
    for (path, expected) in [
        ("/to-private", Refusal::AddressRefused),
        ("/to-http", Refusal::UrlRefused),
        ("/to-ip", Refusal::UrlRefused),
        ("/to-ipv6", Refusal::UrlRefused),
        ("/to-localhost", Refusal::UrlRefused),
        ("/to-port", Refusal::UrlRefused),
        ("/to-denied", Refusal::UrlRefused),
        ("/to-logout", Refusal::ActionPath),
        ("/to-self", Refusal::Redirects),
        ("/chain4", Refusal::Redirects),
        ("/no-location", Refusal::Redirects),
    ] {
        let before = site.requests.lock().unwrap().len();
        assert_eq!(fetch(&fetcher, path).await, Err(expected), "{path}");
        let after = site.requests.lock().unwrap().len();
        // The refused target is never requested.
        let hops = match path {
            "/to-self" | "/chain4" => MAX_REDIRECTS + 1,
            _ => 1,
        };
        assert_eq!(after - before, hops, "{path}");
    }
}

#[tokio::test]
async fn redirects_within_the_limit_are_followed_without_cookies() {
    let site = site(routes).await;
    let fetcher = local(&site);
    let page = fetch(&fetcher, "/to-final").await.unwrap();
    assert_eq!(
        (page.url.as_str(), page.text.as_str()),
        ("https://page.example.com/final", "Final")
    );
    let page = fetch(&fetcher, "/to-other").await.unwrap();
    assert_eq!(page.url, "https://other.example.com/final");
    let page = fetch(&fetcher, "/chain3").await.unwrap();
    assert_eq!(page.text, "Final");
    let requests = site.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|head| !head.to_lowercase().contains("cookie")));
    assert!(requests.iter().any(|head| head
        .to_lowercase()
        .contains(&format!("host: other.example.com:{}", site.port))));
}

#[tokio::test]
async fn a_name_that_rebinds_between_hops_is_refused() {
    let site = site(routes).await;
    let resolver = FakeResolver::default()
        .with("page.example.com", &["127.0.0.1"])
        .with("page.example.com", &["10.0.0.1"]);
    let fetcher = Fetcher::for_test(resolver, loopback_or_public, site.port);
    assert_eq!(
        fetch(&fetcher, "/to-final").await,
        Err(Refusal::AddressRefused)
    );
    assert_eq!(site.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn status_and_content_type_are_checked() {
    let site = site(routes).await;
    let fetcher = local(&site);
    for (path, expected) in [
        ("/missing", Err(Refusal::Status)),
        ("/not-modified", Err(Refusal::Status)),
        ("/server-error", Err(Refusal::Status)),
        ("/json", Err(Refusal::ContentType)),
        ("/png", Err(Refusal::ContentType)),
        ("/no-type", Err(Refusal::ContentType)),
        ("/text", Ok("plain")),
        ("/upper", Ok("café")),
    ] {
        let got = fetch(&fetcher, path).await;
        assert_eq!(
            got.as_ref().map(|page| page.text.as_str()),
            expected.as_ref().map(|t| *t),
            "{path}"
        );
    }
}

#[tokio::test]
async fn bodies_are_read_up_to_the_limit() {
    let site = site(routes).await;
    let page = fetch(&local(&site), "/huge").await.unwrap();
    assert!(page.truncated);
    assert_eq!(page.text.len(), crate::MAX_TEXT_BYTES);
}

#[tokio::test]
async fn a_silent_site_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let resolver = FakeResolver::default().with("page.example.com", &["127.0.0.1"]);
    let fetcher = Fetcher::for_test(resolver, loopback_or_public, port);
    let started = std::time::Instant::now();
    assert_eq!(fetch(&fetcher, "/").await, Err(Refusal::Timeout));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn the_hosts_own_networks_are_not_remote() {
    // Loopback is always an interface network; a public resolver is not.
    assert!(on_local_network("127.0.0.1".parse().unwrap()));
    assert!(on_local_network(
        "::ffff:127.0.0.1".parse::<IpAddr>().unwrap().to_canonical()
    ));
    assert!(!on_local_network("8.8.8.8".parse().unwrap()));
    assert!(is_remote("8.8.8.8".parse().unwrap()));
    assert!(!is_remote("127.0.0.1".parse().unwrap()));
}

#[tokio::test]
async fn the_client_resolves_only_the_pinned_name() {
    let pinned = Pinned {
        host: "page.example.com".into(),
        addrs: vec!["127.0.0.1:443".parse().unwrap()],
    };
    let ok: Vec<_> = pinned
        .resolve("page.example.com".parse().unwrap())
        .await
        .unwrap()
        .collect();
    assert_eq!(ok, vec!["127.0.0.1:443".parse::<SocketAddr>().unwrap()]);
    assert!(pinned
        .resolve("other.example.com".parse().unwrap())
        .await
        .is_err());
}
