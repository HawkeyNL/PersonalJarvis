//! Advisory, bounded task classification. Jev never authorizes an action or
//! names an executable, provider, model, device, agent, or filesystem path.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::{redirect::Policy, Client, Url};
use serde::Deserialize;
use serde_json::json;

use crate::{metering::record_usage_with_metadata, AppState};

const OFFICIAL_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MAX_STATE_BYTES: usize = 4_096;
const MAX_RESPONSE_BYTES: usize = 16_384;
const MIN_CONFIDENCE: f64 = 0.75;
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

/// Jev may recommend a semantic route, but cannot supply a concrete model or
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

/// Keep Auto usable when Jev recommends a tier with no currently eligible,
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

/// Mockable Core-owned decision boundary. Implementations return only a fixed
/// task kind and accounting metadata; neither models nor actions are accepted.
#[async_trait::async_trait]
pub trait FastIntentRouter: Send + Sync {
    async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str>;
}

impl IntentDecision {
    pub fn usable_kind(&self) -> Option<WorkKind> {
        (self.confidence >= MIN_CONFIDENCE).then_some(self.kind)
    }
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
        let body = json!({
            "model": self.model,
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
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err("Jev response is too large");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "Jev response failed")? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err("Jev response is too large");
            }
            bytes.extend_from_slice(&chunk);
        }
        let decision = parse_response(&bytes);
        if decision.is_err() {
            self.cool_down(Duration::from_secs(30));
        }
        decision
    }
}

#[async_trait::async_trait]
impl FastIntentRouter for JevRouter {
    async fn classify(&self, latest_user_turn: &str) -> Result<IntentDecision, &'static str> {
        JevRouter::classify(self, latest_user_turn).await
    }
}

fn parse_response(bytes: &[u8]) -> Result<IntentDecision, &'static str> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| "Jev response is malformed")?;
    let answer = response.answers.work_kind;
    if answer.kind != "choice"
        || !answer.confidence.is_finite()
        || !(0.0..=1.0).contains(&answer.confidence)
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
        confidence: answer.confidence.min(choice_probability),
        model: response.model,
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    })
}

/// One bounded, separately metered Jev decision. A missing key, unsafe budget
/// estimate, insufficient budget or malformed remote response leaves ordinary
/// deterministic routing untouched.
pub(crate) async fn decide(state: &AppState, latest_user_turn: &str) -> Option<WorkKind> {
    let jev = state.jev.as_ref()?;
    if latest_user_turn.is_empty() || latest_user_turn.len() > MAX_STATE_BYTES {
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
            let kind = decision.usable_kind();
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
