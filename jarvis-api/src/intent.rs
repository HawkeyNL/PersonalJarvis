//! Advisory, bounded task classification. No classifier authorizes an action or
//! names an executable, provider, model, device, agent, or filesystem path.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::{redirect::Policy, Client, Url};
use serde::Deserialize;
use serde_json::json;

use crate::{metering::record_usage_with_metadata, AppState};

const OFFICIAL_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MAX_STATE_BYTES: usize = 4_096;
const MAX_RESPONSE_BYTES: usize = 16_384;
const DEFAULT_JEV_CONFIDENCE: f64 = 0.75;
// This is a virtual HTTP authority only. reqwest connects exclusively through
// the root-owned systemd Unix socket, so there is no DNS or TCP listener.
const LOCAL_ENDPOINT: &str = "http://jarvis-laya.local/v1/systemone";
const LOCAL_SOCKET: &str = "/run/jarvis-laya.sock";
pub(crate) const MAX_BILLABLE_INPUT_TOKENS: u32 = 4_096;
pub(crate) const MAX_BILLABLE_OUTPUT_TOKENS: u32 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkKind {
    Conversation,
    QuickAnswer,
    Research,
    Coding,
    ActionRequest,
}

impl WorkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::QuickAnswer => "quick_answer",
            Self::Research => "research",
            Self::Coding => "coding",
            Self::ActionRequest => "action_request",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "conversation" => Some(Self::Conversation),
            "quick_answer" => Some(Self::QuickAnswer),
            "research" => Some(Self::Research),
            "coding" => Some(Self::Coding),
            "action_request" => Some(Self::ActionRequest),
            _ => None,
        }
    }
}

/// A classifier may recommend a semantic route, but cannot supply a concrete model or
/// override an explicit owner-selected mode.
pub(crate) fn route_mode(
    requested: jarvis_llm::RoutingMode,
    kind: Option<WorkKind>,
) -> jarvis_llm::RoutingMode {
    use jarvis_llm::RoutingMode;
    if requested != RoutingMode::Auto {
        return requested;
    }
    match kind {
        Some(WorkKind::QuickAnswer) => RoutingMode::Fast,
        Some(WorkKind::Research) => RoutingMode::Research,
        Some(WorkKind::Coding | WorkKind::ActionRequest) => RoutingMode::Deep,
        Some(WorkKind::Conversation) | None => RoutingMode::Auto,
    }
}

/// A pinned owner brain is an explicit choice, not an Auto-routing hint.
pub(crate) fn should_classify(
    requested: jarvis_llm::RoutingMode,
    owner_brain_pinned: bool,
) -> bool {
    requested == jarvis_llm::RoutingMode::Auto && !owner_brain_pinned
}

/// Keep Auto usable when a classifier recommends a tier with no currently eligible,
/// owner-enabled model. This check is read-only; the router still validates the
/// live policy again immediately before a provider call.
pub(crate) fn available_route_mode(
    requested: jarvis_llm::RoutingMode,
    kind: Option<WorkKind>,
    latest_user_turn: &str,
    provider: &dyn jarvis_llm::LlmProvider,
) -> jarvis_llm::RoutingMode {
    let advised = route_mode(requested, kind);
    if requested == jarvis_llm::RoutingMode::Auto
        && advised != jarvis_llm::RoutingMode::Auto
        && !provider.can_serve_tier(jarvis_llm::classify_task(advised, latest_user_turn).tier)
    {
        jarvis_llm::RoutingMode::Auto
    } else {
        advised
    }
}

#[derive(Debug, Clone)]
pub struct IntentDecision {
    pub kind: WorkKind,
    pub confidence: f64,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl IntentDecision {
    pub fn usable_kind(&self) -> Option<WorkKind> {
        self.usable_kind_at(DEFAULT_JEV_CONFIDENCE)
    }

    pub fn usable_kind_at(&self, threshold: f64) -> Option<WorkKind> {
        (self.confidence >= threshold).then_some(self.kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayaMode {
    Off,
    Shadow,
    Primary,
}

impl LayaMode {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "off" => Ok(Self::Off),
            "shadow" => Ok(Self::Shadow),
            "primary" => Ok(Self::Primary),
            _ => Err("invalid Laya mode"),
        }
    }
}

impl IntentRouterChain {
    /// The hosted closure is supplied by Core so it retains exclusive control
    /// of Jev's budget reservation and usage accounting.
    pub async fn choose<F, Fut>(&self, latest_user_turn: &str, hosted: F) -> Option<WorkKind>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<WorkKind>>,
    {
        if latest_user_turn.is_empty() || latest_user_turn.len() > MAX_STATE_BYTES {
            return None;
        }
        match self.mode {
            LayaMode::Off => hosted().await,
            LayaMode::Primary => {
                if let Some(laya) = &self.laya {
                    match laya.classify(latest_user_turn).await {
                        Ok(decision) => {
                            if let Some(kind) = decision.usable_kind_at(self.laya_threshold) {
                                tracing::debug!(
                                    provider = laya.provider_id(),
                                    "local intent accepted"
                                );
                                return Some(kind);
                            }
                            tracing::debug!(
                                provider = laya.provider_id(),
                                "local intent below confidence threshold"
                            );
                        }
                        Err(reason) => tracing::warn!(
                            reason,
                            "local intent unavailable; trying hosted fallback"
                        ),
                    }
                }
                hosted().await
            }
            LayaMode::Shadow => {
                let local = async {
                    match self.laya.as_deref() {
                        Some(laya) => laya
                            .classify(latest_user_turn)
                            .await
                            .ok()
                            .and_then(|decision| decision.usable_kind_at(self.laya_threshold)),
                        None => None,
                    }
                };
                let (actual, shadow) = tokio::join!(hosted(), local);
                tracing::debug!(actual = ?actual, shadow = ?shadow, agrees = actual == shadow,
                    "shadow intent comparison");
                actual
            }
        }
    }
}

/// Core owns the schema and cascade. Each provider can only return a known
/// semantic label, never a model, executable, capability or approval.
#[async_trait::async_trait]
pub trait FastIntentProvider: Send + Sync {
    fn provider_id(&self) -> &'static str;
    /// Cheap local preflight before Core reserves a remote provider budget.
    fn may_send(&self) -> bool {
        true
    }
    async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str>;
}

#[derive(Clone)]
pub struct IntentRouterChain {
    pub laya: Option<Arc<dyn FastIntentProvider>>,
    pub jev: Option<Arc<dyn FastIntentProvider>>,
    pub mode: LayaMode,
    pub laya_threshold: f64,
    pub jev_threshold: f64,
}

#[derive(Deserialize)]
struct Response {
    model: String,
    answers: Answers,
    usage: Usage,
}

#[derive(Deserialize)]
struct Answers {
    work_kind: Choice,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    confidence: f64,
    #[serde(default)]
    answer_confidence: Option<f64>,
    probabilities: std::collections::BTreeMap<String, f64>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u32,
    output_tokens: u32,
}

/// The key is intentionally absent from Debug and never leaves this server.
pub struct JevRouter {
    client: Client,
    endpoint: Url,
    key: String,
    model: String,
    cooldown_until: Mutex<Option<Instant>>,
}

impl JevRouter {
    pub fn new(key: String, model: String) -> Result<Self, &'static str> {
        Self::with_endpoint(key, model, OFFICIAL_ENDPOINT)
    }

    fn with_endpoint(key: String, model: String, endpoint: &str) -> Result<Self, &'static str> {
        if key.is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
            return Err("invalid Jev credential");
        }
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
        {
            return Err("invalid Jev model alias");
        }
        let endpoint = Url::parse(endpoint).map_err(|_| "invalid Jev endpoint")?;
        let client = Client::builder()
            .timeout(Duration::from_secs(3))
            .redirect(Policy::none())
            .build()
            .map_err(|_| "cannot configure Jev transport")?;
        Ok(Self {
            client,
            endpoint,
            key,
            model,
            cooldown_until: Mutex::new(None),
        })
    }

    fn cool_down(&self, duration: Duration) {
        if let Ok(mut until) = self.cooldown_until.lock() {
            *until = Some(Instant::now() + duration);
        }
    }

    /// Sends only the most recent bounded user turn, never persona, transcript,
    /// credentials, tool output or private agent instructions.
    pub async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str> {
        if latest_user_turn.is_empty() || latest_user_turn.len() > MAX_STATE_BYTES {
            return Err("Jev input is unavailable or too large");
        }
        if self
            .cooldown_until
            .lock()
            .is_ok_and(|until| until.is_some_and(|time| time > Instant::now()))
        {
            return Err("Jev is temporarily unavailable");
        }
        let body = system_one_request(Some(&self.model), latest_user_turn);
        let mut response = match self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                self.cool_down(Duration::from_secs(10));
                return Err("Jev request failed");
            }
        };
        if !response.status().is_success() {
            let duration = match response.status().as_u16() {
                401 | 403 => Duration::from_secs(15 * 60),
                429 => Duration::from_secs(60),
                _ => Duration::from_secs(30),
            };
            self.cool_down(duration);
            return Err("Jev request was rejected");
        }
        let bytes = read_bounded_response(&mut response).await?;
        let decision = parse_response(&bytes);
        if decision.is_err() {
            self.cool_down(Duration::from_secs(30));
        }
        decision
    }
}

fn system_one_request(model: Option<&str>, latest_user_turn: &str) -> serde_json::Value {
    let mut body = json!({
        "state": {"user_request": latest_user_turn},
        "questions": {
            "work_kind": {
                "type": "choice",
                "instructions": "Classify the user's primary request. This is advisory routing only; do not authorize or execute actions.",
                "criteria": {
                    "conversation": "General explanation, creative discussion or ordinary chat.",
                    "quick_answer": "Short factual or simple utility question that needs no tools or deep reasoning.",
                    "research": "Needs current sources, evidence gathering or comparison.",
                    "coding": "Software development, debugging, code review or architecture.",
                    "action_request": "Requests a side effect such as saving a note, reminder, system change or transaction."
                }
            }
        }
    });
    if let Some(model) = model {
        body["model"] = json!(model);
    }
    body
}

async fn read_bounded_response(response: &mut reqwest::Response) -> Result<Vec<u8>, &'static str> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
    {
        return Err("System-1 response is too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "System-1 response failed")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("System-1 response is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[async_trait::async_trait]
impl FastIntentProvider for JevRouter {
    fn provider_id(&self) -> &'static str {
        "jev"
    }
    fn may_send(&self) -> bool {
        self.cooldown_until
            .lock()
            .is_ok_and(|until| until.is_none_or(|time| time <= Instant::now()))
    }
    async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str> {
        JevRouter::classify(self, latest_user_turn).await
    }
}

/// Laya is a local CPU service reached only through a root-owned systemd socket.
/// The fixed Unix socket prevents a local process from impersonating a stopped
/// service, and reqwest never resolves the virtual HTTP hostname over TCP.
pub struct LayaRouter {
    client: Client,
    endpoint: Url,
}

impl LayaRouter {
    pub fn new(timeout_ms: u64) -> Result<Self, &'static str> {
        Self::with_socket(LOCAL_ENDPOINT, LOCAL_SOCKET, timeout_ms)
    }

    fn with_socket(endpoint: &str, socket: &str, timeout_ms: u64) -> Result<Self, &'static str> {
        if !(100..=5_000).contains(&timeout_ms) {
            return Err("invalid Laya timeout");
        }
        let endpoint = Url::parse(endpoint).map_err(|_| "invalid Laya endpoint")?;
        if endpoint.scheme() != "http"
            || endpoint.host_str() != Some("jarvis-laya.local")
            || endpoint.path() != "/v1/systemone"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.port().is_some()
        {
            return Err("Laya must use its fixed virtual System One endpoint");
        }
        if !socket.starts_with('/') || socket.is_empty() {
            return Err("Laya socket path is invalid");
        }
        #[cfg(not(unix))]
        return Err("Laya requires a Unix socket");
        #[cfg(unix)]
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(Policy::none())
            .no_proxy()
            .unix_socket(socket)
            .build()
            .map_err(|_| "cannot configure Laya transport")?;
        #[cfg(unix)]
        Ok(Self { client, endpoint })
    }
}

#[async_trait::async_trait]
impl FastIntentProvider for LayaRouter {
    fn provider_id(&self) -> &'static str {
        "laya"
    }
    async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str> {
        if latest_user_turn.is_empty() || latest_user_turn.len() > MAX_STATE_BYTES {
            return Err("Laya input is unavailable or too large");
        }
        // Omitting model invokes Laya's language router. The alias "laya"
        // explicitly selects English upstream and is unsafe for Dutch turns.
        let body = system_one_request(None, latest_user_turn);
        let started = Instant::now();
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .json(&body)
            .send()
            .await
            .map_err(|_| "Laya unavailable")?;
        if !response.status().is_success() {
            return Err("Laya request failed");
        }
        let bytes = read_bounded_response(&mut response).await?;
        let decision = parse_laya_response(&bytes)?;
        tracing::debug!(provider = "laya", checkpoint = %decision.model,
            latency_ms = started.elapsed().as_millis(), "local intent classified");
        Ok(decision)
    }
}

fn parse_response(bytes: &[u8]) -> Result<IntentDecision, &'static str> {
    parse_provider_response(bytes, false)
}

fn parse_laya_response(bytes: &[u8]) -> Result<IntentDecision, &'static str> {
    parse_provider_response(bytes, true)
}

fn parse_provider_response(bytes: &[u8], local: bool) -> Result<IntentDecision, &'static str> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| "System-1 response is malformed")?;
    let answer = response.answers.work_kind;
    let confidence = if local {
        answer
            .answer_confidence
            .ok_or("Laya answer confidence is missing")?
    } else {
        answer.confidence
    };
    if answer.kind != "choice"
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || response.model.is_empty()
        || response.model.len() > 128
        || !response
            .model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
        || response.usage.input_tokens > MAX_BILLABLE_INPUT_TOKENS
        || response.usage.output_tokens > MAX_BILLABLE_OUTPUT_TOKENS
        || answer.probabilities.len() != 5
        || answer.probabilities.iter().any(|(key, value)| {
            WorkKind::parse(key).is_none() || !value.is_finite() || !(0.0..=1.0).contains(value)
        })
        || !(0.95..=1.05).contains(&answer.probabilities.values().sum::<f64>())
    {
        return Err("Jev decision is uncertain or malformed");
    }
    let kind = WorkKind::parse(&answer.choice).ok_or("Jev returned an unknown task kind")?;
    let choice_probability = answer
        .probabilities
        .get(&answer.choice)
        .copied()
        .ok_or("Jev decision is uncertain")?;
    if answer
        .probabilities
        .values()
        .any(|alternative| *alternative > choice_probability)
    {
        return Err("Jev decision is uncertain");
    }
    Ok(IntentDecision {
        kind,
        confidence: confidence.min(choice_probability),
        model: response.model,
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    })
}

/// The chain only advises Auto chat. Shadow observations never reach the
/// routing result, and local inference never enters the monetary budget book.
pub(crate) async fn decide(state: &AppState, latest_user_turn: &str) -> Option<WorkKind> {
    let chain = state.fast_intent_router.as_ref()?;
    chain
        .choose(latest_user_turn, || async {
            match chain.jev.as_deref() {
                Some(jev) => decide_jev(state, jev, latest_user_turn, chain.jev_threshold).await,
                None => None,
            }
        })
        .await
}

/// Preserve Jev's separately metered budget path. Only a Jev invocation can
/// reserve external spend; Laya never comes through this function.
async fn decide_jev(
    state: &AppState,
    jev: &dyn FastIntentProvider,
    latest_user_turn: &str,
    threshold: f64,
) -> Option<WorkKind> {
    // A known local cooldown makes no outbound request and must not create a
    // monetary reservation or a synthetic usage record.
    if !jev.may_send() {
        return None;
    }
    let projected_eur = jarvis_usage::cost_eur_with_registry(
        &state.pricing_registry,
        "jev",
        "jev-unconfirmed",
        MAX_BILLABLE_INPUT_TOKENS,
        MAX_BILLABLE_OUTPUT_TOKENS,
        0,
        state.eur_per_usd,
    );
    if !projected_eur.is_finite() || projected_eur <= 0.0 {
        return None;
    }
    let projected_cents = (projected_eur * 100.0).ceil().max(1.0) as u64;
    let reservation = format!("jev:{}", uuid::Uuid::now_v7());
    state
        .budget_book
        .reserve(&reservation, projected_cents)
        .ok()?;

    let result = jev.classify(latest_user_turn).await;
    let (model, input_tokens, output_tokens, status, kind) = match result {
        Ok(decision) => {
            let kind = decision.usable_kind_at(threshold);
            (
                decision.model,
                decision.input_tokens,
                decision.output_tokens,
                if kind.is_some() {
                    "succeeded"
                } else {
                    "low_confidence"
                },
                kind,
            )
        }
        Err("Jev is temporarily unavailable" | "Jev input is unavailable or too large") => {
            // Cooldown can begin between preflight and classify under
            // concurrency. No HTTP request was sent, so release the reserved
            // budget without creating a synthetic paid usage record.
            state.budget_book.cancel(&reservation);
            return None;
        }
        Err(reason) => {
            tracing::warn!(reason, "Jev unavailable; deterministic routing retained");
            (
                "jev-unconfirmed".to_owned(),
                MAX_BILLABLE_INPUT_TOKENS,
                MAX_BILLABLE_OUTPUT_TOKENS,
                "failed_estimated_usage",
                None,
            )
        }
    };
    let reply = jarvis_llm::ChatReply {
        text: String::new(),
        model,
        backend: Some("jev".into()),
        requested_route: None,
        actual_provider: None,
        stop_reason: None,
        usage: Some(jarvis_llm::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
    };
    // If usage persistence fails, the reservation is still settled locally;
    // a later successful DB reconciliation can replace the estimate.
    if state
        .budget_book
        .settle(&reservation, projected_cents)
        .is_err()
    {
        return None;
    }
    record_usage_with_metadata(
        state,
        &reply,
        jarvis_usage::UsageMetadata {
            request_id: reservation,
            routing_mode: "intent_classification".into(),
            quality_tier: "advisory".into(),
            status: status.into(),
            ..Default::default()
        },
    )
    .await;
    kind
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct FixtureProvider {
        calls: Arc<AtomicUsize>,
        decision: Option<IntentDecision>,
    }

    #[async_trait::async_trait]
    impl FastIntentProvider for FixtureProvider {
        fn provider_id(&self) -> &'static str {
            "laya"
        }
        async fn classify(&self, _: &str) -> Result<IntentDecision, &'static str> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.decision.clone().ok_or("local unavailable")
        }
    }

    fn fixture_decision(confidence: f64, kind: WorkKind) -> IntentDecision {
        IntentDecision {
            kind,
            confidence,
            model: "multilingual".into(),
            input_tokens: 20,
            output_tokens: 0,
        }
    }

    #[tokio::test]
    async fn local_primary_shadow_and_fallback_never_duplicate_hosted_decision() {
        for (mode, local_confidence, expected, hosted_calls) in [
            (LayaMode::Primary, Some(0.98), Some(WorkKind::Coding), 0),
            (LayaMode::Primary, Some(0.60), Some(WorkKind::Research), 1),
            (LayaMode::Primary, None, Some(WorkKind::Research), 1),
            (LayaMode::Shadow, Some(0.98), Some(WorkKind::Research), 1),
            (LayaMode::Off, Some(0.98), Some(WorkKind::Research), 1),
        ] {
            let local_calls = Arc::new(AtomicUsize::new(0));
            let hosted_count = Arc::new(AtomicUsize::new(0));
            let chain = IntentRouterChain {
                laya: Some(Arc::new(FixtureProvider {
                    calls: local_calls.clone(),
                    decision: local_confidence
                        .map(|confidence| fixture_decision(confidence, WorkKind::Coding)),
                })),
                jev: None,
                mode,
                laya_threshold: 0.95,
                jev_threshold: 0.75,
            };
            let answer = chain
                .choose("review code", || async {
                    hosted_count.fetch_add(1, Ordering::SeqCst);
                    Some(WorkKind::Research)
                })
                .await;
            assert_eq!(answer, expected);
            assert_eq!(hosted_count.load(Ordering::SeqCst), hosted_calls);
            assert_eq!(
                local_calls.load(Ordering::SeqCst),
                usize::from(mode != LayaMode::Off)
            );
        }
    }

    #[test]
    fn local_requires_answer_probability_not_jev_entropy_confidence() {
        let mut payload = json!({
            "model":"multilingual",
            "answers":{"work_kind":{"type":"choice","choice":"coding",
                "confidence":0.99,"answer_confidence":0.61,
                "probabilities":{"conversation":0.10,"quick_answer":0.10,
                    "research":0.09,"coding":0.61,"action_request":0.10}}},
            "usage":{"input_tokens":20,"output_tokens":0}
        });
        let decision = parse_laya_response(&serde_json::to_vec(&payload).unwrap()).unwrap();
        assert_eq!(decision.confidence, 0.61);
        assert_eq!(decision.usable_kind_at(0.95), None);
        payload["answers"]["work_kind"]
            .as_object_mut()
            .unwrap()
            .remove("answer_confidence");
        assert!(parse_laya_response(&serde_json::to_vec(&payload).unwrap()).is_err());
        payload["answers"]["work_kind"]["answer_confidence"] = json!(0.99);
        payload["answers"]["work_kind"]["choice"] = json!("execute_shell");
        assert!(parse_laya_response(&serde_json::to_vec(&payload).unwrap()).is_err());
        assert!(parse_laya_response(&vec![b'x'; MAX_RESPONSE_BYTES + 1]).is_err());
    }

    #[test]
    fn local_endpoint_rejects_tcp_and_unsafe_urls() {
        for endpoint in [
            "http://192.0.2.10:8091/v1/systemone",
            "https://127.0.0.1:8091/v1/systemone",
            "http://127.0.0.1:8091/v1/systemone?token=x",
            "http://user@127.0.0.1:8091/v1/systemone",
        ] {
            assert!(LayaRouter::with_socket(endpoint, LOCAL_SOCKET, 1000).is_err());
        }
        assert!(LayaRouter::new(1000).is_ok());
    }

    #[tokio::test]
    async fn local_unix_socket_contract_has_no_credential_and_bounds_responses() {
        let directory = tempfile::tempdir().unwrap();
        let socket_path = directory.path().join("laya.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
                assert!(header.len() < 8192);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with("POST /v1/systemone HTTP/1.1"));
            assert!(!header.to_ascii_lowercase().contains("authorization:"));
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            let mut request = vec![0; length];
            socket.read_exact(&mut request).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert!(
                body.get("model").is_none(),
                "local model must auto-route by language"
            );
            assert_eq!(body["state"]["user_request"], "Hoi, bekijk mijn code");
            let reply = json!({"model":"multilingual",
                "answers":{"work_kind":{"type":"choice","choice":"coding",
                    "confidence":0.98,"answer_confidence":0.98,
                    "probabilities":{"conversation":0.005,"quick_answer":0.005,
                        "research":0.005,"coding":0.98,"action_request":0.005}}},
                "usage":{"input_tokens":20,"output_tokens":0}})
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let local =
            LayaRouter::with_socket(LOCAL_ENDPOINT, socket_path.to_str().unwrap(), 1000).unwrap();
        assert_eq!(
            local.classify("Hoi, bekijk mijn code").await.unwrap().kind,
            WorkKind::Coding
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stopped_local_service_immediately_falls_back_without_jev_key() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("missing.sock");
        let local = Arc::new(
            LayaRouter::with_socket(LOCAL_ENDPOINT, socket.to_str().unwrap(), 100).unwrap(),
        );
        let chain = IntentRouterChain {
            laya: Some(local),
            jev: None,
            mode: LayaMode::Primary,
            laya_threshold: 0.95,
            jev_threshold: 0.75,
        };
        assert_eq!(chain.choose("hello", || async { None }).await, None);
    }

    #[tokio::test]
    async fn local_socket_timeout_and_oversized_response_degrade_safely() {
        let directory = tempfile::tempdir().unwrap();
        let slow_path = directory.path().join("slow.sock");
        let slow = tokio::net::UnixListener::bind(&slow_path).unwrap();
        let slow_server = tokio::spawn(async move {
            let (_connection, _) = slow.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        });
        let router =
            LayaRouter::with_socket(LOCAL_ENDPOINT, slow_path.to_str().unwrap(), 100).unwrap();
        assert!(router.classify("Hoi").await.is_err());
        slow_server.await.unwrap();

        let large_path = directory.path().join("large.sock");
        let large = tokio::net::UnixListener::bind(&large_path).unwrap();
        let large_server = tokio::spawn(async move {
            let (mut connection, _) = large.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = connection.read(&mut request).await.unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_RESPONSE_BYTES + 1
            );
            connection.write_all(header.as_bytes()).await.unwrap();
        });
        let router =
            LayaRouter::with_socket(LOCAL_ENDPOINT, large_path.to_str().unwrap(), 1000).unwrap();
        assert!(router.classify("Hoi").await.is_err());
        large_server.await.unwrap();
    }

    #[test]
    fn parses_only_high_confidence_bounded_known_choices() {
        let valid = json!({
            "model":"jev-latest",
            "answers":{"work_kind":{"type":"choice","choice":"coding","confidence":0.91,
                "probabilities":{"conversation":0.01,"quick_answer":0.01,"research":0.03,"coding":0.91,"action_request":0.04}}},
            "usage":{"input_tokens":120,"output_tokens":12}
        });
        assert_eq!(
            parse_response(&serde_json::to_vec(&valid).unwrap())
                .unwrap()
                .kind,
            WorkKind::Coding
        );
        let mut low = valid.clone();
        low["answers"]["work_kind"]["confidence"] = json!(0.4);
        assert!(parse_response(&serde_json::to_vec(&low).unwrap())
            .unwrap()
            .usable_kind()
            .is_none());
        let mut split = valid.clone();
        split["answers"]["work_kind"]["probabilities"] = json!({
            "conversation":0.15,"quick_answer":0.15,"research":0.15,
            "coding":0.40,"action_request":0.15
        });
        assert!(parse_response(&serde_json::to_vec(&split).unwrap())
            .unwrap()
            .usable_kind()
            .is_none());
        let mut injected = valid;
        injected["answers"]["work_kind"]["choice"] = json!("sudo systemctl restart core");
        assert!(parse_response(&serde_json::to_vec(&injected).unwrap()).is_err());
        injected["answers"]["work_kind"]["choice"] = json!("coding");
        injected["answers"]["work_kind"]["probabilities"]["coding"] = json!(0.01);
        injected["answers"]["work_kind"]["probabilities"]["conversation"] = json!(0.91);
        assert!(parse_response(&serde_json::to_vec(&injected).unwrap()).is_err());
    }

    #[test]
    fn jev_only_routes_to_existing_quality_modes_and_never_overrides_owner_mode() {
        use jarvis_llm::{classify_task, RoutingMode, Tier};
        assert_eq!(
            route_mode(RoutingMode::Auto, Some(WorkKind::QuickAnswer)),
            RoutingMode::Fast
        );
        assert_eq!(
            route_mode(RoutingMode::Auto, Some(WorkKind::Research)),
            RoutingMode::Research
        );
        assert_eq!(
            route_mode(RoutingMode::Auto, Some(WorkKind::Coding)),
            RoutingMode::Deep
        );
        assert_eq!(
            route_mode(RoutingMode::Auto, Some(WorkKind::ActionRequest)),
            RoutingMode::Deep
        );
        assert_eq!(
            route_mode(RoutingMode::Deep, Some(WorkKind::QuickAnswer)),
            RoutingMode::Deep
        );
        assert_eq!(
            classify_task(
                route_mode(RoutingMode::Auto, Some(WorkKind::QuickAnswer)),
                "security review"
            )
            .tier,
            Tier::Default,
            "the deterministic safety floor prevents an unsafe Jev downgrade"
        );
    }

    #[test]
    fn unavailable_advisory_tier_falls_back_to_existing_auto_route() {
        struct MidOnly;
        #[async_trait::async_trait]
        impl jarvis_llm::LlmProvider for MidOnly {
            fn label(&self) -> &str {
                "mid-only-fixture"
            }
            fn can_serve_tier(&self, tier: jarvis_llm::Tier) -> bool {
                tier != jarvis_llm::Tier::Cheap
            }
            async fn chat(
                &self,
                _: &jarvis_llm::ChatRequest,
            ) -> Result<jarvis_llm::ChatReply, jarvis_llm::LlmError> {
                unreachable!("routing availability must never start inference")
            }
        }
        use jarvis_llm::RoutingMode;
        assert_eq!(
            available_route_mode(
                RoutingMode::Auto,
                Some(WorkKind::QuickAnswer),
                "hoi",
                &MidOnly,
            ),
            RoutingMode::Auto,
        );
        assert_eq!(
            available_route_mode(RoutingMode::Auto, Some(WorkKind::Coding), "code", &MidOnly),
            RoutingMode::Deep,
        );
        assert_eq!(
            available_route_mode(
                RoutingMode::Deep,
                Some(WorkKind::QuickAnswer),
                "hoi",
                &MidOnly,
            ),
            RoutingMode::Deep,
        );
    }

    #[test]
    fn pinned_owner_brain_skips_jev_even_when_request_mode_is_auto() {
        use jarvis_llm::RoutingMode;
        assert!(!should_classify(RoutingMode::Auto, true));
        assert!(should_classify(RoutingMode::Auto, false));
        assert!(!should_classify(RoutingMode::Deep, false));
    }

    #[tokio::test]
    async fn official_contract_uses_bearer_header_and_only_latest_bounded_turn() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                header.push(byte[0]);
                assert!(header.len() < 8_192);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = String::from_utf8(header).unwrap();
            assert!(header.starts_with("POST /v1/systemone HTTP/1.1"));
            assert!(header
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-key"));
            assert!(!header.lines().next().unwrap().contains("fixture-key"));
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(length < 2_048);
            let mut request = vec![0; length];
            socket.read_exact(&mut request).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(body["model"], "jev-latest");
            assert_eq!(body["state"]["user_request"], "Review this code");
            assert!(body["questions"]["work_kind"]["criteria"].is_object());
            assert_eq!(body["questions"].as_object().unwrap().len(), 1);
            let reply = json!({
                "model":"jev-latest",
                "answers":{"work_kind":{"type":"choice","choice":"coding","confidence":0.91,
                    "probabilities":{"conversation":0.01,"quick_answer":0.01,"research":0.03,"coding":0.91,"action_request":0.04}}},
                "usage":{"input_tokens":120,"output_tokens":12}
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(), reply
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let router =
            JevRouter::with_endpoint("fixture-key".into(), "jev-latest".into(), &endpoint).unwrap();
        let decision = router.classify("Review this code").await.unwrap();
        assert_eq!(decision.usable_kind(), Some(WorkKind::Coding));
        assert_eq!(decision.input_tokens, 120);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rate_limit_cools_down_without_retrying_or_leaking_credential() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            let request_line = String::from_utf8_lossy(&request[..read]);
            assert!(!request_line
                .lines()
                .next()
                .unwrap()
                .contains("fixture-secret"));
            socket
                .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let router =
            JevRouter::with_endpoint("fixture-secret".into(), "jev-latest".into(), &endpoint)
                .unwrap();
        assert!(router.classify("hello").await.is_err());
        server.await.unwrap();
        assert_eq!(
            router.classify("hello again").await.unwrap_err(),
            "Jev is temporarily unavailable"
        );
        assert!(!FastIntentProvider::may_send(&router));
    }

    #[tokio::test]
    async fn unsafe_model_alias_and_oversized_input_are_rejected_without_network() {
        assert!(JevRouter::new("fixture".into(), "bad alias".into()).is_err());
        assert!(JevRouter::new("fixture".into(), "jev-2026.09/fast".into()).is_ok());
        assert!(JevRouter::new("fixture\nheader: x".into(), "jev-latest".into()).is_err());
        let router = JevRouter::new("fixture".into(), "jev-latest".into()).unwrap();
        assert!(router
            .classify(&"x".repeat(MAX_STATE_BYTES + 1))
            .await
            .is_err());
    }
}
