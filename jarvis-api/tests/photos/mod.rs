use super::*;
use image::{codecs::png::PngEncoder, ImageEncoder, Rgba, RgbaImage};
use jarvis_api::photos::PhotoStore;
use uuid::Uuid;

fn noisy_png(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x9e37_79b9_u32;
    let image = RgbaImage::from_fn(width, height, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let [r, g, b, _] = seed.to_le_bytes();
        Rgba([r, g, b, 255])
    });
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            image.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
    out
}

pub(crate) async fn send(
    app: &axum::Router,
    token: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Upload a small valid photo and return its id.
pub(crate) async fn upload(app: &axum::Router, token: &str) -> Uuid {
    let response = send(
        app,
        token,
        "POST",
        "/v1/photos",
        "image/png",
        noisy_png(8, 8),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
async fn photo_routes_refuse_missing_or_invalid_sessions() {
    let app = build_router(state(jarvis_store::Database::init(), None).await);
    let id = Uuid::new_v4();
    for (method, path) in [
        ("POST", "/v1/photos".to_string()),
        ("GET", format!("/v1/photos/{id}")),
        ("PATCH", format!("/v1/photos/{id}")),
        ("DELETE", format!("/v1/photos/{id}")),
    ] {
        let response = send(&app, "not-a-session", method, &path, "image/png", vec![]).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn photo_routes_are_owner_scoped_bounded_and_inert() -> Result<(), Box<dyn std::error::Error>>
{
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("photos_http_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let owner = jarvis_identity::first_user_or_create(&db, "fixture owner").await?;
    let (_, token) = realtime::device_token(&db, owner.id).await;
    let other_owner = jarvis_identity::create_user(&db, "other fixture owner").await?;
    let (_, other_token) = realtime::device_token(&db, other_owner.id).await;

    // Without a usable photo directory the routes are off, nothing else.
    let off = build_router(state(db.clone(), None).await);
    let response = send(
        &off,
        &token,
        "POST",
        "/v1/photos",
        "image/png",
        noisy_png(8, 8),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let root = tempfile::tempdir()?;
    let dir = root.path().join("photos");
    let mut fixture = state(db.clone(), None).await;
    fixture.photos = Arc::new(PhotoStore::open(&dir));
    let app = build_router(fixture);

    // The route's own body limit replaces the global 2 MiB one: a 3 MiB
    // photo is accepted and comes back sanitized. The Content-Type is
    // ignored; the bytes decide.
    let big = noisy_png(900, 900);
    assert!(big.len() > 2 * 1024 * 1024);
    let response = send(&app, &token, "POST", "/v1/photos", "text/html", big).await;
    assert_eq!(response.status(), StatusCode::OK);
    let uploaded = json_body(response).await;
    assert_eq!(
        (uploaded["width"].as_u64(), uploaded["height"].as_u64()),
        (Some(900), Some(900))
    );
    assert!(uploaded["bytes"].as_u64().unwrap() <= 1024 * 1024);
    let id: Uuid = uploaded["id"].as_str().unwrap().parse()?;
    assert_eq!(id.get_version_num(), 4);

    let too_big = {
        let mut body = noisy_png(8, 8);
        body.resize(15 * 1024 * 1024 + 1, 0);
        body
    };
    let response = send(&app, &token, "POST", "/v1/photos", "image/png", too_big).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let gif = b"GIF89a\x01\0\x01\0\0\0\0;".to_vec();
    let response = send(&app, &token, "POST", "/v1/photos", "image/png", gif).await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        json_body(response).await["error"],
        "only JPEG, PNG and WebP photos are supported"
    );

    // The owner reads an inert, private JPEG.
    let path = format!("/v1/photos/{id}");
    let response = send(&app, &token, "GET", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::OK);
    for (name, value) in [
        ("content-type", "image/jpeg"),
        ("cache-control", "private, no-store"),
        ("x-content-type-options", "nosniff"),
        ("content-disposition", "inline"),
        ("content-security-policy", "default-src 'none'"),
    ] {
        assert_eq!(response.headers()[name], value, "{name}");
    }
    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024).await?;
    assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
    let file = std::fs::metadata(dir.join(format!("{id}.jpg")))?;
    use std::os::unix::fs::MetadataExt;
    assert_eq!(file.mode() & 0o7777, 0o600);

    // Another owner's photo is indistinguishable from a missing one.
    for (method, body) in [
        ("GET", vec![]),
        ("PATCH", br#"{"saved":true}"#.to_vec()),
        ("DELETE", vec![]),
    ] {
        let response = send(&app, &other_token, method, &path, "application/json", body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
        assert_eq!(json_body(response).await["error"], "no such photo");
    }
    let missing = format!("/v1/photos/{}", Uuid::new_v4());
    let response = send(&app, &token, "GET", &missing, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Keep and un-keep; strict, tiny bodies.
    let response = send(
        &app,
        &token,
        "PATCH",
        &path,
        "application/json",
        br#"{"saved":true}"#.to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let kept = json_body(response).await;
    assert_eq!(kept["saved"], true);
    // Not sent yet: it still goes a day after upload.
    let day_after = kept["expires_at"].as_str().unwrap().to_string();
    // Once sent, a kept photo never expires.
    db.query("UPDATE chat_photos SET conversation_id = 'c' WHERE record::id(id) = $id")
        .bind(json!({"id": id.to_string()}))
        .await?
        .check()?;
    let response = send(
        &app,
        &token,
        "PATCH",
        &path,
        "application/json",
        br#"{"saved":true}"#.to_vec(),
    )
    .await;
    assert_eq!(json_body(response).await["expires_at"], Value::Null);
    let response = send(
        &app,
        &token,
        "PATCH",
        &path,
        "application/json",
        br#"{"saved":false}"#.to_vec(),
    )
    .await;
    let unkept = json_body(response).await;
    assert_eq!(unkept["saved"], false);
    assert!(unkept["expires_at"].as_str().unwrap() > day_after.as_str());
    for body in [
        br#"{"saved":true,"user_id":"x"}"#.to_vec(),
        br#"{"saved":"yes"}"#.to_vec(),
        format!(r#"{{"saved":true{}}}"#, " ".repeat(2048)).into_bytes(),
    ] {
        let response = send(&app, &token, "PATCH", &path, "application/json", body).await;
        assert!(response.status().is_client_error());
    }
    let missing_patch = send(
        &app,
        &token,
        "PATCH",
        &missing,
        "application/json",
        br#"{"saved":true}"#.to_vec(),
    )
    .await;
    assert_eq!(missing_patch.status(), StatusCode::NOT_FOUND);

    // A row whose file is gone reads as missing.
    let second = upload(&app, &token).await;
    std::fs::remove_file(dir.join(format!("{second}.jpg")))?;
    let response = send(
        &app,
        &token,
        "GET",
        &format!("/v1/photos/{second}"),
        "",
        vec![],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Delete removes the file and the row.
    let response = send(&app, &token, "DELETE", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!dir.join(format!("{id}.jpg")).exists());
    let response = send(&app, &token, "GET", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = send(&app, &token, "DELETE", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Fifty uploads per owner per rolling day; the other owner is not affected.
    db.query(
        "FOR $i IN array::repeat(0, 49) { CREATE chat_photos SET id = <string>rand::uuid::v4(), user_id = $user, \
         conversation_id = NONE, saved = false, width = 1, height = 1, bytes = 1, created_at = time::now() - 1h; }",
    )
    .bind(json!({"user": owner.id.to_string()}))
    .await?
    .check()?;
    let response = send(
        &app,
        &token,
        "POST",
        "/v1/photos",
        "image/png",
        noisy_png(8, 8),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        json_body(response).await["error"],
        "daily photo upload limit reached"
    );
    upload(&app, &other_token).await;
    // Two GiB kept in total.
    db.query(
        "UPDATE chat_photos SET created_at = time::now() - 2d WHERE user_id = $user; \
         FOR $i IN array::repeat(0, 2048) { CREATE chat_photos SET id = <string>rand::uuid::v4(), user_id = $user, \
         conversation_id = NONE, saved = true, width = 1, height = 1, bytes = 1048576, created_at = time::now() - 2d; }",
    )
    .bind(json!({"user": owner.id.to_string()}))
    .await?
    .check()?;
    let response = send(
        &app,
        &token,
        "POST",
        "/v1/photos",
        "image/png",
        noisy_png(8, 8),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
    Ok(())
}

#[tokio::test]
#[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
async fn photos_join_a_message_atomically_and_leave_with_its_conversation(
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
    db.signin(Root {
        username: &env::var("JARVIS_SURREAL_TEST_USER")?,
        password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
    })
    .await?;
    db.use_ns(format!("photos_runs_{}", Uuid::now_v7().simple()))
        .use_db("fixture")
        .await?;
    jarvis_store::apply_baseline_schema(&db).await?;
    let owner = jarvis_identity::first_user_or_create(&db, "fixture owner").await?;
    let (device, token) = realtime::device_token(&db, owner.id).await;
    let other_owner = jarvis_identity::create_user(&db, "other fixture owner").await?;
    let (other_device, other_token) = realtime::device_token(&db, other_owner.id).await;
    let root = tempfile::tempdir()?;
    let dir = root.path().join("photos");
    let mut fixture = state(db.clone(), None).await;
    fixture.photos = Arc::new(PhotoStore::open(&dir));
    let hub = fixture.realtime.clone();
    let app = build_router(fixture);
    let mut display = hub.subscribe(owner.id, device).unwrap();
    let mut other_display = hub.subscribe(other_owner.id, other_device).unwrap();
    for display in [&mut display, &mut other_display] {
        assert!(matches!(
            display.receiver.recv().await.unwrap().event,
            jarvis_client_core::realtime::Event::ConnectionReady { .. }
        ));
    }

    let first = upload(&app, &token).await;
    let second = upload(&app, &token).await;
    let foreign = upload(&app, &other_token).await;
    let pending = |id: Uuid| {
        let db = db.clone();
        async move {
            let rows: Vec<bool> = db
                .query("SELECT VALUE conversation_id = NONE FROM chat_photos WHERE record::id(id) = $id")
                .bind(json!({"id": id.to_string()}))
                .await
                .unwrap()
                .take(0)
                .unwrap();
            rows == [true]
        }
    };
    let counts = || {
        let db = db.clone();
        async move {
            let mut response = db
                .query("RETURN [count(SELECT * FROM conversations), count(SELECT * FROM chat_messages WHERE role = 'user'), count(SELECT * FROM assistant_runs)]")
                .await
                .unwrap();
            response.take::<Vec<u64>>(0).unwrap()
        }
    };
    let run = |attachments: Vec<Uuid>, content: &str| {
        json!({"request_id": Uuid::now_v7(), "messages": [{"role": "user", "content": content}],
            "attachments": attachments})
    };

    // Another owner's photo rolls the whole submission back.
    let (status, body) = realtime::post(
        &app,
        &token,
        "/v1/assistant/runs",
        run(vec![first, foreign], "Look"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "no such photo");
    assert!(pending(first).await && pending(foreign).await);
    assert_eq!(counts().await, [0, 0, 0]);
    // Photos never go to web research or the legacy endpoint.
    let mut research = run(vec![first], "Look");
    research["mode"] = json!("research");
    let (status, _) = realtime::post(&app, &token, "/v1/assistant/runs", research).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut legacy = run(vec![first], "Look");
    legacy.as_object_mut().unwrap().remove("request_id");
    let (status, _) = realtime::post(&app, &token, "/v1/assistant/chat", legacy).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(counts().await, [0, 0, 0]);

    // Photos alone are a message; a retry is the same run.
    let request = run(vec![second, first], "");
    let (status, submitted) =
        realtime::post(&app, &token, "/v1/assistant/runs", request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let (status, retried) = realtime::post(&app, &token, "/v1/assistant/runs", request).await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    assert_eq!(submitted["run_id"], retried["run_id"]);
    assert!(!pending(first).await && !pending(second).await);
    // A photo already sent cannot join another message.
    let (status, _) = realtime::post(
        &app,
        &token,
        "/v1/assistant/runs",
        run(vec![first], "Again"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(counts().await, [1, 1, 1]);

    // Live displays get the ids, never the bytes; the other owner gets nothing.
    let created = loop {
        let envelope =
            tokio::time::timeout(std::time::Duration::from_secs(5), display.receiver.recv())
                .await?
                .unwrap();
        if let jarvis_client_core::realtime::Event::MessageCreated { message, .. } = &envelope.event
        {
            break message.clone();
        }
    };
    assert_eq!(created.attachments, [second, first]);
    assert_eq!(created.content, "");
    assert!(other_display.receiver.try_recv().is_err());

    let conversation = submitted["conversation_id"].as_str().unwrap().to_string();
    let run_path = format!(
        "/v1/assistant/runs/{}",
        submitted["run_id"].as_str().unwrap()
    );
    for _ in 0..100 {
        let response = send(&app, &token, "GET", &run_path, "", vec![]).await;
        if json_body(response).await["state"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // A photo about to expire unsent is not claimed.
    let late = upload(&app, &token).await;
    db.query("UPDATE chat_photos SET created_at = time::now() - 23h30m WHERE record::id(id) = $id")
        .bind(json!({"id": late.to_string()}))
        .await?
        .check()?;
    let (status, _) =
        realtime::post(&app, &token, "/v1/assistant/runs", run(vec![late], "Late")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(pending(late).await);
    // A continued conversation takes photos too.
    let third = upload(&app, &token).await;
    let mut more = run(vec![third], "And this one");
    more["conversation_id"] = json!(submitted["conversation_id"]);
    let mut status = StatusCode::CONFLICT;
    for _ in 0..100 {
        (status, _) = realtime::post(&app, &token, "/v1/assistant/runs", more.clone()).await;
        if status != StatusCode::CONFLICT {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(status, StatusCode::OK);
    assert!(!pending(third).await);
    let path = format!("/v1/conversations/{conversation}");
    let response = send(&app, &token, "GET", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let history = json_body(response).await;
    assert_eq!(history["title"], "Photo");
    assert_eq!(
        history["messages"][0]["attachments"],
        json!([second.to_string(), first.to_string()])
    );
    let mut listed: Vec<String> = history["photos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            assert!(p["expires_at"].is_string());
            p["id"].as_str().unwrap().to_string()
        })
        .collect();
    listed.sort();
    let mut want = vec![first.to_string(), second.to_string(), third.to_string()];
    want.sort();
    assert_eq!(listed, want);
    let response = send(&app, &other_token, "GET", &path, "", vec![]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Deleting the conversation deletes its photos, kept ones too.
    let response = send(
        &app,
        &token,
        "PATCH",
        &format!("/v1/photos/{first}"),
        "application/json",
        br#"{"saved":true}"#.to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // The finished run releases the conversation a moment after it is stored.
    let mut deleted = StatusCode::CONFLICT;
    for _ in 0..100 {
        deleted = send(&app, &token, "DELETE", &path, "", vec![])
            .await
            .status();
        if deleted != StatusCode::CONFLICT {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(deleted, StatusCode::OK);
    for id in [first, second, third] {
        assert!(!dir.join(format!("{id}.jpg")).exists());
        let response = send(&app, &token, "GET", &format!("/v1/photos/{id}"), "", vec![]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    // The other owner's pending photo is untouched.
    assert!(pending(foreign).await);
    assert!(dir.join(format!("{foreign}.jpg")).exists());
    Ok(())
}
