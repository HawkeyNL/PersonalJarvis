//! Broker socket tests against the in-process IBKR fake from the OAuth
//! client tests.

use std::{
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    sync::oneshot,
    task::JoinHandle,
};

use super::*;
use crate::oauth::tests::{fake_client, Fake, ACCESS_TOKEN, CONSUMER_KEY, SECRET};

struct Broker {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    fake: Arc<Fake>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<std::io::Result<()>>,
}

fn own_uid() -> u32 {
    unsafe { libc::geteuid() }
}

async fn start(allowed: &[u32]) -> Broker {
    let (client, fake) = fake_client().await;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("broker.sock");
    let listener = server::bind(&socket).unwrap();
    let (stop, stopped) = oneshot::channel();
    let task = tokio::spawn(server::serve(
        listener,
        Arc::new(client),
        allowed.into(),
        async {
            let _ = stopped.await;
        },
    ));
    Broker {
        _dir: dir,
        socket,
        fake,
        stop,
        task,
    }
}

/// Send raw bytes, half-close, and read the whole reply.
async fn raw(socket: &Path, request: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    // The broker may answer and close before reading everything.
    let _ = stream.write_all(request).await;
    let _ = stream.shutdown().await;
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply).await;
    reply
}

fn error_of(reply: &[u8]) -> BrokerError {
    match serde_json::from_slice::<Reply<serde_json::Value>>(reply).unwrap() {
        Reply::Error(error) => error,
        Reply::Ok(data) => panic!("unexpected success: {data}"),
    }
}

#[tokio::test]
async fn typed_round_trip_for_every_read() {
    let broker = start(&[own_uid()]).await;
    let client = BrokerClient::new(&broker.socket, own_uid());
    assert_eq!(client.accounts().await.unwrap()[0].account_id, "DU1234567");
    let positions = client.positions("DU1234567").await.unwrap();
    assert_eq!(positions[1].position, Some("2.5".parse().unwrap()));
    let ledger = client.ledger("DU1234567").await.unwrap();
    assert_eq!(ledger["BASE"].cashbalance, Some("12.5".parse().unwrap()));
    let snapshot = client.snapshot(&[265598]).await.unwrap();
    assert_eq!(snapshot[0].last, Some("190.5".parse().unwrap()));
    assert_eq!(
        snapshot[0].availability,
        Some(crate::oauth::MarketDataAvailability::RealTime)
    );
    let history = client.history(265598, "1y", "1d").await.unwrap();
    assert_eq!(history.bars[0].low, Some("0.5".parse().unwrap()));
    // The fake has no search route: the upstream 404 maps to a generic error.
    assert_eq!(
        client.secdef_search("AAPL").await.unwrap_err(),
        BrokerError::Upstream
    );
    // Parameters are validated by the OAuth client before anything is sent.
    assert_eq!(
        client.positions("../iserver/account").await.unwrap_err(),
        BrokerError::InvalidInput {
            field: "account id".into()
        }
    );
    assert_eq!(broker.fake.count("/iserver/account"), 0);
}

#[test]
fn wire_format_is_stable() {
    let request = BrokerRequest::History {
        conid: 1,
        period: "1y".into(),
        bar: "1d".into(),
    };
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"kind":"history","conid":1,"period":"1y","bar":"1d"}"#
    );
    assert_eq!(
        serde_json::to_string(&BrokerRequest::Accounts {}).unwrap(),
        r#"{"kind":"accounts"}"#
    );
    assert_eq!(
        String::from_utf8(server::error_line(BrokerError::RetryLater {
            retry_after_secs: 5
        }))
        .unwrap(),
        "{\"error\":{\"code\":\"retry_later\",\"retry_after_secs\":5}}\n"
    );
}

#[tokio::test]
async fn refuses_a_peer_uid_that_is_not_allowlisted() {
    let broker = start(&[own_uid().wrapping_add(1)]).await;
    let reply = raw(&broker.socket, b"{\"kind\":\"accounts\"}\n").await;
    assert!(reply.is_empty(), "no reply to an unknown peer");
    assert_eq!(
        BrokerClient::new(&broker.socket, own_uid())
            .accounts()
            .await
            .unwrap_err(),
        BrokerError::Unavailable
    );
    assert!(broker.fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_unknown_malformed_and_oversize_requests() {
    let broker = start(&[own_uid()]).await;
    let mut oversize = br#"{"kind":"ledger","account_id":""#.to_vec();
    oversize.resize(MAX_REQUEST_BYTES + 10, b'A');
    oversize.extend_from_slice(b"\"}\n");
    for request in [
        &b"{\"kind\":\"place_order\",\"conid\":1}\n"[..],
        b"{\"kind\":\"whatif\"}\n",
        b"{\"kind\":\"cancel_order\"}\n",
        b"{\"kind\":\"switch_account\",\"account_id\":\"DU1\"}\n",
        b"{\"kind\":\"get\",\"path\":\"/iserver/account/orders\"}\n",
        b"{\"kind\":\"accounts\",\"path\":\"/x\"}\n",
        b"{\"kind\":\"ledger\",\"account_id\":\"DU1234567\",\"extra\":1}\n",
        b"{\"kind\":\"ledger\"}\n",
        b"{\"kind\":\"positions\",\"account_id\":\"DU1234567\",\"page\":2}\n",
        b"{\"kind\":\"secdef_search\",\"symbol\":\"AAPL\",\"sec_type\":\"OPT\"}\n",
        b"{\"kind\":\"snapshot\",\"conids\":[1],\"fields\":\"7762\"}\n",
        b"{\"kind\":\"history\",\"conid\":1,\"period\":\"1y\",\"bar\":\"1d\",\"outsideRth\":true}\n",
        b"{\"kind\":\"accounts\",\"kind\":\"positions\"}\n",
        b"{\"kind\":\"snapshot\",\"conids\":[\"1\"]}\n",
        b"not json\n",
        b"{\"kind\":\"accounts\"}",
        b"",
        &oversize,
    ] {
        let reply = raw(&broker.socket, request).await;
        assert_eq!(
            error_of(&reply),
            BrokerError::InvalidRequest,
            "{}",
            String::from_utf8_lossy(&request[..request.len().min(80)])
        );
    }
    assert!(broker.fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_silent_peer_times_out_without_holding_the_broker() {
    let broker = start(&[own_uid()]).await;
    let mut idle = UnixStream::connect(&broker.socket).await.unwrap();
    let mut reply = Vec::new();
    let started = std::time::Instant::now();
    idle.read_to_end(&mut reply).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(error_of(&reply), BrokerError::Timeout);
}

#[tokio::test]
async fn refuses_connections_beyond_the_concurrency_bound() {
    let broker = start(&[own_uid()]).await;
    // Silent peers hold every slot until their read timeout.
    let mut idle = Vec::new();
    for _ in 0..8 {
        idle.push(UnixStream::connect(&broker.socket).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        BrokerClient::new(&broker.socket, own_uid())
            .accounts()
            .await
            .unwrap_err(),
        BrokerError::Busy
    );
    assert!(broker.fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn keep_alive_tickles_only_an_open_brokerage_session() {
    let (client, fake) = fake_client().await;
    client.keep_alive().await.unwrap();
    assert!(
        fake.requests.lock().unwrap().is_empty(),
        "no mint when idle"
    );
    client.history(265598, "1y", "1d").await.unwrap();
    client.keep_alive().await.unwrap();
    assert_eq!(fake.count("/tickle"), 1);
    // The fake reports the session lost: the next read re-opens it.
    client.keep_alive().await.unwrap();
    assert_eq!(fake.count("/tickle"), 1);
    client.history(265598, "1y", "1d").await.unwrap();
    assert_eq!(fake.count("/ssodh/init"), 2);
    // A failing tickle is not retried every interval.
    fake.fail.lock().unwrap().insert("/tickle", 500);
    client.keep_alive().await.unwrap_err();
    client.keep_alive().await.unwrap();
    assert_eq!(fake.count("/tickle"), 2);
}

#[tokio::test]
async fn client_refuses_a_socket_served_by_another_uid() {
    let broker = start(&[own_uid()]).await;
    let client = BrokerClient::new(&broker.socket, own_uid().wrapping_add(1));
    assert_eq!(
        client.accounts().await.unwrap_err(),
        BrokerError::Unavailable
    );
    assert!(broker.fake.requests.lock().unwrap().is_empty());
}

#[test]
fn masks_account_ids_to_their_last_three_characters() {
    assert_eq!(server::mask("DU1234567"), "***567");
    assert_eq!(server::mask("U12"), "***");
    assert_eq!(server::mask(""), "invalid");
    assert_eq!(server::mask("DU12345é7"), "invalid");
    assert_eq!(server::mask("../iserver"), "invalid");
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn audit_lines_and_errors_carry_no_secrets_urls_or_account_ids() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || writer.clone())
        .finish();
    // Global, not scoped: a scoped subscriber misses callsites that parallel
    // tests hit first. Lines from those tests land here too, which only
    // widens the leak check.
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let broker = start(&[own_uid()]).await;
    let client = BrokerClient::new(&broker.socket, own_uid());
    client.positions("DU1234567").await.unwrap();
    broker.fake.fail.lock().unwrap().insert("/ledger", 500);
    let ledger = client.ledger("DU1234567").await.unwrap_err();
    broker.fake.fail.lock().unwrap().insert("/history", 401);
    let history = client.history(4242424, "1y", "1d").await.unwrap_err();
    assert_eq!(ledger, BrokerError::Upstream);
    assert_eq!(history, BrokerError::UpstreamAuth);
    let reply = raw(
        &broker.socket,
        b"{\"kind\":\"ledger\",\"account_id\":\"DU1234567\"}\n",
    )
    .await;

    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("***567"), "{logs}");
    assert!(logs.contains("\"kind\":\"ledger\""), "{logs}");
    let replies = format!(
        "{ledger} {ledger:?} {history} {history:?} {}",
        String::from_utf8_lossy(&reply)
    );
    for text in [&logs, &replies] {
        for leaked in [
            "DU1234567",
            "4242424",
            "127.0.0.1",
            "/portfolio",
            "/iserver",
            "http",
            CONSUMER_KEY,
            ACCESS_TOKEN,
            &hex::encode(SECRET),
        ] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
    }
}

#[tokio::test]
async fn graceful_shutdown_stops_accepting_and_finishes() {
    let broker = start(&[own_uid()]).await;
    let client = BrokerClient::new(&broker.socket, own_uid());
    client.accounts().await.unwrap();
    broker.stop.send(()).unwrap();
    broker.task.await.unwrap().unwrap();
    assert_eq!(
        client.accounts().await.unwrap_err(),
        BrokerError::Unavailable
    );
}

#[test]
fn resolves_numeric_and_named_peers() {
    assert_eq!(server::resolve_peers("1000, root").unwrap(), [1000, 0]);
    assert!(server::resolve_peers("").is_err());
    assert!(server::resolve_peers(" , ").is_err());
    assert!(server::resolve_peers("jarvis-no-such-user-x").is_err());
    assert!(server::resolve_peers("99999999999").is_err());
}

#[tokio::test]
async fn bind_replaces_only_a_stale_socket() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-socket");
    std::fs::write(&file, b"keep").unwrap();
    assert!(server::bind(&file).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    let socket = dir.path().join("broker.sock");
    drop(server::bind(&socket).unwrap());
    server::bind(&socket).unwrap();
}
