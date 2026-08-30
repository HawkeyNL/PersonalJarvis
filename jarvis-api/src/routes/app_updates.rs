//! Authenticated, read-only delivery of already verified application updates.
//!
//! The sync service owns remote credentials, validation and atomic activation.
//! Core only exposes the active generation to enrolled devices. Tauri still
//! verifies the embedded updater signature before installation.

use std::path::{Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, State},
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    release: Release,
    artifacts: Vec<ArtifactEntry>,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactEntry {
    platform: String,
    architecture: String,
    distribution: String,
    artifact: Option<Artifact>,
    external: Option<serde_json::Value>,
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
    released_at: String,
    notes: Option<String>,
    signature: String,
    artifact_path: PathBuf,
    filename: String,
    size: u64,
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
        released_at: manifest.release.released_at,
        notes: manifest.release.notes,
        signature: entry.signature.value,
        artifact_path: canonical_artifact,
        filename,
        size: artifact.size,
    })
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
    Json(json!({
        "schema_version": 1,
        "channel": "stable",
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
) -> Response {
    if channel != "stable" {
        return opaque(StatusCode::NOT_FOUND, "update channel unavailable");
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
    fn updater_targets_are_explicit_and_mobile_is_not_installable() {
        assert_eq!(platform("windows", "x86_64"), Some(("windows", "x86_64")));
        assert_eq!(platform("darwin", "aarch64"), Some(("macos", "arm64")));
        assert_eq!(platform("linux", "x86_64"), Some(("linux", "x86_64")));
        assert_eq!(platform("android", "universal"), None);
        assert_eq!(platform("ios", "arm64"), None);
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
