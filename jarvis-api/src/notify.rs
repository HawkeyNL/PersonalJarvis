//! Owner notifications through a self-hosted ntfy server (docs/NOTIFICATIONS.md).
//!
//! A push only says that something needs attention: no amounts, addresses,
//! device names or key fingerprints. Details stay behind the authenticated
//! app. Handlers enqueue without waiting; one worker publishes with a bounded
//! timeout and retry, and a full queue drops the event and counts it.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use url::{Host, Url};

/// Environment variable naming the full publish URL including the topic.
pub const URL_ENV: &str = "JARVIS_NOTIFY_NTFY_URL";
/// Publisher access token, delivered like the other Core secrets through a
/// root-managed systemd `EnvironmentFile`.
pub const TOKEN_ENV: &str = "JARVIS_NOTIFY_NTFY_TOKEN";

const QUEUE_CAPACITY: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_ATTEMPTS: u32 = 3;
/// At most one push per event kind in this window; bursts collapse.
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const MAX_TOKEN_BYTES: usize = 256;
const MAX_TOPIC_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotifyEvent {
    /// Monthly LLM spend reached 90% of the hard budget.
    BudgetNinetyPercent,
    /// A new device pairing request awaits approval in the app.
    PairingRequested,
}

impl NotifyEvent {
    fn kind(self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "budget",
            Self::PairingRequested => "pairing",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "Budget reached 90% this month",
            Self::PairingRequested => "New device wants to pair — open Jarvis to review",
        }
    }

    fn priority(self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "default",
            Self::PairingRequested => "high",
        }
    }

    fn tags(self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "moneybag",
            Self::PairingRequested => "key",
        }
    }

    /// Deep link the app opens when the notification is tapped.
    fn click(self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "jarvis://health/usage",
            Self::PairingRequested => "jarvis://approvals/pairing",
        }
    }
}

/// Cheap to clone. The default value is disabled and ignores every event.
#[derive(Clone, Default)]
pub struct Notifier {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    queue: mpsc::Sender<NotifyEvent>,
    last_sent: Mutex<HashMap<NotifyEvent, Instant>>,
    /// (year, month) in UTC of the last budget notification, matching the
    /// calendar month `jarvis_usage::month_total_eur` sums over.
    budget_month: Mutex<Option<(i32, u8)>>,
    dropped: AtomicU64,
}

impl Notifier {
    /// Validate the URL and token and spawn the publish worker. `Err` is a
    /// stable, non-sensitive reason; the caller keeps notifications disabled.
    pub fn start(raw_url: &str, token: &str) -> Result<Self, &'static str> {
        start_with(
            parse_url(raw_url)?,
            parse_token(token.as_bytes())?,
            REQUEST_TIMEOUT,
            FIRST_BACKOFF,
        )
    }

    fn with_queue(queue: mpsc::Sender<NotifyEvent>) -> Self {
        Self {
            inner: Some(Arc::new(Inner {
                queue,
                last_sent: Mutex::new(HashMap::new()),
                budget_month: Mutex::new(None),
                dropped: AtomicU64::new(0),
            })),
        }
    }

    /// Enqueue without waiting. Repeats within `MIN_INTERVAL` are coalesced;
    /// a full queue drops the event and counts it.
    pub fn notify(&self, event: NotifyEvent) {
        let Some(inner) = &self.inner else {
            return;
        };
        let now = Instant::now();
        {
            let mut last = inner
                .last_sent
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if last
                .get(&event)
                .is_some_and(|at| now.duration_since(*at) < MIN_INTERVAL)
            {
                return;
            }
            last.insert(event, now);
        }
        if inner.queue.try_send(event).is_err() {
            let dropped = inner.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                kind = event.kind(),
                dropped,
                "owner notification queue full; dropped"
            );
        }
    }

    /// Call after every refresh of this month's spend.
    // ponytail: the month is read here, not with the spend total; a refresh
    // racing the UTC month boundary can attribute last month's total to the
    // new month. Pass the month in from the usage query if that matters.
    pub fn budget_spend(&self, spent_cents: u64, budget_cents: u64) {
        if self.budget_crossed(spent_cents, budget_cents, current_month()) {
            self.notify(NotifyEvent::BudgetNinetyPercent);
        }
    }

    /// Startup: a month already at or above 90% counts as notified, so a
    /// restart does not repeat the push.
    pub fn seed_budget(&self, spent_cents: u64, budget_cents: u64) {
        self.budget_crossed(spent_cents, budget_cents, current_month());
    }

    /// True the first time in `month` that spend reaches 90% of the budget.
    // ponytail: in-memory marker, so a crash between crossing and delivery
    // loses that month's push; persist it if that ever matters.
    fn budget_crossed(&self, spent_cents: u64, budget_cents: u64, month: (i32, u8)) -> bool {
        let Some(inner) = &self.inner else {
            return false;
        };
        if budget_cents == 0 || u128::from(spent_cents) * 10 < u128::from(budget_cents) * 9 {
            return false;
        }
        let mut marker = inner
            .budget_month
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *marker == Some(month) {
            return false;
        }
        *marker = Some(month);
        true
    }
}

fn current_month() -> (i32, u8) {
    let now = time::OffsetDateTime::now_utc();
    (now.year(), u8::from(now.month()))
}

fn start_with(
    url: Url,
    token: HeaderValue,
    timeout: Duration,
    backoff: Duration,
) -> Result<Notifier, &'static str> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        // Never forward the token to another origin or a proxy.
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| "notification client unavailable")?;
    let (queue, receiver) = mpsc::channel(QUEUE_CAPACITY);
    tokio::spawn(run(client, url, token, receiver, backoff));
    Ok(Notifier::with_queue(queue))
}

async fn run(
    client: reqwest::Client,
    url: Url,
    token: HeaderValue,
    mut receiver: mpsc::Receiver<NotifyEvent>,
    backoff: Duration,
) {
    while let Some(event) = receiver.recv().await {
        let mut delay = backoff;
        for attempt in 1..=MAX_ATTEMPTS {
            match publish(&client, &url, &token, event).await {
                Ok(()) => {
                    tracing::info!(kind = event.kind(), "owner notification sent");
                    break;
                }
                Err(retry) if retry && attempt < MAX_ATTEMPTS => {
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(_) => {
                    tracing::warn!(kind = event.kind(), attempt, "owner notification failed");
                    break;
                }
            }
        }
    }
}

/// `Err(true)` is worth retrying (transport error, 429, 5xx).
async fn publish(
    client: &reqwest::Client,
    url: &Url,
    token: &HeaderValue,
    event: NotifyEvent,
) -> Result<(), bool> {
    let response = client
        .post(url.clone())
        .header(AUTHORIZATION, token.clone())
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("Title", "Jarvis")
        .header("Priority", event.priority())
        .header("Tags", event.tags())
        .header("Click", event.click())
        .body(event.message())
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => {
            let status = response.status();
            tracing::warn!(
                kind = event.kind(),
                status = status.as_u16(),
                "ntfy refused owner notification"
            );
            Err(status.as_u16() == 429 || status.is_server_error())
        }
        Err(error) => {
            // `without_url`: the topic is part of the URL and stays out of logs.
            tracing::warn!(kind = event.kind(), error = %error.without_url(), "ntfy unreachable");
            Err(true)
        }
    }
}

/// `http` only to a loopback address, otherwise `https`; no credentials,
/// query or fragment; the path is exactly one ntfy topic name.
pub fn parse_url(raw: &str) -> Result<Url, &'static str> {
    let url = Url::parse(raw.trim()).map_err(|_| "notification URL is invalid")?;
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    };
    let scheme_ok = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => loopback,
        _ => false,
    };
    if !scheme_ok {
        return Err("notification URL must be https or loopback http");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("notification URL must not carry credentials, query or fragment");
    }
    let topic = url.path().strip_prefix('/').unwrap_or_default();
    if topic.is_empty()
        || topic.len() > MAX_TOPIC_LEN
        || !topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("notification URL path must be a single ntfy topic");
    }
    Ok(url)
}

fn parse_token(raw: &[u8]) -> Result<HeaderValue, &'static str> {
    let token = std::str::from_utf8(raw)
        .map_err(|_| "notification token is invalid")?
        .trim();
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || !token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err("notification token is invalid");
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "notification token is invalid")?;
    value.set_sensitive(true);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc::UnboundedReceiver,
        time::timeout,
    };

    /// Loopback-only ntfy stand-in. `status: None` accepts and never answers.
    async fn mock(status: Option<u16>) -> (String, UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/jarvis-test", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let sender = sender.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let read = socket.read(&mut chunk).await.unwrap_or(0);
                        raw.extend_from_slice(&chunk[..read]);
                        let text = String::from_utf8_lossy(&raw).to_string();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let length = head
                                .lines()
                                .find_map(|line| line.strip_prefix("content-length: "))
                                .and_then(|value| value.parse::<usize>().ok())
                                .unwrap_or(0);
                            if body.len() >= length {
                                break;
                            }
                        }
                        if read == 0 {
                            break;
                        }
                    }
                    let _ = sender.send(String::from_utf8_lossy(&raw).to_string());
                    match status {
                        Some(code) => {
                            let reply = format!(
                                "HTTP/1.1 {code} Fixture\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            );
                            let _ = socket.write_all(reply.as_bytes()).await;
                        }
                        None => tokio::time::sleep(Duration::from_secs(30)).await,
                    }
                });
            }
        });
        (url, receiver)
    }

    fn fast(url: &str) -> Notifier {
        start_with(
            parse_url(url).unwrap(),
            parse_token(b"tk_fixture\n").unwrap(),
            Duration::from_millis(200),
            Duration::from_millis(10),
        )
        .unwrap()
    }

    async fn next(receiver: &mut UnboundedReceiver<String>) -> String {
        timeout(Duration::from_secs(5), receiver.recv())
            .await
            .expect("request within 5s")
            .unwrap()
    }

    async fn nothing_more(receiver: &mut UnboundedReceiver<String>) {
        assert!(timeout(Duration::from_millis(300), receiver.recv())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn publishes_a_generic_payload_with_the_bearer_token() {
        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(NotifyEvent::PairingRequested);
        let request = next(&mut requests).await;
        assert!(request.starts_with("POST /jarvis-test HTTP/1.1\r\n"));
        for header in [
            "authorization: Bearer tk_fixture\r\n",
            "title: Jarvis\r\n",
            "priority: high\r\n",
            "tags: key\r\n",
            "click: jarvis://approvals/pairing\r\n",
        ] {
            assert!(request.contains(header), "missing {header:?}");
        }
        assert!(request.ends_with("\r\n\r\nNew device wants to pair — open Jarvis to review"));
        nothing_more(&mut requests).await;

        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(NotifyEvent::BudgetNinetyPercent);
        let request = next(&mut requests).await;
        assert!(request.contains("priority: default\r\n"));
        assert!(request.contains("click: jarvis://health/usage\r\n"));
        assert!(request.ends_with("\r\n\r\nBudget reached 90% this month"));
    }

    #[tokio::test]
    async fn a_hanging_server_times_out_and_is_retried_three_times() {
        let (url, mut requests) = mock(None).await;
        fast(&url).notify(NotifyEvent::PairingRequested);
        for _ in 0..MAX_ATTEMPTS {
            next(&mut requests).await;
        }
        nothing_more(&mut requests).await;
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let (url, mut requests) = mock(Some(403)).await;
        fast(&url).notify(NotifyEvent::PairingRequested);
        next(&mut requests).await;
        nothing_more(&mut requests).await;
    }

    #[tokio::test]
    async fn a_full_queue_drops_and_counts() {
        let (queue, mut receiver) = mpsc::channel(1);
        let notifier = Notifier::with_queue(queue);
        notifier.notify(NotifyEvent::PairingRequested);
        notifier.notify(NotifyEvent::BudgetNinetyPercent);
        let inner = notifier.inner.as_ref().unwrap();
        assert_eq!(inner.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.try_recv(), Ok(NotifyEvent::PairingRequested));
    }

    #[tokio::test]
    async fn repeats_within_the_window_are_coalesced() {
        let (queue, mut receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        for _ in 0..5 {
            notifier.notify(NotifyEvent::PairingRequested);
        }
        notifier.notify(NotifyEvent::BudgetNinetyPercent);
        assert_eq!(receiver.try_recv(), Ok(NotifyEvent::PairingRequested));
        assert_eq!(receiver.try_recv(), Ok(NotifyEvent::BudgetNinetyPercent));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn budget_fires_once_per_calendar_month_at_ninety_percent() {
        let (queue, _receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        let october = (2026, 10);
        assert!(!notifier.budget_crossed(899, 1_000, october));
        assert!(notifier.budget_crossed(900, 1_000, october));
        assert!(!notifier.budget_crossed(950, 1_000, october));
        assert!(!notifier.budget_crossed(5_000, 1_000, october));
        assert!(notifier.budget_crossed(900, 1_000, (2026, 11)));
        assert!(!notifier.budget_crossed(u64::MAX, 0, (2026, 12)));
        assert!(!Notifier::default().budget_crossed(900, 1_000, (2027, 1)));
    }

    #[test]
    fn seeding_an_already_crossed_month_suppresses_the_push() {
        let (queue, mut receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        notifier.seed_budget(950, 1_000);
        notifier.budget_spend(990, 1_000);
        assert!(receiver.try_recv().is_err());
        let (queue, mut receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        notifier.seed_budget(100, 1_000);
        notifier.budget_spend(899, 1_000);
        assert!(receiver.try_recv().is_err());
        notifier.budget_spend(900, 1_000);
        assert_eq!(receiver.try_recv(), Ok(NotifyEvent::BudgetNinetyPercent));
        notifier.budget_spend(1_000, 1_000);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn url_must_be_loopback_http_or_https_with_one_topic() {
        for ok in [
            "http://127.0.0.1:2586/jarvis-owner",
            "http://[::1]:2586/jarvis_owner",
            "http://localhost:2586/abc",
            "https://ntfy.example.com/Topic-1",
        ] {
            assert!(parse_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "ntfy",
            "http://ntfy.example.com/topic",
            "http://192.168.1.10:2586/topic",
            "ftp://127.0.0.1/topic",
            "https://user:pass@ntfy.example.com/topic",
            "https://token@ntfy.example.com/topic",
            "https://ntfy.example.com/",
            "https://ntfy.example.com/a/b",
            "https://ntfy.example.com/topic?auth=x",
            "https://ntfy.example.com/topic#x",
            "https://ntfy.example.com/top.ic",
            &format!("https://ntfy.example.com/{}", "a".repeat(65)),
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn token_must_be_one_printable_word() {
        assert!(parse_token(b"tk_abc123\n").is_ok());
        for bad in [
            &b""[..],
            b" \n",
            b"tk abc",
            b"tk\x01abc",
            b"\xff\xfe",
            &[b'a'; MAX_TOKEN_BYTES + 1],
        ] {
            assert!(parse_token(bad).is_err());
        }
        assert!(parse_token(b"tk_abc").unwrap().is_sensitive());
    }
}
