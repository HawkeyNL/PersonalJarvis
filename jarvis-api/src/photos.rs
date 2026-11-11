//! Chat photos: the sanitizer, on-disk storage and the owner's photo routes.
//!
//! Every upload is typed by its magic bytes (the Content-Type header is
//! ignored), decoded with hard limits, one decode at a time, and re-encoded
//! as a fresh baseline JPEG of at most 1600 px and 1 MiB. Nothing of the
//! original file survives: no EXIF, GPS, XMP, ICC profile or trailing data.
//!
//! Files live in one private directory, named only from parsed UUIDs. The
//! database holds the metadata; every query is scoped to the owner, and a
//! photo that is not the caller's is indistinguishable from a missing one.
//! Logs carry counts only, never ids or image content.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::{
    body::Body,
    extract::{Path as UrlPath, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use image::{
    codecs::jpeg::JpegEncoder, imageops::FilterType, metadata::Orientation, DynamicImage,
    ImageDecoder, ImageError, ImageFormat, ImageReader, Limits, RgbImage,
};
use serde::Deserialize;
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::rate_limit::allow_authenticated_device;
use crate::{AppState, Authed};

/// The largest upload body the photo route reads.
pub const MAX_UPLOAD_BYTES: usize = 15 * 1024 * 1024;
/// The largest stored photo; also enforced by the schema.
pub const MAX_PHOTO_BYTES: usize = 1024 * 1024;
/// The longest side of a stored photo; also enforced by the schema.
pub const MAX_SIDE: u32 = 1600;
/// Unsaved photos are deleted this long after their upload.
pub const RETENTION_DAYS: i64 = 90;
/// The most photos one message carries; also enforced by the schema.
pub const MAX_ATTACHMENTS: usize = 4;
/// PATCH bodies are tiny JSON objects.
pub const PATCH_BODY_LIMIT: usize = 1024;

const MAX_DIMENSION: u32 = 10_000;
const MAX_PIXELS: u64 = 40_000_000;
const MAX_DECODE_ALLOC: u64 = 192 * 1024 * 1024;
const DECODE_TIMEOUT: Duration = Duration::from_secs(20);
const QUALITY: u8 = 82;
const FALLBACK_QUALITY: u8 = 65;
const UPLOADS_PER_MIN: u32 = 30;
const READS_PER_MIN: u32 = 240;
const WRITES_PER_MIN: u32 = 60;
const UPLOADS_PER_DAY: u32 = 50;
/// Uploads whose body may be in memory at once, across all devices.
const UPLOADS_IN_FLIGHT: usize = 2;
const PENDING_HOURS: i64 = 24;
const MAX_OWNER_BYTES: i64 = 2 * 1024 * 1024 * 1024;
const PRUNE_BATCH: usize = 500;
/// Most orphan candidates handled per pass; the whole directory is scanned.
const SWEEP_MAX_CANDIDATES: usize = 10_000;
const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);
const TMP_PREFIX: &str = ".tmp-";

/// Why a photo request failed. Each maps to a fixed, non-secret message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoError {
    /// Not JPEG, PNG or WebP by magic bytes.
    Unsupported,
    /// The upload could not be decoded.
    Invalid,
    /// Too many pixels, too wide or tall, or still over 1 MiB as JPEG.
    TooLarge,
    /// Another decode held the slot for too long, or a decode timed out.
    Busy,
    /// The photo directory failed its startup checks.
    Unavailable,
    RateLimited,
    DailyLimit,
    StorageFull,
    NotFound,
    Internal,
}

impl PhotoError {
    fn status_and_message(self) -> (StatusCode, &'static str) {
        match self {
            Self::Unsupported => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "only JPEG, PNG and WebP photos are supported",
            ),
            Self::Invalid => (StatusCode::BAD_REQUEST, "the photo could not be read"),
            Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "the photo is too large"),
            Self::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "photo processing is busy; try again",
            ),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "photos are unavailable"),
            Self::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
            Self::DailyLimit => (
                StatusCode::TOO_MANY_REQUESTS,
                "daily photo upload limit reached",
            ),
            Self::StorageFull => (StatusCode::INSUFFICIENT_STORAGE, "photo storage is full"),
            Self::NotFound => (StatusCode::NOT_FOUND, "no such photo"),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }
}

impl std::fmt::Display for PhotoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.status_and_message().1)
    }
}

impl std::error::Error for PhotoError {}

impl IntoResponse for PhotoError {
    fn into_response(self) -> Response {
        let (status, message) = self.status_and_message();
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// A sanitized photo: a fresh baseline JPEG and its size.
#[derive(Debug)]
pub struct Sanitized {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageFormat::Png)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::WebP)
    } else {
        None
    }
}

fn decode_error(error: ImageError) -> PhotoError {
    match error {
        ImageError::Limits(_) => PhotoError::TooLarge,
        _ => PhotoError::Invalid,
    }
}

/// Decode an upload with hard limits and re-encode it as a clean JPEG.
/// CPU-bound: call it from a blocking thread.
pub fn sanitize(input: &[u8]) -> Result<Sanitized, PhotoError> {
    let format = sniff(input).ok_or(PhotoError::Unsupported)?;
    let mut reader = ImageReader::with_format(Cursor::new(input), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    // Refuse a decompression bomb from its header, before any pixel work.
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(PhotoError::Invalid);
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(PhotoError::TooLarge);
    }
    // The decoders do not count their output buffer against `max_alloc`.
    if decoder.total_bytes() > MAX_DECODE_ALLOC {
        return Err(PhotoError::TooLarge);
    }
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    // An animated image yields its first frame.
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    image.apply_orientation(orientation);
    let mut rgb = flatten_on_white(image);
    if rgb.width() > MAX_SIDE || rgb.height() > MAX_SIDE {
        rgb = DynamicImage::ImageRgb8(rgb)
            .resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle)
            .into_rgb8();
    }
    for quality in [QUALITY, FALLBACK_QUALITY] {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&rgb)
            .map_err(|_| PhotoError::Internal)?;
        if jpeg.len() <= MAX_PHOTO_BYTES {
            return Ok(Sanitized {
                jpeg,
                width: rgb.width(),
                height: rgb.height(),
            });
        }
    }
    Err(PhotoError::TooLarge)
}

/// Transparent pixels become white; the output has no alpha channel.
fn flatten_on_white(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let rgba = image.into_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let a = u16::from(a);
        let blend = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([blend(r), blend(g), blend(b)])
    })
}

/// The photo directory and the single decode slot. Built once at startup;
/// without a directory that passed its checks every photo route answers 503
/// and the rest of Core runs normally.
pub struct PhotoStore {
    dir: Option<PathBuf>,
    uploads: Arc<Semaphore>,
    decode: Arc<Semaphore>,
}

impl Default for PhotoStore {
    /// Unavailable: photo routes answer 503.
    fn default() -> Self {
        Self {
            dir: None,
            uploads: Arc::new(Semaphore::new(UPLOADS_IN_FLIGHT)),
            decode: Arc::new(Semaphore::new(1)),
        }
    }
}

impl PhotoStore {
    /// Create the directory 0700 if it is missing, then require a real
    /// directory (not a symlink) owned by this user with mode exactly 0700.
    /// Blocking: call it at startup or from a blocking thread.
    pub fn open(dir: &Path) -> Self {
        match prepare_dir(dir) {
            Ok(()) => Self {
                dir: Some(dir.to_path_buf()),
                ..Self::default()
            },
            Err(_) => {
                tracing::warn!("photo storage failed its checks; photo routes are off");
                Self::default()
            }
        }
    }

    pub fn is_available(&self) -> bool {
        self.dir.is_some()
    }

    fn dir(&self) -> Result<PathBuf, PhotoError> {
        self.dir.clone().ok_or(PhotoError::Unavailable)
    }
}

fn prepare_dir(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if !meta.file_type().is_dir() || meta.uid() != euid || meta.mode() & 0o7777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe photo directory",
        ));
    }
    Ok(())
}

fn file_path(dir: &Path, id: Uuid) -> PathBuf {
    dir.join(format!("{}.jpg", id.hyphenated()))
}

/// Write a new photo file 0600: a fresh temporary file in the same
/// directory, flushed, renamed into place, then the directory flushed.
fn write_file(dir: &Path, id: Uuid, jpeg: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!("{TMP_PREFIX}{}", Uuid::new_v4().hyphenated()));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(jpeg)?;
        file.sync_all()?;
        fs::rename(&tmp, file_path(dir, id))?;
        File::open(dir)?.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_file(file_path(dir, id));
    }
    written
}

/// Read a stored photo, bounded, and only if it still looks like a JPEG.
fn read_file(dir: &Path, id: Uuid) -> Result<Vec<u8>, PhotoError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file_path(dir, id))
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => PhotoError::NotFound,
            _ => PhotoError::Internal,
        })?;
    if !file.metadata().is_ok_and(|meta| meta.file_type().is_file()) {
        return Err(PhotoError::Internal);
    }
    let mut bytes = Vec::new();
    file.take(MAX_PHOTO_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PhotoError::Internal)?;
    if bytes.len() > MAX_PHOTO_BYTES || !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err(PhotoError::Internal);
    }
    Ok(bytes)
}

/// Remove a photo file; a file that is already gone is fine.
fn remove_file(dir: &Path, id: Uuid) -> io::Result<()> {
    match fs::remove_file(file_path(dir, id)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, PhotoError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| PhotoError::Internal)
}

/// Remove the files of deleted photo rows, after their rows are gone. Used
/// when a conversation is deleted; the orphan sweep is the backstop.
pub(crate) async fn remove_files(store: &PhotoStore, ids: Vec<Uuid>) {
    let Ok(dir) = store.dir() else {
        return;
    };
    let count = ids.len();
    let failed = blocking(move || {
        ids.into_iter()
            .filter(|id| remove_file(&dir, *id).is_err())
            .count()
    })
    .await
    .unwrap_or(count);
    if failed > 0 {
        tracing::warn!(failed, "photo files left for the orphan sweep");
    }
}

fn database_error<E>(_: E) -> PhotoError {
    PhotoError::Internal
}

async fn owns(state: &AppState, user: Uuid, id: Uuid) -> Result<bool, PhotoError> {
    let rows: Vec<String> = state
        .db
        .query("SELECT VALUE record::id(id) FROM chat_photos WHERE record::id(id) = $id AND user_id = $user LIMIT 1")
        .bind(json!({"id": id.to_string(), "user": user.to_string()}))
        .await
        .map_err(database_error)?
        .take(0)
        .map_err(database_error)?;
    Ok(!rows.is_empty())
}

#[derive(Deserialize)]
struct Usage {
    uploads: u32,
    bytes: i64,
}

/// Per-owner caps: uploads in the last 24 hours and bytes kept in total.
async fn check_caps(state: &AppState, user: Uuid) -> Result<(), PhotoError> {
    let usage: Option<Usage> = state
        .db
        .query(
            "RETURN { \
             uploads: (SELECT count() AS n FROM chat_photos WHERE user_id = $user AND created_at > time::now() - 24h GROUP ALL)[0].n ?? 0, \
             bytes: (SELECT math::sum(bytes) AS n FROM chat_photos WHERE user_id = $user GROUP ALL)[0].n ?? 0 }",
        )
        .bind(json!({"user": user.to_string()}))
        .await
        .map_err(database_error)?
        .take(0)
        .map_err(database_error)?;
    let usage = usage.ok_or(PhotoError::Internal)?;
    // Rows only count photos that still exist; the in-memory counter also
    // counts deleted ones, so upload-and-delete cannot reset the day.
    // ponytail: the counter resets on restart; a ledger table if that matters.
    let day = Duration::from_secs(24 * 60 * 60);
    if usage.uploads >= UPLOADS_PER_DAY
        || !state.rate_limiter.check_reserved(
            &format!("photo-day:user:{user}"),
            UPLOADS_PER_DAY,
            day,
        )
    {
        return Err(PhotoError::DailyLimit);
    }
    if usage.bytes.saturating_add(MAX_PHOTO_BYTES as i64) > MAX_OWNER_BYTES {
        return Err(PhotoError::StorageFull);
    }
    Ok(())
}

/// POST /v1/photos: a raw image body. Sanitize, store, then record it as an
/// unsent photo of the caller.
pub(crate) async fn upload(
    auth: Authed,
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<serde_json::Value>, PhotoError> {
    if !allow_authenticated_device(&state, auth.device.id, "photo", UPLOADS_PER_MIN) {
        return Err(PhotoError::RateLimited);
    }
    let dir = state.photos.dir()?;
    // ponytail: caps are checked before the insert, so uploads racing each
    // other can pass a cap by a few photos; the in-flight slots bound it.
    check_caps(&state, auth.user.id).await?;
    // The body is read only after every cheap check, and only by a bounded
    // number of uploads at once, each holding at most 15 MiB.
    let _slot = state
        .photos
        .uploads
        .clone()
        .try_acquire_owned()
        .map_err(|_| PhotoError::Busy)?;
    let body = axum::body::to_bytes(body, MAX_UPLOAD_BYTES)
        .await
        .map_err(|_| PhotoError::TooLarge)?;
    if sniff(&body).is_none() {
        return Err(PhotoError::Unsupported);
    }
    let decode = state.photos.decode.clone();
    // The permit moves into the blocking task, so even after a timeout no
    // second decode starts until the first one has really finished.
    let sanitized = tokio::time::timeout(DECODE_TIMEOUT, async move {
        let permit = decode
            .acquire_owned()
            .await
            .map_err(|_| PhotoError::Internal)?;
        blocking(move || {
            let _permit = permit;
            sanitize(&body)
        })
        .await?
    })
    .await
    .map_err(|_| PhotoError::Busy)??;
    let id = Uuid::new_v4();
    let Sanitized {
        jpeg,
        width,
        height,
    } = sanitized;
    let bytes = jpeg.len();
    let write_dir = dir.clone();
    blocking(move || write_file(&write_dir, id, &jpeg))
        .await?
        .map_err(|_| PhotoError::Internal)?;
    let created = state
        .db
        .query(
            "CREATE chat_photos SET id = $id, user_id = $user, conversation_id = NONE, saved = false, \
             width = $width, height = $height, bytes = $bytes, created_at = time::now() RETURN NONE",
        )
        .bind(json!({"id": id.to_string(), "user": auth.user.id.to_string(),
            "width": width, "height": height, "bytes": bytes}))
        .await
        .map(|response| response.check().is_ok());
    if !matches!(created, Ok(true)) {
        let _ = blocking(move || remove_file(&dir, id)).await;
        return Err(PhotoError::Internal);
    }
    Ok(Json(
        json!({"id": id, "width": width, "height": height, "bytes": bytes}),
    ))
}

/// GET /v1/photos/{id}: the caller's photo as a private, inert JPEG.
pub(crate) async fn download(
    auth: Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<Uuid>,
) -> Result<Response, PhotoError> {
    if !allow_authenticated_device(&state, auth.device.id, "photo-read", READS_PER_MIN) {
        return Err(PhotoError::RateLimited);
    }
    let dir = state.photos.dir()?;
    if !owns(&state, auth.user.id, id).await? {
        return Err(PhotoError::NotFound);
    }
    let bytes = blocking(move || read_file(&dir, id)).await??;
    let mut response = bytes.into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, "image/jpeg"),
        (header::CACHE_CONTROL, "private, no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_DISPOSITION, "inline"),
        (header::CONTENT_SECURITY_POLICY, "default-src 'none'"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchReq {
    saved: bool,
}

#[derive(Deserialize)]
struct Kept {
    saved: bool,
    pending: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

/// When a photo expires: 24 hours after upload while no message carries
/// it (kept or not), 90 days after upload once sent, never once sent and
/// kept.
pub(crate) fn expires_at(saved: bool, pending: bool, created_at: OffsetDateTime) -> Option<String> {
    let after = if pending {
        time::Duration::hours(PENDING_HOURS)
    } else if saved {
        return None;
    } else {
        time::Duration::days(RETENTION_DAYS)
    };
    (created_at + after)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// PATCH /v1/photos/{id}: keep (`saved: true`) or stop keeping a photo.
pub(crate) async fn update(
    auth: Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<Uuid>,
    Json(req): Json<PatchReq>,
) -> Result<Json<serde_json::Value>, PhotoError> {
    if !allow_authenticated_device(&state, auth.device.id, "photo-write", WRITES_PER_MIN) {
        return Err(PhotoError::RateLimited);
    }
    state.photos.dir()?;
    let rows: Vec<Kept> = state
        .db
        .query(
            "UPDATE chat_photos SET saved = $saved WHERE record::id(id) = $id AND user_id = $user \
             RETURN saved, conversation_id = NONE AS pending, created_at",
        )
        .bind(json!({"id": id.to_string(), "user": auth.user.id.to_string(), "saved": req.saved}))
        .await
        .map_err(database_error)?
        .take(0)
        .map_err(database_error)?;
    let kept = rows.into_iter().next().ok_or(PhotoError::NotFound)?;
    Ok(Json(json!({
        "id": id,
        "saved": kept.saved,
        "expires_at": expires_at(kept.saved, kept.pending, kept.created_at),
    })))
}

/// DELETE /v1/photos/{id}: remove the file, then its row.
pub(crate) async fn delete(
    auth: Authed,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<Uuid>,
) -> Result<StatusCode, PhotoError> {
    if !allow_authenticated_device(&state, auth.device.id, "photo-write", WRITES_PER_MIN) {
        return Err(PhotoError::RateLimited);
    }
    let dir = state.photos.dir()?;
    if !owns(&state, auth.user.id, id).await? {
        return Err(PhotoError::NotFound);
    }
    blocking(move || remove_file(&dir, id))
        .await?
        .map_err(|_| PhotoError::Internal)?;
    state
        .db
        .query("DELETE chat_photos WHERE record::id(id) = $id AND user_id = $user")
        .bind(json!({"id": id.to_string(), "user": auth.user.id.to_string()}))
        .await
        .map_err(database_error)?
        .check()
        .map_err(database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Unsaved photos 90 days after upload, and photos no message carries 24
/// hours after upload. Sending only claims photos younger than 23 hours, so
/// a pass never removes a photo that is being sent.
const EXPIRED: &str = "(saved = false AND created_at < time::now() - 90d) \
    OR (conversation_id = NONE AND created_at < time::now() - 24h)";

/// What one retention pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub expired: usize,
    pub orphans: usize,
}

/// Retention: delete unsaved photos 90 days after upload and photos never
/// sent within 24 hours (file first, then row), then sweep files that have
/// no row. Runs from the hourly maintenance; each pass is bounded.
pub async fn prune(db: &jarvis_store::Database, store: &PhotoStore) -> Result<Pruned, PhotoError> {
    let dir = store.dir()?;
    let expired: Vec<String> = db
        .query(format!(
            "SELECT VALUE record::id(id) FROM chat_photos WHERE {EXPIRED} LIMIT $limit"
        ))
        .bind(json!({"limit": PRUNE_BATCH}))
        .await
        .map_err(database_error)?
        .take(0)
        .map_err(database_error)?;
    let expired: Vec<Uuid> = expired
        .iter()
        .filter_map(|id| Uuid::try_parse(id).ok())
        .collect();
    let unlink_dir = dir.clone();
    let unlinked: Vec<String> = blocking(move || {
        expired
            .into_iter()
            .filter(|id| remove_file(&unlink_dir, *id).is_ok())
            .map(|id| id.to_string())
            .collect()
    })
    .await?;
    if !unlinked.is_empty() {
        // The condition again: a photo kept meanwhile keeps its row.
        db.query(format!(
            "DELETE chat_photos WHERE record::id(id) IN $ids AND ({EXPIRED})"
        ))
        .bind(json!({"ids": unlinked}))
        .await
        .map_err(database_error)?
        .check()
        .map_err(database_error)?;
    }
    let orphans = sweep_orphans(db, dir).await?;
    Ok(Pruned {
        expired: unlinked.len(),
        orphans,
    })
}

enum Entry {
    Tmp(PathBuf),
    Photo(Uuid),
}

/// Files older than an hour that are leftover temporaries, or photo files
/// whose row is gone. Anything else in the directory is left alone.
async fn sweep_orphans(db: &jarvis_store::Database, dir: PathBuf) -> Result<usize, PhotoError> {
    let list_dir = dir.clone();
    let entries = blocking(move || list_old_entries(&list_dir)).await??;
    let (tmps, photos): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|e| matches!(e, Entry::Tmp(_)));
    let photos: Vec<Uuid> = photos
        .into_iter()
        .filter_map(|e| match e {
            Entry::Photo(id) => Some(id),
            Entry::Tmp(_) => None,
        })
        .collect();
    let mut orphans = Vec::new();
    for chunk in photos.chunks(PRUNE_BATCH) {
        let ids: Vec<String> = chunk.iter().map(Uuid::to_string).collect();
        let known: Vec<String> = db
            .query("SELECT VALUE record::id(id) FROM chat_photos WHERE record::id(id) IN $ids")
            .bind(json!({"ids": ids}))
            .await
            .map_err(database_error)?
            .take(0)
            .map_err(database_error)?;
        orphans.extend(
            chunk
                .iter()
                .filter(|id| !known.contains(&id.to_string()))
                .copied(),
        );
    }
    blocking(move || {
        let tmps = tmps
            .into_iter()
            .filter_map(|e| match e {
                Entry::Tmp(path) => fs::remove_file(path).ok(),
                Entry::Photo(_) => None,
            })
            .count();
        tmps + orphans
            .into_iter()
            .filter(|id| remove_file(&dir, *id).is_ok())
            .count()
    })
    .await
}

fn list_old_entries(dir: &Path) -> Result<Vec<Entry>, PhotoError> {
    let cutoff = SystemTime::now()
        .checked_sub(ORPHAN_MIN_AGE)
        .ok_or(PhotoError::Internal)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|_| PhotoError::Internal)? {
        if entries.len() >= SWEEP_MAX_CANDIDATES {
            break;
        }
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() || meta.modified().map_or(true, |m| m > cutoff) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(TMP_PREFIX) {
            entries.push(Entry::Tmp(entry.path()));
        } else if let Some(id) = name.strip_suffix(".jpg").and_then(|stem| {
            Uuid::try_parse(stem)
                .ok()
                .filter(|id| id.hyphenated().to_string() == stem)
        }) {
            entries.push(Entry::Photo(id));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::png::PngEncoder, ImageEncoder, Rgb, Rgba, RgbaImage};

    fn jpeg(image: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(image)
            .unwrap();
        out
    }

    fn png(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }

    fn webp(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }

    /// A big-endian TIFF block with Orientation and a GPS IFD holding a
    /// latitude reference: what a phone camera writes into APP1.
    fn exif_app1(orientation: u16) -> Vec<u8> {
        let mut tiff = b"MM\0\x2a\0\0\0\x08".to_vec();
        tiff.extend([0, 2]);
        tiff.extend([0x01, 0x12, 0, 3, 0, 0, 0, 1]);
        tiff.extend(orientation.to_be_bytes());
        tiff.extend([0, 0]);
        tiff.extend([0x88, 0x25, 0, 4, 0, 0, 0, 1, 0, 0, 0, 38]);
        tiff.extend([0, 0, 0, 0]);
        assert_eq!(tiff.len(), 38);
        tiff.extend([0, 1]);
        tiff.extend([0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
        tiff.extend([0, 0, 0, 0]);
        let mut segment = vec![0xFF, 0xE1];
        segment.extend(((2 + 6 + tiff.len()) as u16).to_be_bytes());
        segment.extend(b"Exif\0\0");
        segment.extend(tiff);
        segment
    }

    /// Insert segments right after SOI.
    fn with_segments(jpeg: &[u8], segments: &[u8]) -> Vec<u8> {
        let mut out = jpeg[..2].to_vec();
        out.extend(segments);
        out.extend(&jpeg[2..]);
        out
    }

    fn has_marker(jpeg: &[u8], marker: u8) -> bool {
        jpeg.windows(2).any(|w| w == [0xFF, marker])
    }

    fn assert_clean(out: &Sanitized) {
        assert!(out.jpeg.starts_with(&[0xFF, 0xD8, 0xFF]));
        assert!(out.jpeg.len() <= MAX_PHOTO_BYTES);
        assert!(out.width <= MAX_SIDE && out.height <= MAX_SIDE);
        assert!(!out.jpeg.windows(6).any(|w| w == b"Exif\0\0"));
        // No APP1 (EXIF/XMP) or APP2 (ICC) segment.
        assert!(!has_marker(&out.jpeg, 0xE1));
        assert!(!has_marker(&out.jpeg, 0xE2));
        let decoded = image::load_from_memory_with_format(&out.jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (out.width, out.height));
    }

    fn halves(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        })
    }

    #[test]
    fn every_supported_format_round_trips_to_a_clean_jpeg() {
        let rgba = RgbaImage::from_pixel(64, 48, Rgba([10, 200, 30, 255]));
        for input in [jpeg(&halves(64, 48)), png(&rgba), webp(&rgba)] {
            let out = sanitize(&input).unwrap();
            assert_clean(&out);
            assert_eq!((out.width, out.height), (64, 48));
        }
    }

    #[test]
    fn gps_exif_is_stripped() {
        let input = with_segments(&jpeg(&halves(32, 32)), &exif_app1(1));
        assert!(input.windows(6).any(|w| w == b"Exif\0\0"));
        let out = sanitize(&input).unwrap();
        assert_clean(&out);
        assert_eq!((out.width, out.height), (32, 32));
    }

    #[test]
    fn orientation_six_is_applied_before_the_metadata_is_dropped() {
        // Red left, blue right; rotated 90 degrees clockwise red is on top.
        let input = with_segments(&jpeg(&halves(64, 32)), &exif_app1(6));
        let out = sanitize(&input).unwrap();
        assert_clean(&out);
        assert_eq!((out.width, out.height), (32, 64));
        let decoded = image::load_from_memory(&out.jpeg).unwrap().into_rgb8();
        let top = decoded.get_pixel(16, 8).0;
        let bottom = decoded.get_pixel(16, 56).0;
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
    }

    #[test]
    fn large_photos_shrink_to_the_long_side_and_alpha_becomes_white() {
        let out = sanitize(&png(&RgbaImage::from_pixel(3200, 800, Rgba([0, 0, 0, 0])))).unwrap();
        assert_clean(&out);
        assert_eq!((out.width, out.height), (1600, 400));
        let decoded = image::load_from_memory(&out.jpeg).unwrap().into_rgb8();
        assert!(decoded.get_pixel(800, 200).0.iter().all(|c| *c > 245));
        // An extreme aspect ratio keeps at least one pixel per side.
        let out = sanitize(&png(&RgbaImage::from_pixel(9000, 2, Rgba([1, 2, 3, 255])))).unwrap();
        assert_clean(&out);
        assert_eq!(out.width, 1600);
        assert!(out.height >= 1);
    }

    #[test]
    fn noisy_photos_stay_under_one_mebibyte() {
        let mut seed = 0x2545_f491_u32;
        let noise = RgbaImage::from_fn(1600, 1600, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let [r, g, b, _] = seed.to_le_bytes();
            Rgba([r, g, b, 255])
        });
        match sanitize(&png(&noise)) {
            Ok(out) => assert_clean(&out),
            Err(error) => assert_eq!(error, PhotoError::TooLarge),
        }
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A PNG that only claims a size: valid signature and IHDR, then a
    /// tiny IDAT. A decoder that trusted the header would allocate for it.
    fn png_header(width: u32, height: u32, bit_depth: u8) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = |kind: &[u8], data: &[u8]| {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend(data);
            out.extend(&body);
            out.extend(crc32(&body).to_be_bytes());
        };
        let mut ihdr = width.to_be_bytes().to_vec();
        ihdr.extend(height.to_be_bytes());
        ihdr.extend([bit_depth, 6, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        chunk(b"IEND", &[]);
        out
    }

    #[test]
    fn decompression_bombs_are_refused_from_the_header() {
        for (width, height, depth) in [
            (20_000, 20_000, 8),
            (10_001, 1, 8),
            (7_000, 7_000, 8),
            // Under 40 MP, but 16-bit RGBA needs 280 MB of pixels.
            (7_000, 5_000, 16),
        ] {
            assert_eq!(
                sanitize(&png_header(width, height, depth)).unwrap_err(),
                PhotoError::TooLarge,
                "{width}x{height}x{depth}"
            );
        }
        // The same size in 8 bits passes the limits and fails on its data.
        assert_eq!(
            sanitize(&png_header(7_000, 5_000, 8)).unwrap_err(),
            PhotoError::Invalid
        );
    }

    #[test]
    fn unsupported_truncated_and_polyglot_inputs_are_refused() {
        let heic = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic".to_vec();
        let gif = b"GIF89a\x01\0\x01\0\0\0\0;".to_vec();
        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec();
        for input in [heic, gif, svg, Vec::new(), b"RIFF\0\0\0\0WAVE".to_vec()] {
            assert_eq!(sanitize(&input).unwrap_err(), PhotoError::Unsupported);
        }
        let png = png(&RgbaImage::from_pixel(16, 16, Rgba([1, 2, 3, 255])));
        let jpeg = jpeg(&halves(16, 16));
        for truncated in [&png[..40], &jpeg[..20], &webp(&RgbaImage::new(4, 4))[..16]] {
            assert!(sanitize(truncated).is_err());
        }
        // JPEG magic in front of markup is not a photo.
        let mut fake = vec![0xFF, 0xD8, 0xFF];
        fake.extend(b"<html><script>alert(1)</script></html>");
        assert_eq!(sanitize(&fake).unwrap_err(), PhotoError::Invalid);
        // A real image with a payload appended keeps only the pixels.
        let mut polyglot = png.clone();
        polyglot.extend(b"<html><script>alert(1)</script></html>PK\x03\x04");
        let out = sanitize(&polyglot).unwrap();
        assert_clean(&out);
        assert!(!out.jpeg.windows(8).any(|w| w == b"<script>"));
        assert!(!out.jpeg.windows(4).any(|w| w == b"PK\x03\x04"));
    }

    #[test]
    fn photo_directory_must_be_private_real_and_owned() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("photos");
        assert!(PhotoStore::open(&dir).is_available());
        assert_eq!(
            fs::metadata(&dir).unwrap().mode() & 0o7777,
            0o700,
            "created 0700"
        );
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!PhotoStore::open(&dir).is_available());
        let link = root.path().join("link");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!PhotoStore::open(&link).is_available());
        let file = root.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(!PhotoStore::open(&file).is_available());
    }

    #[test]
    fn files_are_private_and_reads_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("photos");
        prepare_dir(&dir).unwrap();
        let id = Uuid::new_v4();
        let photo = jpeg(&halves(8, 8));
        write_file(&dir, id, &photo).unwrap();
        let path = file_path(&dir, id);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(read_file(&dir, id).unwrap(), photo);
        // Nothing but the photo is left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::write(&path, vec![0xFF; MAX_PHOTO_BYTES + 1]).unwrap();
        assert_eq!(read_file(&dir, id).unwrap_err(), PhotoError::Internal);
        fs::write(&path, b"<html>").unwrap();
        assert_eq!(read_file(&dir, id).unwrap_err(), PhotoError::Internal);
        remove_file(&dir, id).unwrap();
        assert_eq!(read_file(&dir, id).unwrap_err(), PhotoError::NotFound);
        // Removing a missing file is fine.
        remove_file(&dir, id).unwrap();
        // A FIFO in place of a photo neither blocks nor reads.
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo has no other preconditions.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read_file(&dir, id).unwrap_err(), PhotoError::Internal);
        fs::remove_file(&path).unwrap();
        // A symlink in place of a photo is never followed.
        std::os::unix::fs::symlink(root.path().join("elsewhere"), &path).unwrap();
        assert_eq!(read_file(&dir, id).unwrap_err(), PhotoError::Internal);
    }

    #[test]
    fn expiry_is_ninety_days_after_upload_unless_saved() {
        let at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        assert_eq!(
            expires_at(false, false, at).as_deref(),
            Some("2024-02-12T22:13:20Z")
        );
        assert_eq!(expires_at(true, false, at), None);
        // Unsent photos go after a day, kept or not.
        for saved in [false, true] {
            assert_eq!(
                expires_at(saved, true, at).as_deref(),
                Some("2023-11-15T22:13:20Z")
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn retention_removes_expired_and_orphaned_photos_only(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("photos_{}", Uuid::now_v7().simple()))
            .use_db("fixture")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let root = tempfile::tempdir()?;
        let dir = root.path().join("photos");
        let store = PhotoStore::open(&dir);
        assert!(store.is_available());
        let conversation = Uuid::now_v7().to_string();
        let row = |id: Uuid, conversation: Option<&str>, saved: bool, age: &str| {
            let conversation = conversation.map_or("NONE".to_string(), |c| format!("'{c}'"));
            format!("CREATE chat_photos SET id = '{id}', user_id = 'owner', conversation_id = {conversation}, saved = {saved}, width = 8, height = 8, bytes = 100, created_at = time::now() - {age};")
        };
        let old = || SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        let photo = jpeg(&halves(8, 8));
        let make = |id: Uuid, aged: bool| {
            write_file(&dir, id, &photo).unwrap();
            if aged {
                File::options()
                    .write(true)
                    .open(file_path(&dir, id))
                    .unwrap()
                    .set_modified(old())
                    .unwrap();
            }
        };
        let expired = Uuid::new_v4();
        let kept_saved = Uuid::new_v4();
        let recent = Uuid::new_v4();
        let stale_pending = Uuid::new_v4();
        let fresh_pending = Uuid::new_v4();
        let orphan = Uuid::new_v4();
        let young_orphan = Uuid::new_v4();
        let missing_file = Uuid::new_v4();
        let mut setup = String::new();
        setup += &row(expired, Some(&conversation), false, "91d");
        setup += &row(kept_saved, Some(&conversation), true, "400d");
        setup += &row(recent, Some(&conversation), false, "89d");
        setup += &row(stale_pending, None, false, "25h");
        setup += &row(fresh_pending, None, false, "23h");
        setup += &row(missing_file, Some(&conversation), false, "91d");
        db.query(setup).await?.check()?;
        for id in [
            expired,
            kept_saved,
            recent,
            stale_pending,
            fresh_pending,
            orphan,
        ] {
            make(id, true);
        }
        make(young_orphan, false);
        let old_tmp = dir.join(format!("{TMP_PREFIX}{}", Uuid::new_v4()));
        fs::write(&old_tmp, b"partial")?;
        File::options()
            .write(true)
            .open(&old_tmp)?
            .set_modified(old())?;
        let young_tmp = dir.join(format!("{TMP_PREFIX}{}", Uuid::new_v4()));
        fs::write(&young_tmp, b"partial")?;
        let unrelated = dir.join("notes.txt");
        fs::write(&unrelated, b"x")?;
        File::options()
            .write(true)
            .open(&unrelated)?
            .set_modified(old())?;

        let pruned = prune(&db, &store).await?;
        assert_eq!(
            pruned,
            Pruned {
                expired: 3,
                orphans: 2
            }
        );
        let mut left: Vec<String> = db
            .query("SELECT VALUE record::id(id) FROM chat_photos")
            .await?
            .take(0)?;
        left.sort();
        let mut want: Vec<String> = [kept_saved, recent, fresh_pending]
            .iter()
            .map(Uuid::to_string)
            .collect();
        want.sort();
        assert_eq!(left, want);
        for id in [kept_saved, recent, fresh_pending, young_orphan] {
            assert!(file_path(&dir, id).exists());
        }
        for id in [expired, stale_pending, orphan] {
            assert!(!file_path(&dir, id).exists());
        }
        assert!(!old_tmp.exists());
        assert!(young_tmp.exists());
        assert!(unrelated.exists());
        // A second pass finds nothing.
        assert_eq!(prune(&db, &store).await?, Pruned::default());
        Ok(())
    }
}
