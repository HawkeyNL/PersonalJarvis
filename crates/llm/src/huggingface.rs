//! Hugging Face Inference Providers adapter.
//!
//! The owner authorizes a base model. Hugging Face's execution route is a
//! separate, validated suffix used only on the outbound request.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;

use crate::{
    model_policy::validate_hf_route, ChatReply, ChatRequest, LlmError, LlmProvider,
    OpenAiCompatProvider, Tier,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HuggingFaceRoute(String);

impl HuggingFaceRoute {
    pub fn parse(value: impl Into<String>) -> Result<Self, LlmError> {
        let value = value.into();
        validate_hf_route(&value).map_err(LlmError::NotConfigured)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn hf_routed_model(base_model: &str, route: &str) -> Result<String, LlmError> {
    if base_model.is_empty()
        || base_model.len() > 256
        || base_model.chars().any(char::is_control)
        || base_model
            .rsplit('/')
            .next()
            .is_some_and(|part| part.contains(':'))
    {
        return Err(LlmError::NotConfigured(
            "invalid Hugging Face base model".into(),
        ));
    }
    validate_hf_route(route).map_err(LlmError::NotConfigured)?;
    if route == "auto" {
        Ok(base_model.to_owned())
    } else {
        Ok(format!("{base_model}:{route}"))
    }
}

#[async_trait]
trait HuggingFaceChatTransport: Send + Sync {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatReply, LlmError>;
}

#[async_trait]
impl HuggingFaceChatTransport for OpenAiCompatProvider {
    async fn chat(&self, request: &ChatRequest) -> Result<ChatReply, LlmError> {
        LlmProvider::chat(self, request).await
    }
}

pub struct HuggingFaceProvider {
    inner: Arc<dyn HuggingFaceChatTransport>,
    model_default: String,
    model_hard: String,
    model_cheap: String,
    route_default: HuggingFaceRoute,
    route_hard: HuggingFaceRoute,
    route_cheap: HuggingFaceRoute,
    owner_routes: BTreeMap<String, HuggingFaceRoute>,
    label: String,
}

impl HuggingFaceProvider {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model_default: impl Into<String>,
        model_hard: impl Into<String>,
        model_cheap: impl Into<String>,
        route_default: impl Into<String>,
        route_hard: impl Into<String>,
        route_cheap: impl Into<String>,
        owner_routes: BTreeMap<String, String>,
    ) -> Result<Self, LlmError> {
        let model_default = model_default.into();
        let model_hard = model_hard.into();
        let model_cheap = model_cheap.into();
        let inner = OpenAiCompatProvider::new(
            "huggingface",
            "huggingface",
            api_key,
            base_url,
            &model_default,
            &model_hard,
            &model_cheap,
        )?;
        let owner_routes = owner_routes
            .into_iter()
            .map(|(model, route)| Ok((model, HuggingFaceRoute::parse(route)?)))
            .collect::<Result<_, LlmError>>()?;
        let route_default = HuggingFaceRoute::parse(route_default)?;
        let route_hard = HuggingFaceRoute::parse(route_hard)?;
        let route_cheap = HuggingFaceRoute::parse(route_cheap)?;
        Ok(Self::with_transport(
            Arc::new(inner),
            model_default,
            model_hard,
            model_cheap,
            route_default,
            route_hard,
            route_cheap,
            owner_routes,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn with_transport(
        inner: Arc<dyn HuggingFaceChatTransport>,
        model_default: String,
        model_hard: String,
        model_cheap: String,
        route_default: HuggingFaceRoute,
        route_hard: HuggingFaceRoute,
        route_cheap: HuggingFaceRoute,
        owner_routes: BTreeMap<String, HuggingFaceRoute>,
    ) -> Self {
        let label = format!("huggingface:{model_default}:{}", route_default.as_str());
        Self {
            inner,
            label,
            model_default,
            model_hard,
            model_cheap,
            route_default,
            route_hard,
            route_cheap,
            owner_routes,
        }
    }

    fn base_model(&self, request: &ChatRequest) -> String {
        request.model.clone().unwrap_or_else(|| {
            match request.tier {
                Tier::Default => &self.model_default,
                Tier::Hard => &self.model_hard,
                Tier::Cheap => &self.model_cheap,
            }
            .clone()
        })
    }

    fn route_for<'a>(&'a self, model: &str, tier: Tier) -> &'a HuggingFaceRoute {
        self.owner_routes.get(model).unwrap_or(match tier {
            Tier::Default => &self.route_default,
            Tier::Hard => &self.route_hard,
            Tier::Cheap => &self.route_cheap,
        })
    }
}

#[async_trait]
impl LlmProvider for HuggingFaceProvider {
    fn label(&self) -> &str {
        &self.label
    }

    async fn chat(&self, request: &ChatRequest) -> Result<ChatReply, LlmError> {
        let base_model = self.base_model(request);
        let route = self.route_for(&base_model, request.tier);
        let routed_model = hf_routed_model(&base_model, route.as_str())?;
        tracing::debug!(
            base_model = %base_model,
            requested_hf_route = %route.as_str(),
            actual_hf_provider = "unknown",
            "sending Hugging Face chat request"
        );
        let mut routed_request = request.clone();
        routed_request.model = Some(routed_model);
        let mut reply = self.inner.chat(&routed_request).await?;
        // Keep accounting and policy identity stable; the suffix is execution
        // infrastructure, not a different owner-authorized model.
        reply.model = base_model;
        reply.backend = Some("huggingface".into());
        reply.requested_route = Some(route.as_str().to_owned());
        // The OpenAI-compatible response does not currently provide a stable,
        // documented execution-provider field. Never infer one.
        reply.actual_provider = None;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        always_available, build_router_with_policy, CatalogModel, HuggingFaceBackend,
        HuggingFaceCatalog, ModelAccessEntry, ModelAccessPolicy, ModelClass, OpenAiBackend,
        ProviderConfig, RoutingMode,
    };
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    #[test]
    fn constructs_reserved_and_explicit_routes() {
        assert_eq!(
            hf_routed_model("openai/gpt-oss-20b", "auto").unwrap(),
            "openai/gpt-oss-20b"
        );
        assert_eq!(
            hf_routed_model("openai/gpt-oss-20b", "fastest").unwrap(),
            "openai/gpt-oss-20b:fastest"
        );
        assert_eq!(
            hf_routed_model("openai/gpt-oss-20b", "cheapest").unwrap(),
            "openai/gpt-oss-20b:cheapest"
        );
        assert_eq!(
            hf_routed_model("openai/gpt-oss-20b", "preferred").unwrap(),
            "openai/gpt-oss-20b:preferred"
        );
        assert_eq!(
            hf_routed_model("openai/gpt-oss-20b", "groq").unwrap(),
            "openai/gpt-oss-20b:groq"
        );
    }

    #[test]
    fn rejects_unsafe_route_or_routed_base() {
        assert!(hf_routed_model("openai/gpt", "https://evil").is_err());
        assert!(hf_routed_model("openai/gpt:groq", "auto").is_err());
    }

    #[derive(Default)]
    struct CapturingTransport {
        request: std::sync::Mutex<Option<ChatRequest>>,
    }

    #[async_trait]
    impl HuggingFaceChatTransport for CapturingTransport {
        async fn chat(&self, request: &ChatRequest) -> Result<ChatReply, LlmError> {
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(ChatReply {
                text: "ok".into(),
                model: request.model.clone().unwrap(),
                backend: Some("huggingface".into()),
                requested_route: None,
                actual_provider: None,
                stop_reason: Some("stop".into()),
                usage: Some(crate::Usage {
                    input_tokens: 3,
                    output_tokens: 1,
                    ..Default::default()
                }),
            })
        }
    }

    #[tokio::test]
    async fn in_memory_discovery_allowlist_route_router_and_chat_flow() {
        use crate::router::{Candidate, RouterProvider};

        let discovered = HuggingFaceCatalog::from_api_response(
            &json!({"data":[{"id":"openai/gpt-oss-20b","providers":[
                {"provider":"groq","status":"live","pricing":{"input":0.1,"output":0.2}},
                {"provider":"novita","status":"error"}
            ]}]}),
            "fixture",
        );
        let model = discovered.models[0].id.clone();
        assert!(discovered.route_available(&model, "groq"));
        let policy = ModelAccessPolicy {
            version: 1,
            models: vec![ModelAccessEntry {
                provider: "huggingface".into(),
                model: model.clone(),
                enabled: true,
                source: "mocked-discovery".into(),
                route: Some("groq".into()),
            }],
        };
        let transport = Arc::new(CapturingTransport::default());
        let provider = HuggingFaceProvider::with_transport(
            transport.clone(),
            model.clone(),
            model.clone(),
            model.clone(),
            HuggingFaceRoute::parse("fastest").unwrap(),
            HuggingFaceRoute::parse("preferred").unwrap(),
            HuggingFaceRoute::parse("cheapest").unwrap(),
            BTreeMap::from([(model.clone(), HuggingFaceRoute::parse("groq").unwrap())]),
        );
        let router = RouterProvider::with_policy(
            vec![Candidate {
                id: "huggingface".into(),
                provider: Arc::new(provider),
            }],
            always_available(),
            vec![CatalogModel {
                backend: "huggingface".into(),
                id: model.clone(),
                class: ModelClass::Light,
            }],
            policy,
        );
        let reply = router
            .chat(&ChatRequest {
                system: None,
                messages: vec![crate::ChatMessage::user("hello")],
                tier: Tier::Default,
                mode: RoutingMode::Auto,
                max_tokens: 16,
                model: Some(model.clone()),
            })
            .await
            .unwrap();
        assert_eq!(reply.model, model);
        assert_eq!(reply.requested_route.as_deref(), Some("groq"));
        let sent = transport.request.lock().unwrap().clone().unwrap();
        assert_eq!(sent.model.as_deref(), Some("openai/gpt-oss-20b:groq"));
    }

    async fn mock_chat() -> Option<(String, oneshot::Receiver<String>)> {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return None,
            Err(error) => panic!("bind mock HTTP listener: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end;
            loop {
                let mut chunk = [0_u8; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    panic!("mock request ended before headers");
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    header_end = position + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap();
            while request.len() < header_end + content_length {
                let mut chunk = [0_u8; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                request.extend_from_slice(&chunk[..read]);
            }
            let captured = String::from_utf8(request).unwrap();
            sender.send(captured).unwrap();
            let body = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        Some((format!("http://{address}/v1"), receiver))
    }

    #[tokio::test]
    async fn mocked_catalog_allowlist_route_and_chat_flow_preserves_base_identity() {
        let Some((base_url, captured)) = mock_chat().await else {
            return;
        };
        let discovered = HuggingFaceCatalog::from_api_response(
            &json!({"data":[{"id":"openai/gpt-oss-20b","providers":[
                {"provider":"groq","status":"live","pricing":{"input":0.1,"output":0.2}},
                {"provider":"novita","status":"error"}
            ]}]}),
            "fixture",
        );
        let model = discovered.models[0].id.as_str();
        assert!(discovered.route_available(model, "groq"));
        assert!(!discovered.route_available(model, "novita"));
        let policy = ModelAccessPolicy {
            version: 1,
            models: vec![ModelAccessEntry {
                provider: "huggingface".into(),
                model: model.into(),
                enabled: true,
                source: "fixture".into(),
                route: Some("groq".into()),
            }],
        };
        let provider = build_router_with_policy(
            ProviderConfig {
                provider: "router".into(),
                api_key: None,
                anthropic_base_url: String::new(),
                model_default: String::new(),
                model_hard: String::new(),
                model_cheap: String::new(),
                ollama_url: "http://127.0.0.1:1".into(),
                ollama_model: String::new(),
                claude_cli_bin: "/does/not/exist".into(),
                openai: OpenAiBackend::default(),
                deepseek: OpenAiBackend::default(),
                xai: OpenAiBackend::default(),
                zai: OpenAiBackend::default(),
                ollama_cloud: OpenAiBackend::default(),
                huggingface: HuggingFaceBackend {
                    api_key: Some("hf-fixture-secret".into()),
                    base_url,
                    model_default: model.into(),
                    model_hard: model.into(),
                    model_cheap: model.into(),
                    route_default: "fastest".into(),
                    route_hard: "preferred".into(),
                    route_cheap: "cheapest".into(),
                },
            },
            always_available(),
            vec![CatalogModel {
                backend: "huggingface".into(),
                id: model.into(),
                class: ModelClass::Light,
            }],
            policy,
        );
        let reply = provider
            .chat(&ChatRequest {
                system: None,
                messages: vec![crate::ChatMessage::user("hello")],
                tier: Tier::Default,
                mode: RoutingMode::Auto,
                max_tokens: 16,
                model: Some(model.into()),
            })
            .await
            .unwrap();
        assert_eq!(reply.model, model);
        assert_eq!(reply.backend.as_deref(), Some("huggingface"));
        assert_eq!(reply.requested_route.as_deref(), Some("groq"));
        assert_eq!(reply.actual_provider, None);
        let request = captured.await.unwrap();
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(request.contains("authorization: Bearer hf-fixture-secret"));
        assert!(request.contains(r#""model":"openai/gpt-oss-20b:groq""#));
        assert!(!format!("{reply:?}").contains("hf-fixture-secret"));
    }

    #[tokio::test]
    async fn disabled_model_never_reaches_hugging_face() {
        let model = "openai/gpt-oss-20b";
        let transport = Arc::new(CapturingTransport::default());
        let huggingface = HuggingFaceProvider::with_transport(
            transport.clone(),
            model.into(),
            model.into(),
            model.into(),
            HuggingFaceRoute::parse("fastest").unwrap(),
            HuggingFaceRoute::parse("preferred").unwrap(),
            HuggingFaceRoute::parse("cheapest").unwrap(),
            BTreeMap::from([(model.into(), HuggingFaceRoute::parse("groq").unwrap())]),
        );
        let provider = crate::router::RouterProvider::with_policy(
            vec![crate::router::Candidate {
                id: "huggingface".into(),
                provider: Arc::new(huggingface),
            }],
            always_available(),
            vec![CatalogModel {
                backend: "huggingface".into(),
                id: model.into(),
                class: ModelClass::Light,
            }],
            ModelAccessPolicy {
                version: 1,
                models: vec![ModelAccessEntry {
                    provider: "huggingface".into(),
                    model: model.into(),
                    enabled: false,
                    source: "fixture".into(),
                    route: Some("groq".into()),
                }],
            },
        );
        let result = provider
            .chat(&ChatRequest {
                system: None,
                messages: vec![crate::ChatMessage::user("hello")],
                tier: Tier::Default,
                mode: RoutingMode::Auto,
                max_tokens: 16,
                model: Some(model.into()),
            })
            .await;
        assert!(matches!(result, Err(LlmError::NotConfigured(_))));
        assert!(transport.request.lock().unwrap().is_none());
    }
}
