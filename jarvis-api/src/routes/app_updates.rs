//! Authenticated, read-only delivery of already verified application updates.
//!
//! The sync service owns remote credentials, validation and atomic activation.
//! Core only exposes the active generation to enrolled devices. Tauri still
//! verifies the embedded updater signature before installation.

use std::path::{Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use semver::Version;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

use crate::{AppState, AppUpdateMirror, Authed};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ANDROID_VERSION_CODE: u32 = 2_100_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    release: Release,
    artifacts: Vec<ArtifactEntry>,
    // Supplemental installers are checksum-bound by the signed upstream
    // manifest, validated/mirrored by sync, and never exposed as updater targets.
    #[serde(default, rename = "installers")]
    _installers: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    version: String,
    channel: String,
    released_at: String,
    minimum_client_protocol: u32,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    source_revision: Option<String>,
    #[serde(default)]
    client_protocol: Option<u32>,
}

impl Release {
    fn identity_valid(&self) -> bool {
        match self.product.as_deref() {
            None => {
                self.tag.is_none()
                    && self.source_revision.is_none()
                    && self.client_protocol.is_none()
            }
            Some("desktop" | "mobile") => {
                self.tag.as_deref() == Some(format!("app-v{}", self.version).as_str())
                    && self.source_revision.as_ref().is_some_and(|revision| {
                        revision.len() == 40
                            && revision
                                .bytes()
                                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    })
                    && self.client_protocol.is_some_and(|value| {
                        (1..=65535).contains(&value) && self.minimum_client_protocol <= value
                    })
            }
            _ => false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactEntry {
    platform: String,
    architecture: String,
    distribution: String,
    artifact: Option<Artifact>,
    external: Option<serde_json::Value>,
    metadata: Option<serde_json::Value>,
    signature: Signature,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    sha256: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signature {
    scheme: String,
    value: String,
}

struct SelectedRelease {
    version: Version,
    minimum_client_protocol: u32,
    released_at: String,
    notes: Option<String>,
    signature: String,
    artifact_path: PathBuf,
    filename: String,
    size: u64,
}

struct SelectedAndroidRelease {
    version: Version,
    version_code: u32,
    minimum_client_protocol: u32,
    released_at: String,
    notes: Option<String>,
    sha256: String,
    signing_certificate_sha256: String,
    artifact_path: PathBuf,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AndroidUpdateQuery {
    client_protocol: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesktopUpdateQuery {
    client_protocol: u32,
}

#[derive(Debug, Eq, PartialEq)]
enum AndroidOffer {
    Available,
    Current,
    Incompatible,
}

fn protocol_is_compatible(client_protocol: u32, minimum_client_protocol: u32) -> bool {
    client_protocol > 0 && minimum_client_protocol > 0 && minimum_client_protocol <= client_protocol
}

fn android_offer(
    installed_version_code: u32,
    client_protocol: u32,
    release_version_code: u32,
    minimum_client_protocol: u32,
) -> AndroidOffer {
    if !protocol_is_compatible(client_protocol, minimum_client_protocol) {
        AndroidOffer::Incompatible
    } else if release_version_code <= installed_version_code {
        AndroidOffer::Current
    } else {
        AndroidOffer::Available
    }
}

fn opaque(status: StatusCode, message: &'static str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn platform(target: &str, arch: &str) -> Option<(&'static str, &'static str)> {
    match (target, arch) {
        ("windows", "x86_64") => Some(("windows", "x86_64")),
        ("darwin", "aarch64") => Some(("macos", "arm64")),
        ("linux", "x86_64") => Some(("linux", "x86_64")),
        _ => None,
    }
}

fn canonical_filename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

async fn active_release(
    mirror: &AppUpdateMirror,
    channel: &str,
    target: &str,
    arch: &str,
) -> Result<SelectedRelease, Response> {
    let (platform, architecture) = platform(target, arch)
        .ok_or_else(|| opaque(StatusCode::NOT_FOUND, "unsupported update target"))?;
    let releases = tokio::fs::canonicalize(mirror.root().join("releases"))
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    let current = tokio::fs::canonicalize(mirror.root().join("current"))
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    if !current.starts_with(&releases) || current.parent() != Some(releases.as_path()) {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror unavailable",
        ));
    }
    let manifest_path = current.join("manifest.json");
    let metadata = tokio::fs::metadata(&manifest_path)
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    if !metadata.is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror unavailable",
        ));
    }
    let encoded = tokio::fs::read(&manifest_path)
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    let manifest: Manifest = serde_json::from_slice(&encoded)
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    if manifest.schema_version != 1
        || manifest.release.channel != channel
        || manifest.release.minimum_client_protocol == 0
        || !manifest.release.identity_valid()
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let version = Version::parse(&manifest.release.version)
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    let entry = manifest
        .artifacts
        .into_iter()
        .find(|entry| entry.platform == platform && entry.architecture == architecture)
        .ok_or_else(|| opaque(StatusCode::NOT_FOUND, "update target unavailable"))?;
    if entry.distribution != "home-node-updater"
        || entry.external.is_some()
        || entry.metadata.is_some()
        || entry.signature.scheme != "tauri-minisign"
        || entry.signature.value.is_empty()
        || entry.signature.value.len() > 16 * 1024
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact = entry
        .artifact
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    let filename = FsPath::new(&artifact.path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| canonical_filename(value))
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?
        .to_string();
    if artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || artifact.size == 0
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact_path = current
        .join(format!("{platform}-{architecture}"))
        .join(&filename);
    let canonical_artifact = tokio::fs::canonicalize(&artifact_path).await.map_err(|_| {
        opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update artifact unavailable",
        )
    })?;
    if !canonical_artifact.starts_with(&current) {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact_metadata = tokio::fs::metadata(&canonical_artifact)
        .await
        .map_err(|_| {
            opaque(
                StatusCode::SERVICE_UNAVAILABLE,
                "update artifact unavailable",
            )
        })?;
    if !artifact_metadata.is_file() || artifact_metadata.len() != artifact.size {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update artifact unavailable",
        ));
    }
    Ok(SelectedRelease {
        version,
        minimum_client_protocol: manifest.release.minimum_client_protocol,
        released_at: manifest.release.released_at,
        notes: manifest.release.notes,
        signature: entry.signature.value,
        artifact_path: canonical_artifact,
        filename,
        size: artifact.size,
    })
}

async fn active_android_release(
    mirror: &AppUpdateMirror,
    channel: &str,
) -> Result<SelectedAndroidRelease, Response> {
    let releases = tokio::fs::canonicalize(mirror.root().join("releases"))
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    let current = tokio::fs::canonicalize(mirror.root().join("current"))
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    if !current.starts_with(&releases) || current.parent() != Some(releases.as_path()) {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror unavailable",
        ));
    }
    let manifest_path = current.join("manifest.json");
    let manifest_metadata = tokio::fs::metadata(&manifest_path)
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    if !manifest_metadata.is_file() || manifest_metadata.len() > MAX_MANIFEST_BYTES {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror unavailable",
        ));
    }
    let encoded = tokio::fs::read(&manifest_path)
        .await
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror unavailable"))?;
    let manifest: Manifest = serde_json::from_slice(&encoded)
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    if manifest.schema_version != 1
        || manifest.release.channel != channel
        || manifest.release.minimum_client_protocol == 0
        || !manifest.release.identity_valid()
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let version = Version::parse(&manifest.release.version)
        .map_err(|_| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    let entry = manifest
        .artifacts
        .into_iter()
        .find(|entry| entry.platform == "android" && entry.architecture == "universal")
        .ok_or_else(|| opaque(StatusCode::NOT_FOUND, "Android update unavailable"))?;
    let metadata = entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.as_object())
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    if metadata.len() != 1 {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let version_code = metadata
        .get("version_code")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| (1..=MAX_ANDROID_VERSION_CODE).contains(value))
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    if entry.distribution != "home-node-apk"
        || entry.external.is_some()
        || entry.signature.scheme != "android-apk-signing-certificate-sha256"
        || entry.signature.value.len() != 64
        || !entry
            .signature
            .value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact = entry
        .artifact
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    let filename = FsPath::new(&artifact.path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| canonical_filename(value) && value.ends_with(".apk"))
        .ok_or_else(|| opaque(StatusCode::SERVICE_UNAVAILABLE, "update mirror invalid"))?;
    if artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || artifact.size == 0
    {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact_path = current.join("android-universal").join(filename);
    let canonical_artifact = tokio::fs::canonicalize(&artifact_path).await.map_err(|_| {
        opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update artifact unavailable",
        )
    })?;
    if !canonical_artifact.starts_with(&current) {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update mirror invalid",
        ));
    }
    let artifact_metadata = tokio::fs::metadata(&canonical_artifact)
        .await
        .map_err(|_| {
            opaque(
                StatusCode::SERVICE_UNAVAILABLE,
                "update artifact unavailable",
            )
        })?;
    if !artifact_metadata.is_file() || artifact_metadata.len() != artifact.size {
        return Err(opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "update artifact unavailable",
        ));
    }
    Ok(SelectedAndroidRelease {
        version,
        version_code,
        minimum_client_protocol: manifest.release.minimum_client_protocol,
        released_at: manifest.release.released_at,
        notes: manifest.release.notes,
        sha256: artifact.sha256.to_ascii_lowercase(),
        signing_certificate_sha256: entry.signature.value.to_ascii_lowercase(),
        artifact_path: canonical_artifact,
        size: artifact.size,
    })
}

pub(crate) async fn android_update_check(
    _authed: Authed,
    State(state): State<AppState>,
    Path(current_version_code): Path<u32>,
    Query(query): Query<AndroidUpdateQuery>,
) -> Response {
    if current_version_code == 0 || query.client_protocol == 0 {
        return opaque(StatusCode::BAD_REQUEST, "invalid Android client metadata");
    }
    let Some(mirror) = state.app_update_mirror else {
        return opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "application updates unavailable",
        );
    };
    let mirror = mirror.for_mobile();
    let selected = match active_android_release(&mirror, "stable").await {
        Ok(selected) => selected,
        Err(response) => return response,
    };
    match android_offer(
        current_version_code,
        query.client_protocol,
        selected.version_code,
        selected.minimum_client_protocol,
    ) {
        AndroidOffer::Incompatible => {
            return opaque(
                StatusCode::CONFLICT,
                "Android client protocol is incompatible with this release",
            )
        }
        AndroidOffer::Current => return StatusCode::NO_CONTENT.into_response(),
        AndroidOffer::Available => {}
    }
    Json(json!({
        "schema_version": 1,
        "platform": "android",
        "package_name": "com.hawkeynl.jarvis",
        "version_code": selected.version_code,
        "version_name": selected.version.to_string(),
        "minimum_client_protocol": selected.minimum_client_protocol,
        "released_at": selected.released_at,
        "notes": selected.notes.unwrap_or_default(),
        "artifact": {
            "size": selected.size,
            "sha256": selected.sha256,
            "signing_certificate_sha256": selected.signing_certificate_sha256,
        },
        "download_url": format!(
            "{}/v1/app-updates/android/download",
            mirror.public_base_url()
        ),
    }))
    .into_response()
}

pub(crate) async fn android_update_download(
    _authed: Authed,
    State(state): State<AppState>,
) -> Response {
    let Some(mirror) = state.app_update_mirror else {
        return opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "application updates unavailable",
        );
    };
    let mirror = mirror.for_mobile();
    let selected = match active_android_release(&mirror, "stable").await {
        Ok(selected) => selected,
        Err(response) => return response,
    };
    let file = match tokio::fs::File::open(selected.artifact_path).await {
        Ok(file) => file,
        Err(_) => {
            return opaque(
                StatusCode::SERVICE_UNAVAILABLE,
                "update artifact unavailable",
            )
        }
    };
    let stream = ReaderStream::new(file.take(selected.size));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.android.package-archive"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(length) = HeaderValue::from_str(&selected.size.to_string()) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    response
}

pub(crate) async fn app_update_capability(
    _authed: Authed,
    State(state): State<AppState>,
) -> Response {
    let Some(mirror) = state.app_update_mirror else {
        return opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "application updates unavailable",
        );
    };
    // The capability is release-aware: clients must know whether the active
    // manifest requires a newer updater protocol before receiving an offer.
    // A complete mirrored release always contains this mandatory target.
    let selected = match active_release(&mirror, "stable", "windows", "x86_64").await {
        Ok(selected) => selected,
        Err(response) => return response,
    };
    Json(json!({
        "schema_version": 1,
        "channel": "stable",
        "minimum_client_protocol": selected.minimum_client_protocol,
        "check_endpoint": format!(
            "{}/v1/app-updates/stable/{{{{target}}}}/{{{{arch}}}}/{{{{current_version}}}}",
            mirror.public_base_url()
        )
    }))
    .into_response()
}

pub(crate) async fn app_update_check(
    _authed: Authed,
    State(state): State<AppState>,
    Path((channel, target, arch, current_version)): Path<(String, String, String, String)>,
    Query(query): Query<DesktopUpdateQuery>,
) -> Response {
    if channel != "stable" {
        return opaque(StatusCode::NOT_FOUND, "update channel unavailable");
    }
    if query.client_protocol == 0 {
        return opaque(StatusCode::BAD_REQUEST, "invalid desktop client protocol");
    }
    let current = match Version::parse(current_version.trim_start_matches('v')) {
        Ok(version) => version,
        Err(_) => return opaque(StatusCode::BAD_REQUEST, "invalid client version"),
    };
    let Some(mirror) = state.app_update_mirror else {
        return opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "application updates unavailable",
        );
    };
    let selected = match active_release(&mirror, &channel, &target, &arch).await {
        Ok(selected) => selected,
        Err(response) => return response,
    };
    // Re-check the protocol on the same active generation that supplies the
    // update. This closes the capability/check race when mirror activation
    // occurs between the two requests.
    if !protocol_is_compatible(query.client_protocol, selected.minimum_client_protocol) {
        return opaque(
            StatusCode::CONFLICT,
            "desktop client protocol is incompatible with this release",
        );
    }
    if selected.version <= current {
        return StatusCode::NO_CONTENT.into_response();
    }
    Json(json!({
        "version": selected.version.to_string(),
        "pub_date": selected.released_at,
        "notes": selected.notes.unwrap_or_default(),
        "url": format!(
            "{}/v1/app-updates/artifacts/{}/{}/{}/{}",
            mirror.public_base_url(), selected.version, target, arch, selected.filename
        ),
        "signature": selected.signature,
    }))
    .into_response()
}

pub(crate) async fn app_update_artifact(
    _authed: Authed,
    State(state): State<AppState>,
    Path((version, target, arch, filename)): Path<(String, String, String, String)>,
) -> Response {
    if !canonical_filename(&filename) {
        return opaque(StatusCode::BAD_REQUEST, "invalid artifact identifier");
    }
    let requested_version = match Version::parse(version.trim_start_matches('v')) {
        Ok(version) => version,
        Err(_) => return opaque(StatusCode::BAD_REQUEST, "invalid release version"),
    };
    let Some(mirror) = state.app_update_mirror else {
        return opaque(
            StatusCode::SERVICE_UNAVAILABLE,
            "application updates unavailable",
        );
    };
    let selected = match active_release(&mirror, "stable", &target, &arch).await {
        Ok(selected) => selected,
        Err(response) => return response,
    };
    if selected.version != requested_version || selected.filename != filename {
        return opaque(StatusCode::NOT_FOUND, "update artifact unavailable");
    }
    let file = match tokio::fs::File::open(selected.artifact_path).await {
        Ok(file) => file,
        Err(_) => {
            return opaque(
                StatusCode::SERVICE_UNAVAILABLE,
                "update artifact unavailable",
            )
        }
    };
    let stream = ReaderStream::new(file.take(selected.size));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(length) = HeaderValue::from_str(&selected.size.to_string()) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_release_identity_is_independent_and_fail_closed() {
        let legacy = serde_json::json!({
            "version": "0.1.0", "channel": "stable",
            "released_at": "2026-09-01T12:00:00Z", "minimum_client_protocol": 1
        });
        assert!(serde_json::from_value::<Release>(legacy.clone())
            .unwrap()
            .identity_valid());
        let mut desktop = legacy.clone();
        desktop["product"] = "desktop".into();
        desktop["tag"] = "app-v0.1.0".into();
        desktop["source_revision"] = "a".repeat(40).into();
        desktop["client_protocol"] = 1.into();
        assert!(serde_json::from_value::<Release>(desktop.clone())
            .unwrap()
            .identity_valid());
        for (field, value) in [
            ("tag", serde_json::json!("v0.1.0")),
            ("source_revision", serde_json::json!("main")),
            ("product", serde_json::json!("unknown")),
            ("client_protocol", serde_json::json!(0)),
            ("minimum_client_protocol", serde_json::json!(2)),
        ] {
            let mut invalid = desktop.clone();
            invalid[field] = value;
            assert!(
                !serde_json::from_value::<Release>(invalid)
                    .unwrap()
                    .identity_valid(),
                "{field}"
            );
        }
        let mut incomplete = legacy;
        incomplete["source_revision"] = "a".repeat(40).into();
        assert!(!serde_json::from_value::<Release>(incomplete)
            .unwrap()
            .identity_valid());
    }

    #[test]
    fn updater_targets_are_explicit_and_mobile_is_not_installable() {
        assert_eq!(platform("windows", "x86_64"), Some(("windows", "x86_64")));
        assert_eq!(platform("darwin", "aarch64"), Some(("macos", "arm64")));
        assert_eq!(platform("linux", "x86_64"), Some(("linux", "x86_64")));
        assert_eq!(platform("android", "universal"), None);
        assert_eq!(platform("ios", "arm64"), None);
    }

    #[test]
    fn android_metadata_never_routes_through_tauri_target_selection() {
        assert_eq!(platform("android", "universal"), None);
        assert!(canonical_filename("Jarvis_1.2.3_android_universal.apk"));
    }

    #[test]
    fn android_update_requires_newer_version_code_and_compatible_protocol() {
        assert_eq!(android_offer(41, 1, 42, 1), AndroidOffer::Available);
        assert_eq!(android_offer(42, 1, 42, 1), AndroidOffer::Current);
        assert_eq!(android_offer(43, 1, 42, 1), AndroidOffer::Current);
        assert_eq!(android_offer(41, 1, 42, 2), AndroidOffer::Incompatible);
    }

    #[test]
    fn desktop_protocol_compatibility_is_explicit() {
        assert!(protocol_is_compatible(1, 1));
        assert!(protocol_is_compatible(2, 1));
        assert!(!protocol_is_compatible(1, 2));
        assert!(!protocol_is_compatible(0, 1));
        assert!(!protocol_is_compatible(1, 0));
    }

    #[test]
    fn artifact_identifier_cannot_traverse() {
        assert!(canonical_filename(
            "Jarvis_1.2.3_linux_x86_64.AppImage.tar.gz"
        ));
        assert!(!canonical_filename("../release"));
        assert!(!canonical_filename("folder/release"));
        assert!(!canonical_filename("release\\evil"));
    }
}
