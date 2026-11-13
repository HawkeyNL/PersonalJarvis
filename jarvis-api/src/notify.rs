//! Owner notifications through a self-hosted ntfy server and, for approvals,
//! APNs straight to the Jarvis iOS app (docs/NOTIFICATIONS.md).
//!
//! An approval push names the device behind it (name, platform, model, IP and
//! the start of the key fingerprint): the owner's choice of 2026-11-13. The
//! text passes the ntfy and Apple servers; approving still needs the app's
//! signed flow. Handlers enqueue without waiting; one worker publishes with a
//! bounded timeout and retry, and a full queue drops the event and counts it.

pub mod apns;

use std::{
    collections::HashMap,
    fmt::Write as _,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use tokio::sync::mpsc;
use url::{Host, Url};
use uuid::Uuid;

/// Environment variable naming the full publish URL including the topic.
pub const URL_ENV: &str = "JARVIS_NOTIFY_NTFY_URL";
/// Publisher access token, delivered like the other Core secrets through a
/// root-managed systemd `EnvironmentFile`.
pub const TOKEN_ENV: &str = "JARVIS_NOTIFY_NTFY_TOKEN";

const QUEUE_CAPACITY: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_ATTEMPTS: u32 = 3;
/// Longest one event may spend on app pushes, so an unreachable APNs never
/// holds the worker past the approval's two-minute lifetime.
const APP_PUSH_BUDGET: Duration = Duration::from_secs(30);
/// At most one push per event kind in this window; bursts collapse.
const MIN_INTERVAL: Duration = Duration::from_secs(60);
const MAX_TOKEN_BYTES: usize = 256;
const MAX_TOPIC_LEN: usize = 64;

/// Fixed ASCII (an ntfy header) with the cue a collapsed preview still shows.
const PAIRING_TITLE: &str = "New device wants to pair - verify in Jarvis";
/// Longest device name shown in a push, in characters.
const MAX_NAME_CHARS: usize = 40;
/// Longest platform and model shown in a push, in characters.
const MAX_PLATFORM_CHARS: usize = 32;
/// Longest reminder title shown in a push, in characters.
const MAX_REMINDER_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyEvent {
    /// Monthly LLM spend reached 90% of the hard budget.
    BudgetNinetyPercent,
    /// A new device pairing request awaits approval in the app.
    PairingRequested(DeviceInfo),
    /// Another pairing request arrived inside the coalescing window; fixed
    /// text, so it cannot leave stale device details on the phone.
    PairingAgain,
    /// A signed-in device asks another device to approve its unlock.
    UnlockRequested(DeviceInfo),
    /// A scheduled job has something for the owner.
    JobResult(JobResult),
}

/// What a scheduled job's push says (docs/SCHEDULED_JOBS.md).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobResult {
    /// A research run ended; content-free, the answer stays in Jarvis.
    Prompt,
    /// A reminder with the owner's title, cleaned like a device name.
    Reminder(String),
    /// A Home Node disk at or above the job's threshold.
    Disk { label: &'static str, percent: u8 },
}

/// The device behind an approval push, as Core stored it. Untrusted: a new
/// device chooses its own name, so [`NotifyEvent::alert`] cleans every field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub platform: String,
    pub model: Option<String>,
    pub ip: Option<String>,
    /// Lowercase hex SHA-256 of the device key.
    pub fingerprint: Option<String>,
}

impl NotifyEvent {
    fn kind(&self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "budget",
            Self::PairingRequested(_) | Self::PairingAgain => "pairing",
            Self::UnlockRequested(_) => "unlock",
            Self::JobResult(JobResult::Prompt) => "job",
            Self::JobResult(JobResult::Reminder(_)) => "reminder",
            Self::JobResult(JobResult::Disk { .. }) => "disk",
        }
    }

    /// Title and body, the same for ntfy and APNs. Titles are fixed ASCII
    /// because ntfy takes the title as an HTTP header; device text only ever
    /// goes into the body.
    fn alert(&self) -> (&'static str, String) {
        match self {
            Self::BudgetNinetyPercent => ("Jarvis", "Budget reached 90% this month".to_string()),
            // Trusted fields first; the device-chosen name and model come
            // quoted and labelled, so they cannot pose as the IP or the key.
            Self::PairingRequested(device) => {
                let mut body = String::new();
                if let Some(ip) = device
                    .ip
                    .as_deref()
                    .and_then(|ip| ip.parse::<IpAddr>().ok())
                {
                    let _ = write!(body, "IP {ip} · ");
                }
                let _ = write!(body, "{} · {}", platform(device), named(device));
                if let Some(model) = device.model.as_deref() {
                    let model = clean(&unspoofable(model), MAX_PLATFORM_CHARS, "");
                    if !model.is_empty() {
                        let _ = write!(body, " ({model})");
                    }
                }
                match device
                    .fingerprint
                    .as_deref()
                    .filter(|key| crate::validation::is_hex_of_len(key, 64))
                {
                    Some(key) => {
                        let _ = write!(
                            body,
                            "\nKey {} {} {} {}… Compare the full fingerprint in Jarvis before approving.",
                            &key[..4],
                            &key[4..8],
                            &key[8..12],
                            &key[12..16]
                        );
                    }
                    None => {
                        body.push_str("\nCompare the key fingerprint in Jarvis before approving.")
                    }
                }
                (PAIRING_TITLE, body)
            }
            Self::PairingAgain => (
                PAIRING_TITLE,
                "Another device also asks to pair — check every request in Jarvis.".to_string(),
            ),
            Self::UnlockRequested(device) => (
                "Device asks to unlock Jarvis",
                format!(
                    "{} device {}. Open Jarvis to approve or deny.",
                    platform(device),
                    named(device)
                ),
            ),
            Self::JobResult(JobResult::Prompt) => (
                "Jarvis",
                "A scheduled job has a result — open Jarvis".to_string(),
            ),
            Self::JobResult(JobResult::Reminder(title)) => (
                "Jarvis reminder",
                clean(&unspoofable(title), MAX_REMINDER_CHARS, "Reminder"),
            ),
            Self::JobResult(JobResult::Disk { label, percent }) => {
                ("Jarvis", format!("Disk {label} at {percent}%"))
            }
        }
    }

    fn priority(&self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent | Self::JobResult(_) => "default",
            Self::PairingRequested(_) | Self::PairingAgain | Self::UnlockRequested(_) => "high",
        }
    }

    fn tags(&self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "moneybag",
            Self::PairingRequested(_) | Self::PairingAgain => "key",
            Self::UnlockRequested(_) => "lock",
            Self::JobResult(JobResult::Prompt) => "mag",
            Self::JobResult(JobResult::Reminder(_)) => "bell",
            Self::JobResult(JobResult::Disk { .. }) => "floppy_disk",
        }
    }

    /// Deep link the app opens when the notification is tapped.
    fn click(&self) -> &'static str {
        match self {
            Self::BudgetNinetyPercent => "jarvis://health/usage",
            Self::PairingRequested(_) | Self::PairingAgain => "jarvis://approvals/pairing",
            Self::UnlockRequested(_) => "jarvis://approvals/unlock",
            Self::JobResult(_) => "jarvis://jobs",
        }
    }
}

/// The device-chosen name in fixed quotes, or `without a name`.
fn named(device: &DeviceInfo) -> String {
    match clean(&unspoofable(&device.name), MAX_NAME_CHARS, "") {
        name if name.is_empty() => "without a name".to_string(),
        name => format!("named “{name}”"),
    }
}

/// Device text that cannot fake a field or a link: separators and quotes
/// become `-`; `/` and `:` (which ntfy turns into links) are dropped.
// ponytail: a denylist of look-alikes; switch to an allowlist of letters,
// digits and plain punctuation if it keeps growing.
fn unspoofable(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '/' | ':' | '／' | '：' | '∕'))
        .map(|c| match c {
            '·' | '\u{387}' | '•' | '‧' | '∙' | '⋅' | '⸱' | '・' | '･' | '"' | '\'' | '`' | '“'
            | '”' | '‘' | '’' | '„' | '‟' | '«' | '»' | '‹' | '›' | '「' | '」' | '＂' | '＇'
            | '❝' | '❞' | '〝' | '〞' | '״' => '-',
            c => c,
        })
        .collect()
}

fn platform(device: &DeviceInfo) -> String {
    match device.platform.as_str() {
        "ios" => "iOS".to_string(),
        "macos" => "macOS".to_string(),
        "android" => "Android".to_string(),
        "windows" => "Windows".to_string(),
        "linux" => "Linux".to_string(),
        other => clean(&unspoofable(other), MAX_PLATFORM_CHARS, "Unknown"),
    }
}

/// Device-chosen text made safe to show: control, bidi and other hidden
/// characters dropped, whitespace runs (line breaks included) collapsed to one
/// space, at most `max` characters; `fallback` when nothing visible is left.
fn clean(value: &str, max: usize, fallback: &str) -> String {
    let visible: String = value
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !crate::validation::is_hidden_char(*c))
        .collect();
    let words = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        fallback.to_string()
    } else if words.chars().count() <= max {
        words
    } else {
        words.chars().take(max - 1).chain(['…']).collect()
    }
}

/// Whose devices an approval push goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Audience {
    pub user: Uuid,
    /// The device that asked; it never receives its own approval push.
    pub except: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    event: NotifyEvent,
    /// Only approvals have one; they also go to the user's app push targets.
    audience: Option<Audience>,
}

/// The validated ntfy publish URL and bearer token.
pub struct Ntfy {
    url: Url,
    token: HeaderValue,
}

impl Ntfy {
    pub fn parse(raw_url: &str, token: &str) -> Result<Self, &'static str> {
        Ok(Self {
            url: parse_url(raw_url)?,
            token: parse_token(token.as_bytes())?,
        })
    }
}

/// Cheap to clone. The default value is disabled and ignores every event.
#[derive(Clone, Default)]
pub struct Notifier {
    inner: Option<Arc<Inner>>,
}

type WindowKey = (&'static str, Option<Uuid>);

struct Inner {
    queue: mpsc::Sender<Job>,
    /// Per kind and requesting device, so one device's request never hides
    /// another's: the window start, and whether its one `PairingAgain` went
    /// out.
    last_sent: Mutex<HashMap<WindowKey, (Instant, bool)>>,
    /// (year, month) in UTC of the last budget notification, matching the
    /// calendar month `jarvis_usage::month_total_eur` sums over.
    budget_month: Mutex<Option<(i32, u8)>>,
    dropped: AtomicU64,
}

impl Notifier {
    /// Spawn the publish worker for the configured channels; with neither,
    /// notifications stay off. `Err` is a stable, non-sensitive reason.
    pub fn start(
        ntfy: Option<Ntfy>,
        apns: Option<(apns::Config, jarvis_store::Database)>,
    ) -> Result<Self, &'static str> {
        let apns = apns
            .map(|(config, db)| apns::Apns::new(config, db, REQUEST_TIMEOUT))
            .transpose()?;
        if ntfy.is_none() && apns.is_none() {
            return Ok(Self::default());
        }
        start_with(ntfy, apns, REQUEST_TIMEOUT, FIRST_BACKOFF)
    }

    fn with_queue(queue: mpsc::Sender<Job>) -> Self {
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
        self.enqueue(Job {
            event,
            audience: None,
        });
    }

    /// Like `notify`, and also push to all of `user`'s app push targets.
    pub fn notify_user(&self, event: NotifyEvent, user: Uuid) {
        self.enqueue(Job {
            event,
            audience: Some(Audience { user, except: None }),
        });
    }

    /// Like `notify`, and also push to `user`'s app push targets except the
    /// requesting device.
    pub fn notify_approval(&self, event: NotifyEvent, user: Uuid, except: Option<Uuid>) {
        self.enqueue(Job {
            event,
            audience: Some(Audience { user, except }),
        });
    }

    /// The first event in a window wins. The first later pairing request in
    /// the window sends one `PairingAgain` (fixed text, same collapse id);
    /// anything after that sends nothing until the window ends.
    fn enqueue(&self, mut job: Job) {
        let kind = job.event.kind();
        let key = (kind, job.audience.and_then(|audience| audience.except));
        let Some(inner) = &self.inner else {
            return;
        };
        let now = Instant::now();
        {
            let mut last = inner
                .last_sent
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match last.get_mut(&key) {
                Some((at, again)) if now.duration_since(*at) < MIN_INTERVAL => {
                    if *again || !matches!(job.event, NotifyEvent::PairingRequested(_)) {
                        return;
                    }
                    *again = true;
                    job.event = NotifyEvent::PairingAgain;
                }
                _ => {
                    last.insert(key, (now, false));
                }
            }
        }
        if inner.queue.try_send(job).is_err() {
            let dropped = inner.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(kind, dropped, "owner notification queue full; dropped");
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
    ntfy: Option<Ntfy>,
    apns: Option<apns::Apns>,
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
    tokio::spawn(run(client, ntfy, apns, receiver, backoff));
    Ok(Notifier::with_queue(queue))
}

async fn run(
    client: reqwest::Client,
    ntfy: Option<Ntfy>,
    mut apns: Option<apns::Apns>,
    mut receiver: mpsc::Receiver<Job>,
    backoff: Duration,
) {
    while let Some(job) = receiver.recv().await {
        // The app push first: an approval expires within minutes, and a slow
        // ntfy must not hold it back.
        if let (Some(apns), Some(audience)) = (apns.as_mut(), job.audience) {
            apns.send(&job.event, audience, backoff).await;
        }
        if let Some(ntfy) = &ntfy {
            publish_with_retry(&client, ntfy, &job.event, backoff).await;
        }
    }
}

async fn publish_with_retry(
    client: &reqwest::Client,
    ntfy: &Ntfy,
    event: &NotifyEvent,
    backoff: Duration,
) {
    let mut delay = backoff;
    for attempt in 1..=MAX_ATTEMPTS {
        match publish(client, &ntfy.url, &ntfy.token, event).await {
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

/// `Err(true)` is worth retrying (transport error, 429, 5xx).
async fn publish(
    client: &reqwest::Client,
    url: &Url,
    token: &HeaderValue,
    event: &NotifyEvent,
) -> Result<(), bool> {
    let (title, body) = event.alert();
    let response = client
        .post(url.clone())
        .header(AUTHORIZATION, token.clone())
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("Title", title)
        .header("Priority", event.priority())
        .header("Tags", event.tags())
        .header("Click", event.click())
        .body(body)
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

    #[test]
    fn job_pushes_say_little_and_clean_the_title() {
        let (title, body) = NotifyEvent::JobResult(JobResult::Prompt).alert();
        assert_eq!(
            (title, body.as_str()),
            ("Jarvis", "A scheduled job has a result — open Jarvis")
        );
        let reminder = NotifyEvent::JobResult(JobResult::Reminder(format!(
            "Call\u{202E} the\nbank: https://x \"now\" {}",
            "a".repeat(80)
        )));
        let (title, body) = reminder.alert();
        assert_eq!(title, "Jarvis reminder");
        assert!(body.starts_with("Call the bank httpsx -now- aaa"), "{body}");
        assert_eq!(body.chars().count(), MAX_REMINDER_CHARS);
        assert_eq!(
            NotifyEvent::JobResult(JobResult::Reminder(" ".into()))
                .alert()
                .1,
            "Reminder"
        );
        let disk = NotifyEvent::JobResult(JobResult::Disk {
            label: "data",
            percent: 91,
        });
        assert_eq!(disk.alert().1, "Disk data at 91%");
        assert_eq!(disk.click(), "jarvis://jobs");
        for event in [reminder, disk, NotifyEvent::JobResult(JobResult::Prompt)] {
            assert!(event.alert().0.is_ascii());
        }
    }
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
            Some(Ntfy::parse(url, "tk_fixture\n").unwrap()),
            None,
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

    fn pairing() -> NotifyEvent {
        NotifyEvent::PairingRequested(DeviceInfo {
            name: "Gus’s iPad".into(),
            platform: "ios".into(),
            model: Some("iPad14,1".into()),
            ip: Some("192.0.2.7".into()),
            fingerprint: Some("a1b2c3d4e5f60718".repeat(4)),
        })
    }

    fn unlock() -> NotifyEvent {
        NotifyEvent::UnlockRequested(DeviceInfo {
            name: "Work laptop".into(),
            platform: "linux".into(),
            ..DeviceInfo::default()
        })
    }

    #[tokio::test]
    async fn publishes_the_alert_with_the_bearer_token() {
        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(pairing());
        let request = next(&mut requests).await;
        assert!(request.starts_with("POST /jarvis-test HTTP/1.1\r\n"));
        for header in [
            "authorization: Bearer tk_fixture\r\n",
            "title: New device wants to pair - verify in Jarvis\r\n",
            "priority: high\r\n",
            "tags: key\r\n",
            "click: jarvis://approvals/pairing\r\n",
        ] {
            assert!(request.contains(header), "missing {header:?}");
        }
        assert!(request.ends_with(
            "\r\n\r\nIP 192.0.2.7 · iOS · named “Gus-s iPad” (iPad14,1)\n\
             Key a1b2 c3d4 e5f6 0718… Compare the full fingerprint in Jarvis before approving."
        ));
        nothing_more(&mut requests).await;

        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(NotifyEvent::BudgetNinetyPercent);
        let request = next(&mut requests).await;
        assert!(request.contains("title: Jarvis\r\n"));
        assert!(request.contains("priority: default\r\n"));
        assert!(request.contains("click: jarvis://health/usage\r\n"));
        assert!(request.ends_with("\r\n\r\nBudget reached 90% this month"));

        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(unlock());
        let request = next(&mut requests).await;
        for header in [
            "title: Device asks to unlock Jarvis\r\n",
            "priority: high\r\n",
            "tags: lock\r\n",
            "click: jarvis://approvals/unlock\r\n",
        ] {
            assert!(request.contains(header), "missing {header:?}");
        }
        assert!(request.ends_with(
            "\r\n\r\nLinux device named “Work laptop”. Open Jarvis to approve or deny."
        ));
    }

    #[tokio::test]
    async fn a_hostile_device_name_cannot_add_ntfy_headers() {
        let (url, mut requests) = mock(Some(200)).await;
        fast(&url).notify(NotifyEvent::UnlockRequested(DeviceInfo {
            name: "x\r\nTitle: Fake\r\nX-Evil: 1".into(),
            platform: "ios".into(),
            ..DeviceInfo::default()
        }));
        let request = next(&mut requests).await;
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.contains("title: Device asks to unlock Jarvis\r\n"));
        assert!(!head.to_ascii_lowercase().contains("evil"));
        assert!(!head.contains("Fake"));
        assert_eq!(
            body,
            "iOS device named “x Title Fake X-Evil 1”. Open Jarvis to approve or deny."
        );
    }

    #[test]
    fn device_text_is_cleaned_capped_and_falls_back() {
        // Titles go out as ntfy HTTP headers: fixed ASCII only.
        for event in [
            NotifyEvent::BudgetNinetyPercent,
            pairing(),
            NotifyEvent::PairingAgain,
            unlock(),
        ] {
            let (title, body) = event.alert();
            assert!(title.is_ascii() && HeaderValue::from_str(title).is_ok());
            assert!(!body.is_empty());
        }
        assert_eq!(clean("  Gus’s\t\n iPhone  ", 40, "-"), "Gus’s iPhone");
        assert_eq!(
            clean(
                "a\u{202e}b\u{200b}c\u{2066}d\u{1b}[31me\u{feff}\u{e0041}",
                40,
                "-"
            ),
            "abcd[31me"
        );
        assert_eq!(
            clean("line\u{2028}break\u{85}next", 40, "-"),
            "line break next"
        );
        assert_eq!(clean("\u{200b}\u{202e} \n", 40, "fallback"), "fallback");
        // Capped by characters, not bytes; the cut is marked.
        assert_eq!(clean(&"é".repeat(40), 40, "-"), "é".repeat(40));
        assert_eq!(
            clean(&"😀".repeat(41), 40, "-"),
            format!("{}…", "😀".repeat(39))
        );

        let (_, body) = NotifyEvent::PairingRequested(DeviceInfo::default()).alert();
        assert_eq!(
            body,
            "Unknown · without a name\n\
             Compare the key fingerprint in Jarvis before approving."
        );
        let (_, body) = NotifyEvent::PairingRequested(DeviceInfo {
            name: "Phone".into(),
            platform: "plan9\u{202e}".into(),
            model: Some(" \u{200b} ".into()),
            ip: Some("192.0.2.7\nX".into()),
            fingerprint: Some("zz".repeat(32)),
        })
        .alert();
        assert_eq!(
            body,
            "plan9 · named “Phone”\nCompare the key fingerprint in Jarvis before approving."
        );
        let (_, body) = NotifyEvent::UnlockRequested(DeviceInfo {
            name: format!("{}\u{202e}", "n".repeat(60)),
            platform: "p".repeat(60),
            ..DeviceInfo::default()
        })
        .alert();
        assert_eq!(
            body,
            format!(
                "{}… device named “{}…”. Open Jarvis to approve or deny.",
                "p".repeat(31),
                "n".repeat(39)
            )
        );
    }

    #[tokio::test]
    async fn a_hanging_server_times_out_and_is_retried_three_times() {
        let (url, mut requests) = mock(None).await;
        fast(&url).notify(pairing());
        for _ in 0..MAX_ATTEMPTS {
            next(&mut requests).await;
        }
        nothing_more(&mut requests).await;
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let (url, mut requests) = mock(Some(403)).await;
        fast(&url).notify(pairing());
        next(&mut requests).await;
        nothing_more(&mut requests).await;
    }

    #[tokio::test]
    async fn a_full_queue_drops_and_counts() {
        let (queue, mut receiver) = mpsc::channel(1);
        let notifier = Notifier::with_queue(queue);
        notifier.notify(pairing());
        notifier.notify(NotifyEvent::BudgetNinetyPercent);
        let inner = notifier.inner.as_ref().unwrap();
        assert_eq!(inner.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(receiver.try_recv().map(|job| job.event), Ok(pairing()));
    }

    #[test]
    fn a_device_name_cannot_pose_as_the_ip_or_the_key() {
        let (_, body) = NotifyEvent::PairingRequested(DeviceInfo {
            name: "A · IP 1.2.3.4\nKey a1b2 c3d4 “x” http://e".into(),
            platform: "ios".into(),
            model: Some("M · IP 5.6.7.8".into()),
            ip: Some("192.0.2.7".into()),
            fingerprint: Some("0123456789abcdef".repeat(4)),
        })
        .alert();
        assert_eq!(
            body,
            "IP 192.0.2.7 · iOS · named “A - IP 1.2.3.4 Key a1b2 c3d4 -x- httpe” (M - IP 5.6.7.8)\n\
             Key 0123 4567 89ab cdef… Compare the full fingerprint in Jarvis before approving."
        );
        let (_, body) = NotifyEvent::UnlockRequested(DeviceInfo {
            name: "“Phone” · iOS".into(),
            platform: "linux".into(),
            ..DeviceInfo::default()
        })
        .alert();
        assert_eq!(
            body,
            "Linux device named “-Phone- - iOS”. Open Jarvis to approve or deny."
        );
        let look_alikes = "\u{387}\u{2027}\u{ff65}\u{2e31}❝❞〝〞״";
        assert_eq!(
            unspoofable(&format!("a{look_alikes}b")),
            format!("a{}b", "-".repeat(look_alikes.chars().count()))
        );
    }

    #[tokio::test]
    async fn repeats_within_the_window_are_coalesced() {
        let (queue, mut receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        for _ in 0..5 {
            notifier.notify(pairing());
        }
        notifier.notify(NotifyEvent::BudgetNinetyPercent);
        assert_eq!(receiver.try_recv().map(|job| job.event), Ok(pairing()));
        // One fixed-text follow-up per window, never the later device's text.
        assert_eq!(
            receiver.try_recv().map(|job| job.event),
            Ok(NotifyEvent::PairingAgain)
        );
        assert_eq!(
            NotifyEvent::PairingAgain.alert(),
            (
                "New device wants to pair - verify in Jarvis",
                "Another device also asks to pair — check every request in Jarvis.".to_string()
            )
        );
        assert_eq!(
            receiver.try_recv().map(|job| job.event),
            Ok(NotifyEvent::BudgetNinetyPercent)
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn only_approvals_carry_an_app_push_audience() {
        let (queue, mut receiver) = mpsc::channel(8);
        let notifier = Notifier::with_queue(queue);
        let (user, device) = (Uuid::now_v7(), Uuid::now_v7());
        notifier.notify_approval(unlock(), user, Some(device));
        notifier.notify(NotifyEvent::BudgetNinetyPercent);
        // Coalesced per kind and requesting device; another device's request
        // still goes out.
        notifier.notify_approval(unlock(), user, Some(device));
        let other = Uuid::now_v7();
        notifier.notify_approval(unlock(), user, Some(other));
        assert_eq!(
            receiver.try_recv(),
            Ok(Job {
                event: unlock(),
                audience: Some(Audience {
                    user,
                    except: Some(device)
                }),
            })
        );
        assert_eq!(
            receiver.try_recv(),
            Ok(Job {
                event: NotifyEvent::BudgetNinetyPercent,
                audience: None,
            })
        );
        assert_eq!(
            receiver.try_recv().map(|job| job.audience),
            Ok(Some(Audience {
                user,
                except: Some(other)
            }))
        );
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
        assert_eq!(
            receiver.try_recv().map(|job| job.event),
            Ok(NotifyEvent::BudgetNinetyPercent)
        );
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
