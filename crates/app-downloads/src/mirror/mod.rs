//! Digest-pinned, manually approved iOS candidates. Not automatic app updates.
mod apk;
mod registry;
pub mod release;
pub mod release_store;
mod store;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub use registry::Registry;
pub use store::{read_protected, validate_directory, Store};

pub type Result<T> = std::result::Result<T, &'static str>;
pub const MAX_METADATA: usize = 1024 * 1024;
pub const MAX_IPA: u64 = 2 * 1024 * 1024 * 1024;
pub const REPOSITORY: &str = "hawkeynl/jarvis-client-artifacts";
pub const ARTIFACT_TYPE: &str = "application/vnd.jarvis.ios-sideload-candidate.v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub github_username: String,
    pub version: String,
    pub source_revision: String,
    pub manifest_digest: String,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.github_username.is_empty()
            || self.github_username.len() > 39
            || !self
                .github_username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.source_revision.len() != 40
            || !self.source_revision.bytes().all(lower_hex)
        {
            return Err("invalid mirror configuration");
        }
        super::stable_version(&self.version).map_err(|_| "invalid candidate version")?;
        digest_hex(&self.manifest_digest)?;
        Ok(())
    }

    pub fn filename(&self) -> String {
        format!("Jarvis_{}_ios_arm64_unsigned.ipa", self.version)
    }
}

fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

pub fn digest_hex(value: &str) -> Result<&str> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or("SHA-256 digest required")?;
    if hex.len() != 64 || !hex.bytes().all(lower_hex) {
        return Err("invalid SHA-256 digest");
    }
    Ok(hex)
}

pub fn verify_bytes(bytes: &[u8], digest: &str) -> Result<()> {
    if hex::encode(Sha256::digest(bytes)) != digest_hex(digest)? {
        return Err("artifact checksum mismatch");
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    media_type: String,
    artifact_type: String,
    layers: Vec<Layer>,
    annotations: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub digest: String,
    pub size: u64,
    media_type: String,
    annotations: BTreeMap<String, String>,
}

pub fn parse_manifest(bytes: &[u8], config: &Config) -> Result<(Layer, Layer)> {
    config.validate()?;
    if bytes.len() > MAX_METADATA {
        return Err("manifest exceeds limit");
    }
    verify_bytes(bytes, &config.manifest_digest)?;
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|_| "invalid OCI manifest")?;
    if manifest.schema_version != 2
        || manifest.media_type != "application/vnd.oci.image.manifest.v1+json"
        || manifest.artifact_type != ARTIFACT_TYPE
        || manifest.layers.len() != 2
        || manifest
            .annotations
            .get("org.opencontainers.image.revision")
            != Some(&config.source_revision)
        || manifest
            .annotations
            .get("org.opencontainers.image.source")
            .map(String::as_str)
            != Some("https://github.com/HawkeyNL/PersonalJarvisApp")
    {
        return Err("OCI candidate identity mismatch");
    }
    let mut ipa = None;
    let mut descriptor = None;
    for layer in manifest.layers {
        digest_hex(&layer.digest)?;
        if layer.annotations.len() != 1 {
            return Err("unexpected layer annotations");
        }
        let name = layer
            .annotations
            .get("org.opencontainers.image.title")
            .ok_or("missing layer name")?;
        if name == &config.filename()
            && layer.media_type == "application/octet-stream"
            && layer.size > 0
            && layer.size <= MAX_IPA
            && ipa.is_none()
        {
            ipa = Some(layer);
        } else if name == "ios-candidate.json"
            && layer.media_type == "application/json"
            && layer.size > 0
            && layer.size <= MAX_METADATA as u64
            && descriptor.is_none()
        {
            descriptor = Some(layer);
        } else {
            return Err("unexpected, duplicate or oversized candidate layer");
        }
    }
    Ok((
        ipa.ok_or("missing IPA")?,
        descriptor.ok_or("missing descriptor")?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    schema_version: u32,
    version: String,
    source_revision: String,
    platform: String,
    architecture: String,
    build_number: u32,
    distribution: String,
    apple_signed: bool,
    automatic_install: bool,
    artifact: Artifact,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    name: String,
    size: u64,
    sha256: String,
}

pub fn validate_candidate(bytes: &[u8], layer: &Layer, ipa: &Layer, config: &Config) -> Result<()> {
    if bytes.len() as u64 != layer.size || bytes.len() > MAX_METADATA {
        return Err("candidate descriptor size mismatch");
    }
    verify_bytes(bytes, &layer.digest)?;
    let c: Candidate = serde_json::from_slice(bytes).map_err(|_| "invalid candidate descriptor")?;
    if c.schema_version != 1
        || c.version != config.version
        || c.source_revision != config.source_revision
        || c.platform != "ios"
        || c.architecture != "arm64"
        || !(1..=999_999_999).contains(&c.build_number)
        || c.distribution != "manual-owner-signing"
        || c.apple_signed
        || c.automatic_install
        || c.artifact.name != config.filename()
        || c.artifact.size != ipa.size
        || c.artifact.sha256 != digest_hex(&ipa.digest)?
    {
        return Err("candidate identity or installation policy mismatch");
    }
    Ok(())
}
