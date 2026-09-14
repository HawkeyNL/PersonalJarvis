use super::*;
use futures_util::StreamExt;
use jarvis_client_core::realtime::{Event, EventEnvelope};
use jarvis_client_core::speech::{SpeechAction, VoiceGate};
use jarvis_llm::{ChatReply, ChatRequest, LlmError, LlmProvider};
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};
use uuid::Uuid;

struct Fake(Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<Vec<String>>>>);

struct PausedFake {
    count: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    finish: Arc<tokio::sync::Notify>,
    fail: bool,
}
#[async_trait::async_trait]
impl LlmProvider for PausedFake {
    fn label(&self) -> &str {
        "paused-fixture"
    }
    async fn chat(&self, _: &ChatRequest) -> Result<ChatReply, LlmError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.finish.notified().await;
        if self.fail {
            return Err(LlmError::Empty);
        }
        Ok(ChatReply {
            text: "Persisted while display was offline.".into(),
            model: "fixture".into(),
            backend: Some("ollama".into()),
            requested_route: None,
            actual_provider: None,
            stop_reason: Some("stop".into()),
            usage: None,
        })
    }
}
#[async_trait::async_trait]
impl LlmProvider for Fake {
    fn label(&self) -> &str {
        "fixture"
    }
    async fn chat(&self, req: &ChatRequest) -> Result<ChatReply, LlmError> {
        self.1
            .lock()
            .unwrap()
            .push(req.messages.iter().map(|m| m.content.clone()).collect());
        self.0.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(ChatReply {
            text: "One canonical answer.\n\nNo second inference.".into(),
            model: "fixture-model".into(),
            backend: Some("ollama".into()),
            requested_route: None,
            actual_provider: None,
            stop_reason: Some("stop".into()),
            usage: Some(jarvis_llm::Usage {
                input_tokens: 3,
                output_tokens: 9,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
        })
    }
    async fn chat_stream(
        &self,
        req: &ChatRequest,
        sink: jarvis_llm::TextDeltaSink,
    ) -> Result<ChatReply, LlmError> {
        sink("One canonical answer.");
        tokio::time::sleep(Duration::from_millis(10)).await;
        sink("\n\nNo second inference.");
        self.chat(req).await
    }
}

async fn device_token(db: &jarvis_store::Database, owner: Uuid) -> (Uuid, String) {
    let key = SigningKey::from_bytes(&rand::random());
    let (device, _) = jarvis_identity::register_device(
        db,
        owner,
        "fixture",
        jarvis_identity::Platform::Ios,
        "ed25519",
        key.verifying_key().as_bytes(),
    )
    .await
    .unwrap();
    let challenge = jarvis_identity::create_challenge(db, device.id)
        .await
        .unwrap();
    let login = jarvis_identity::login(
        db,
        device.id,
        challenge.id,
        &key.sign(&challenge.nonce).to_bytes(),
    )
    .await
    .unwrap();
    (device.id, login.token)
}

async fn post(app: &axum::Router, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn event(socket: &mut Socket) -> EventEnvelope {
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* server"]
async fn one_prompt_two_authenticated_sockets_one_canonical_answer(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("realtime_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let owner = jarvis_identity::first_user_or_create(&db, "fixture owner").await?;
    let (device_a, token_a) = device_token(&db, owner.id).await;
    let (device_b, token_b) = device_token(&db, owner.id).await;
    let other_owner = jarvis_identity::create_user(&db, "other fixture owner").await?;
    let (_, other_token) = device_token(&db, other_owner.id).await;
    let mut voice_a = VoiceGate::new(device_a);
    let mut voice_b = VoiceGate::new(device_b);
    voice_a.set_enabled(true);
    voice_b.set_enabled(true);
    let count = Arc::new(AtomicUsize::new(0));
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut fixture = state(db.clone(), None).await;
    fixture.llm = Arc::new(Fake(count.clone(), contexts.clone()));
    let hub = fixture.realtime.clone();
    let app = build_router(fixture);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/v1/events", listener.local_addr()?);
    let server_app = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, server_app).await.unwrap();
    });
    assert!(matches!(
        connect_async(&url).await,
        Err(tokio_tungstenite::tungstenite::Error::Http(response))
            if response.status() == StatusCode::UNAUTHORIZED
    ));
    let mut a_req = url.clone().into_client_request()?;
    a_req
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token_a}").parse()?);
    let mut b_req = url.clone().into_client_request()?;
    b_req
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token_b}").parse()?);
    let (mut a, _) = connect_async(a_req).await?;
    let (mut b, _) = connect_async(b_req).await?;
    let mut other_req = url.clone().into_client_request()?;
    other_req.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {other_token}").parse()?,
    );
    let (mut other, _) = connect_async(other_req).await?;
    event(&mut other).await;
    let mut unsafe_req =
        format!("{url}?token=fixture-not-an-authentication-path").into_client_request()?;
    unsafe_req
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token_a}").parse()?);
    assert!(matches!(
        connect_async(unsafe_req).await,
        Err(tokio_tungstenite::tungstenite::Error::Http(response))
            if response.status() == StatusCode::BAD_REQUEST
    ));
    assert!(matches!(
        event(&mut a).await.event,
        Event::ConnectionReady { .. }
    ));
    event(&mut b).await;
    let request =
        json!({"request_id":Uuid::now_v7(),"messages":[{"role":"user","content":"One question"}]});
    let (status, submitted) = post(&app, &token_a, "/v1/assistant/runs", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let (retry_status, retry) = post(&app, &token_a, "/v1/assistant/runs", request.clone()).await;
    assert_eq!(retry_status, StatusCode::OK, "{retry}");
    assert_eq!(submitted["run_id"], retry["run_id"]);
    // Recover a missing HTTP acknowledgement using the original request UUID.
    // The same UUID on another authenticated device must not locate this run.
    for (token, expected) in [
        (&token_a, StatusCode::OK),
        (&token_b, StatusCode::NOT_FOUND),
    ] {
        let recovered = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/assistant/requests/{}",
                        request["request_id"].as_str().unwrap()
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(recovered.status(), expected);
        if expected == StatusCode::OK {
            assert_eq!(json_body(recovered).await["run_id"], submitted["run_id"]);
        }
    }
    let mut streamed = String::new();
    let mut spoken = Vec::new();
    let canonical = loop {
        let first = event(&mut a).await;
        let second = event(&mut b).await;
        assert_eq!(first, second);
        spoken.extend(
            voice_a
                .event(&first.event)
                .into_iter()
                .filter_map(|a| match a {
                    SpeechAction::Speak(text) => Some(text),
                    _ => None,
                }),
        );
        assert!(!voice_b
            .event(&second.event)
            .iter()
            .any(|a| matches!(a, SpeechAction::Speak(_))));
        assert!(
            !matches!(first.event, Event::AssistantFailed { .. }),
            "{first:?}"
        );
        if let Event::AssistantDelta { text, .. } = &first.event {
            streamed.push_str(text);
        }
        if let Event::AssistantCompleted { message, .. } = first.event {
            break message;
        }
    };
    assert_eq!(streamed, canonical.content);
    assert_eq!(
        spoken.join(" "),
        "One canonical answer. No second inference."
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), other.next())
            .await
            .is_err()
    );
    assert_eq!(hub.voice_owner(owner.id).unwrap().0, device_a);
    // Native playback reports are presentation metadata, not model requests.
    // The authenticated non-owner cannot spoof playback for the owner's run.
    assert_eq!(
        post(
            &app,
            &token_b,
            "/v1/voice/playback",
            json!({"run_id":submitted["run_id"],"state":"started"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for status in ["started", "stopped"] {
        assert_eq!(
            post(
                &app,
                &token_a,
                "/v1/voice/playback",
                json!({"run_id":submitted["run_id"],"state":status})
            )
            .await
            .0,
            StatusCode::OK
        );
        loop {
            let first = event(&mut a).await;
            let second = event(&mut b).await;
            assert_eq!(first, second);
            let playback = match first.event {
                Event::VoiceStarted { device_id, run_id } if status == "started" => {
                    Some((device_id, run_id))
                }
                Event::VoiceStopped { device_id, run_id } if status == "stopped" => {
                    Some((device_id, run_id))
                }
                _ => None,
            };
            if let Some((device, run)) = playback {
                assert_eq!(device, device_a);
                assert_eq!(json!(run), submitted["run_id"]);
                break;
            }
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/conversations/{}", canonical.conversation_id))
                .header(header::AUTHORIZATION, format!("Bearer {token_b}"))
                .body(Body::empty())?,
        )
        .await?;
    let history = json_body(response).await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    assert_eq!(history["messages"][1]["content"], canonical.content);
    assert_eq!(
        jarvis_usage::month_statistics(&db).await?.totals.requests,
        1
    );
    // Reconnect and authoritative recovery, without another inference.
    b.close(None).await?;
    let mut reconnect_req = url.clone().into_client_request()?;
    reconnect_req
        .headers_mut()
        .insert(header::AUTHORIZATION, format!("Bearer {token_b}").parse()?);
    let (mut reconnected, _) = connect_async(reconnect_req).await?;
    assert!(matches!(
        event(&mut reconnected).await.event,
        Event::ConnectionReady {
            reconcile: true,
            ..
        }
    ));
    let mut restarted = state(db.clone(), None).await;
    restarted.llm = Arc::new(Fake(count.clone(), contexts.clone()));
    let restarted_app = build_router(restarted);
    let (retry_status, retry) = post(
        &restarted_app,
        &token_a,
        "/v1/assistant/runs",
        request.clone(),
    )
    .await;
    assert_eq!(retry_status, StatusCode::OK);
    assert_eq!(retry["run_id"], submitted["run_id"]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let mut conflict_request = request;
    conflict_request["messages"][0]["content"] = json!("Different prompt with same id");
    assert_eq!(
        post(&app, &token_a, "/v1/assistant/runs", conflict_request)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(
            &app,
            &token_b,
            "/v1/voice/release",
            json!({"run_id":submitted["run_id"]})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(
            &app,
            &token_a,
            "/v1/voice/release",
            json!({"run_id":submitted["run_id"]})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(hub.voice_owner(owner.id).is_none());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // Legacy synchronous callers also fan out one canonical response. No
    // extra classifier inference is allowed merely to preserve their shape.
    let (legacy_status, legacy) = post(
        &app,
        &token_a,
        "/v1/assistant/chat",
        json!({
            "messages":[{"role":"user","content":"Legacy client question"}]
        }),
    )
    .await;
    assert_eq!(legacy_status, StatusCode::OK);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let legacy_message = loop {
        if let Event::AssistantCompleted { message, .. } = event(&mut a).await.event {
            break message;
        }
    };
    let other_copy = loop {
        if let Event::AssistantCompleted { message, .. } = event(&mut reconnected).await.event {
            break message;
        }
    };
    assert_eq!(legacy_message, other_copy);
    assert_eq!(legacy["reply"], legacy_message.content);
    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/v1/conversations/{}",
                    legacy_message.conversation_id
                ))
                .header(header::AUTHORIZATION, format!("Bearer {token_a}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(deleted.status(), StatusCode::OK);
    loop {
        if let Event::ConversationDeleted { conversation_id } = event(&mut reconnected).await.event
        {
            assert_eq!(conversation_id, legacy_message.conversation_id);
            break;
        }
    }
    // Device B has stale (even invented) display history. Only its final new
    // user turn is input; the model receives the canonical server conversation.
    let stale = json!({"request_id":Uuid::now_v7(), "conversation_id":canonical.conversation_id,
        "messages":[{"role":"assistant","content":"Invented stale answer"}, {"role":"user","content":"Next question"}]});
    assert_eq!(
        post(&app, &token_b, "/v1/assistant/runs", stale).await.0,
        StatusCode::OK
    );
    loop {
        if let Event::AssistantCompleted { message, .. } = event(&mut reconnected).await.event {
            assert_eq!(message.conversation_id, canonical.conversation_id);
            break;
        }
    }
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert_eq!(
        contexts.lock().unwrap().last().unwrap(),
        &vec![
            "One question".to_owned(),
            canonical.content.clone(),
            "Next question".to_owned()
        ]
    );
    a.close(None).await?;
    reconnected.close(None).await?;
    other.close(None).await?;
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "requires disposable JARVIS_SURREAL_TEST_* server"]
async fn disconnect_and_provider_failure_never_repeat_or_lose_the_user_message(
) -> Result<(), Box<dyn std::error::Error>> {
    for fail in [false, true] {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("realtime_{}", Uuid::now_v7().simple()))
            .use_db("fixture")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let owner = jarvis_identity::first_user_or_create(&db, "fixture").await?;
        let (device, token) = device_token(&db, owner.id).await;
        let count = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let finish = Arc::new(tokio::sync::Notify::new());
        let mut fixture = state(db.clone(), None).await;
        fixture.llm = Arc::new(PausedFake {
            count: count.clone(),
            started: started.clone(),
            finish: finish.clone(),
            fail,
        });
        let hub = fixture.realtime.clone();
        let display = hub.subscribe(owner.id, device).unwrap();
        let app = build_router(fixture);
        let request = json!({"request_id":Uuid::now_v7(), "messages":[{"role":"user","content":"Keep this question"}]});
        let (status, run) = post(&app, &token, "/v1/assistant/runs", request.clone()).await;
        assert_eq!(status, StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(5), started.notified()).await?;
        let conversation: Uuid = serde_json::from_value(run["conversation_id"].clone())?;
        let running = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/conversations/{conversation}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(json_body(running).await["assistant_running"], true);
        drop(display); // All displays gone, while inference is still running.
        let retry = post(&app, &token, "/v1/assistant/runs", request.clone()).await;
        assert_eq!(retry.0, StatusCode::OK);
        assert_eq!(retry.1["run_id"], run["run_id"]);
        finish.notify_one();
        let conversation: Uuid = serde_json::from_value(run["conversation_id"].clone())?;
        tokio::time::timeout(Duration::from_secs(5), async {
            while hub.run_active(owner.id, conversation) {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/conversations/{conversation}"))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())?,
            )
            .await?;
        let history = json_body(response).await;
        assert_eq!(history["assistant_running"], false);
        assert_eq!(history["messages"][0]["content"], "Keep this question");
        assert_eq!(
            history["messages"].as_array().unwrap().len(),
            if fail { 1 } else { 2 }
        );
        let retry = post(&app, &token, "/v1/assistant/runs", request).await;
        assert_eq!(retry.1["state"], if fail { "failed" } else { "completed" });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        if !fail {
            let usage = jarvis_usage::month_statistics(&db).await?;
            assert_eq!(usage.totals.requests, 1);
            assert!(usage.totals.input_tokens > 0);
            assert!(usage.totals.output_tokens > 0);
        }
    }
    Ok(())
}
