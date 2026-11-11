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
    assert_eq!(kept["expires_at"], Value::Null);
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
    assert!(unkept["expires_at"].is_string());
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
