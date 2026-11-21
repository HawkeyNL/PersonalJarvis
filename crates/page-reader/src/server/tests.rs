use std::sync::Mutex;

use tokio::io::AsyncWriteExt;

use super::*;
use crate::{
    fetch::tests::{html, site, FakeResolver},
    Page, PageReaderClient,
};

fn own_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

/// A reader on a temporary socket that lets `allowed` in, fetching from a
/// local site where page.example.com points.
async fn reader(
    allowed: u32,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<Mutex<Vec<String>>>,
) {
    let site = site(|path| match path {
        "/" => html("<title>Page</title><p>Body</p>"),
        _ => html(""),
    })
    .await;
    let resolver = FakeResolver::default().with("page.example.com", &["127.0.0.1"]);
    let fetcher = Fetcher::for_test(resolver, |ip| ip.is_loopback(), site.port);
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("reader.sock");
    let listener = bind(&socket).unwrap();
    tokio::spawn(serve(
        listener,
        Arc::new(fetcher),
        vec![allowed].into(),
        std::future::pending(),
    ));
    (dir, socket, site.requests)
}

#[tokio::test]
async fn serves_a_page_to_an_allowed_peer() {
    let (_dir, socket, _) = reader(own_uid()).await;
    let client = PageReaderClient::new(&socket, own_uid());
    let page = client.read("https://page.example.com/", &[]).await.unwrap();
    assert_eq!(
        page,
        Page {
            url: "https://page.example.com/".into(),
            title: "Page".into(),
            text: "Body".into(),
            truncated: false,
        }
    );
    let refused = client.read("http://page.example.com/", &[]).await;
    assert_eq!(refused, Err(Refusal::UrlRefused));
    let denied = client
        .read("https://page.example.com/", &["example.com".into()])
        .await;
    assert_eq!(denied, Err(Refusal::UrlRefused));
}

#[tokio::test]
async fn other_peers_get_no_answer_and_nothing_is_fetched() {
    let (_dir, socket, requests) = reader(own_uid().wrapping_add(1)).await;
    let client = PageReaderClient::new(&socket, own_uid());
    assert_eq!(
        client.read("https://page.example.com/", &[]).await,
        Err(Refusal::Unavailable)
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_client_refuses_a_socket_of_another_uid() {
    let (_dir, socket, requests) = reader(own_uid()).await;
    let client = PageReaderClient::new(&socket, own_uid().wrapping_add(1));
    assert_eq!(
        client.read("https://page.example.com/", &[]).await,
        Err(Refusal::Unavailable)
    );
    assert!(requests.lock().unwrap().is_empty());
    let missing = PageReaderClient::new("/nonexistent/reader.sock", own_uid());
    assert_eq!(
        missing.read("https://page.example.com/", &[]).await,
        Err(Refusal::Unavailable)
    );
}

#[tokio::test]
async fn malformed_requests_are_refused() {
    let (_dir, socket, requests) = reader(own_uid()).await;
    let five = vec!["a.example"; MAX_DENY_HOSTS + 1];
    for line in [
        "not json\n".to_string(),
        "{\"v\":2,\"url\":\"https://page.example.com/\"}\n".to_string(),
        "{\"v\":1,\"url\":\"https://page.example.com/\",\"x\":1}\n".to_string(),
        "{\"v\":1}\n".to_string(),
        format!("{{\"v\":1,\"url\":\"https://page.example.com/\",\"deny_hosts\":{five:?}}}\n"),
        format!(
            "{{\"v\":1,\"url\":\"https://page.example.com/{}\"}}\n",
            "a".repeat(MAX_REQUEST_BYTES)
        ),
        // No newline: the request never ends.
        "{\"v\":1,\"url\":\"https://page.example.com/\"}".to_string(),
    ] {
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        let _ = stream.write_all(line.as_bytes()).await;
        let _ = stream.shutdown().await;
        let mut reply = String::new();
        let read = stream.read_to_string(&mut reply).await;
        // Unread request bytes make the close a reset that may drop the reply.
        if line.len() > MAX_REQUEST_BYTES && read.is_err() {
            continue;
        }
        assert_eq!(
            reply, "{\"refused\":{\"code\":\"invalid_request\"}}\n",
            "{line}"
        );
    }
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bind_never_replaces_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.sock");
    std::fs::write(&path, b"x").unwrap();
    assert!(bind(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"x");
    std::fs::remove_file(&path).unwrap();
    drop(bind(&path).unwrap());
    // A stale socket of this uid is replaced.
    let listener = bind(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
    drop(listener);
}
