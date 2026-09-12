//! Signed client release metadata binds desktop/Android updates and a manual
//! iOS installer. The IPA is not Apple-signed and is never an updater target.
//! OCI is transport; the separately pinned updater key is trust.
use super::{digest_hex, verify_bytes, Config, Layer, Manifest, Result, MAX_IPA, MAX_METADATA};
use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read};

#[cfg(test)]
pub(crate) mod tests;

pub const RELEASE_TYPE: &str = "application/vnd.jarvis.client-release.v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseConfig {
    pub identity: Config,
    #[serde(default)]
    pub track_stable: bool,
    pub tauri_signing_public_key: String,
    pub android_signing_certificate_sha256: String,
}

impl ReleaseConfig {
    pub fn validate(&self) -> Result<()> {
        self.identity.validate()?;
        public_key(&self.tauri_signing_public_key)?;
        digest_hex(&format!(
            "sha256:{}",
            self.android_signing_certificate_sha256
        ))?;
        Ok(())
    }

    /// Discovery only. Replacing this identity does NOT approve metadata; the
    /// pinned key must verify it before any bytes can become active.
    pub fn discover(&mut self, bytes: &[u8]) -> Result<()> {
        if !self.track_stable || bytes.len() > MAX_METADATA {
            return Err("stable tracking not approved");
        }
        let m: Manifest = serde_json::from_slice(bytes).map_err(|_| "invalid stable metadata")?;
        let version = m
            .annotations
            .get("org.opencontainers.image.version")
            .ok_or("stable version missing")?;
        let floor = super::super::stable_version(&self.identity.version)
            .map_err(|_| "invalid release floor")?;
        let next = super::super::stable_version(version).map_err(|_| "invalid stable version")?;
        if next < floor {
            return Err("stable release is older than approved minimum");
        }
        self.identity.version = version.clone();
        self.identity.source_revision = m
            .annotations
            .get("org.opencontainers.image.revision")
            .ok_or("stable revision missing")?
            .clone();
        use sha2::{Digest, Sha256};
        self.identity.manifest_digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        self.identity.validate()
    }
}

fn decoded(value: &str) -> Result<String> {
    if value.len() > 16384 {
        return Err("signature document exceeds limit");
    }
    String::from_utf8(
        STANDARD
            .decode(value.trim())
            .map_err(|_| "invalid signature encoding")?,
    )
    .map_err(|_| "invalid signature document")
}

fn public_key(value: &str) -> Result<PublicKey> {
    PublicKey::decode(&decoded(value)?).map_err(|_| "invalid pinned updater key")
}

/// Prehashed Minisign verification is streaming: no multi-GB allocation. Tauri
/// releases use this format; legacy non-prehashed signatures fail closed.
pub fn verify_signature(mut input: impl Read, signature: &str, key: &str) -> Result<()> {
    let key = public_key(key)?;
    let signature =
        Signature::decode(&decoded(signature)?).map_err(|_| "invalid updater signature")?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|_| "unsupported updater signature")?;
    let mut buffer = [0; 65536];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| "cannot read signed artifact")?;
        if n == 0 {
            break;
        }
        verifier.update(&buffer[..n]);
    }
    verifier
        .finalize()
        .map_err(|_| "updater signature verification failed")
}

pub fn parse_oci(bytes: &[u8], config: &Config) -> Result<BTreeMap<String, Layer>> {
    config.validate()?;
    if bytes.len() > MAX_METADATA {
        return Err("manifest exceeds limit");
    }
    verify_bytes(bytes, &config.manifest_digest)?;
    let m: Manifest = serde_json::from_slice(bytes).map_err(|_| "invalid release OCI manifest")?;
    if m.schema_version != 2
        || m.media_type != "application/vnd.oci.image.manifest.v1+json"
        || m.artifact_type != RELEASE_TYPE
        || m.layers.len() != 12
        || m.annotations.get("org.opencontainers.image.revision") != Some(&config.source_revision)
        || m.annotations
            .get("org.opencontainers.image.source")
            .map(String::as_str)
            != Some("https://github.com/HawkeyNL/PersonalJarvisApp")
    {
        return Err("complete client release identity mismatch");
    }
    let mut layers = BTreeMap::new();
    let expected = expected_names(&config.version);
    for layer in m.layers {
        digest_hex(&layer.digest)?;
        let name = layer
            .annotations
            .get("org.opencontainers.image.title")
            .ok_or("missing asset name")?
            .clone();
        let limit = if name == "latest.json" {
            MAX_METADATA as u64
        } else if name.ends_with(".sig") {
            16384
        } else {
            MAX_IPA
        };
        let media = if name == "latest.json" {
            "application/json"
        } else {
            "application/octet-stream"
        };
        if !expected.contains(&name)
            || layer.annotations.len() != 1
            || layer.size == 0
            || layer.size > limit
            || layer.media_type != media
            || layers.insert(name, layer).is_some()
        {
            return Err("unexpected, unsafe or duplicate release layer");
        }
    }
    if layers.len() != expected.len() {
        return Err("incomplete release layers");
    }
    Ok(layers)
}

pub fn expected_names(version: &str) -> Vec<String> {
    let mut names = vec!["latest.json".into(), "latest.json.sig".into()];
    for (target, suffix) in [
        ("linux_x86_64", ".AppImage"),
        ("windows_x86_64", ".exe"),
        ("macos_arm64", ".app.tar.gz"),
    ] {
        let name = format!("Jarvis_{version}_{target}{suffix}");
        names.push(format!("{name}.sig"));
        names.push(name);
    }
    for (target, suffix) in [
        ("macos_arm64", ".dmg"),
        ("android_universal", ".apk"),
        ("android_universal", ".aab"),
        ("ios_arm64", "_unsigned.ipa"),
    ] {
        names.push(format!("Jarvis_{version}_{target}{suffix}"));
    }
    names
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    schema_version: u32,
    pub release: Identity,
    pub artifacts: Vec<Entry>,
    pub installers: Vec<Installer>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub version: String,
    tag: String,
    source_revision: String,
    product: String,
    channel: String,
    released_at: String,
    client_protocol: u32,
    minimum_client_protocol: u32,
    #[serde(default)]
    notes: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub platform: String,
    pub architecture: String,
    distribution: String,
    pub artifact: Artifact,
    pub signature: Signing,
    #[serde(default)]
    pub metadata: Option<AndroidMetadata>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidMetadata {
    pub version_code: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    scheme: String,
    pub value: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installer {
    pub platform: String,
    pub architecture: String,
    distribution: String,
    pub artifact: Artifact,
}

impl Document {
    pub fn files(&self) -> impl Iterator<Item = (&str, &str, &Artifact)> {
        self.artifacts
            .iter()
            .map(|e| (e.platform.as_str(), e.architecture.as_str(), &e.artifact))
            .chain(
                self.installers
                    .iter()
                    .map(|e| (e.platform.as_str(), e.architecture.as_str(), &e.artifact)),
            )
    }
    pub fn android_code(&self) -> u32 {
        self.artifacts
            .iter()
            .find_map(|e| e.metadata.as_ref().map(|m| m.version_code))
            .unwrap_or(0)
    }
}

/// Parse only after authenticating the exact upstream metadata bytes.
pub fn validate_document(
    bytes: &[u8],
    signature: &str,
    config: &ReleaseConfig,
    layers: &BTreeMap<String, Layer>,
) -> Result<Document> {
    config.validate()?;
    if bytes.len() > MAX_METADATA {
        return Err("release metadata exceeds limit");
    }
    verify_signature(bytes, signature, &config.tauri_signing_public_key)?;
    let doc: Document =
        serde_json::from_slice(bytes).map_err(|_| "invalid signed release metadata")?;
    let r = &doc.release;
    if doc.schema_version != 1
        || r.version != config.identity.version
        || r.tag != format!("app-v{}", r.version)
        || r.source_revision != config.identity.source_revision
        || r.product != "clients"
        || r.channel != "stable"
        || !(1..=65535).contains(&r.client_protocol)
        || !(1..=r.client_protocol).contains(&r.minimum_client_protocol)
        || r.notes
            .as_ref()
            .is_some_and(|s| s.len() > 8192 || s.contains('\0'))
        || chrono::DateTime::parse_from_rfc3339(&r.released_at).is_err()
        || doc.artifacts.len() != 4
        || doc.installers.len() != 3
    {
        return Err("signed client release identity or matrix mismatch");
    }
    let mut seen = std::collections::BTreeSet::new();
    for e in &doc.artifacts {
        let (suffix, distribution, scheme) = match (e.platform.as_str(), e.architecture.as_str()) {
            ("linux", "x86_64") => (".AppImage", "home-node-updater", "tauri-minisign"),
            ("windows", "x86_64") => (".exe", "home-node-updater", "tauri-minisign"),
            ("macos", "arm64") => (".app.tar.gz", "home-node-updater", "tauri-minisign"),
            ("android", "universal") => (
                ".apk",
                "home-node-apk",
                "android-apk-signing-certificate-sha256",
            ),
            _ => return Err("unsupported update target"),
        };
        if !seen.insert(&e.platform)
            || e.distribution != distribution
            || e.signature.scheme != scheme
        {
            return Err("duplicate target or invalid signing policy");
        }
        if e.platform == "android" {
            if e.signature.value != config.android_signing_certificate_sha256
                || !e
                    .metadata
                    .as_ref()
                    .is_some_and(|m| (1..=2_100_000_000).contains(&m.version_code))
            {
                return Err("Android signer or version code mismatch");
            }
        } else if e.metadata.is_some() || Signature::decode(&decoded(&e.signature.value)?).is_err()
        {
            return Err("invalid desktop signature metadata");
        }
        validate_asset(
            &e.artifact,
            &r.version,
            &e.platform,
            &e.architecture,
            suffix,
            layers,
        )?;
    }
    seen.clear();
    for e in &doc.installers {
        let (suffix, distribution) = match (e.platform.as_str(), e.architecture.as_str()) {
            ("macos", "arm64") => (".dmg", "home-node-installer"),
            ("android", "universal") => (".aab", "app-store-bundle"),
            ("ios", "arm64") => ("_unsigned.ipa", "manual-owner-signing"),
            _ => return Err("unsupported installer target"),
        };
        if !seen.insert(&e.platform) || e.distribution != distribution {
            return Err("invalid installer matrix");
        }
        validate_asset(
            &e.artifact,
            &r.version,
            &e.platform,
            &e.architecture,
            suffix,
            layers,
        )?;
    }
    Ok(doc)
}

fn validate_asset(
    a: &Artifact,
    version: &str,
    platform: &str,
    architecture: &str,
    suffix: &str,
    layers: &BTreeMap<String, Layer>,
) -> Result<()> {
    let name = format!("Jarvis_{version}_{platform}_{architecture}{suffix}");
    let expected = format!("releases/v{version}/{platform}-{architecture}/{name}");
    let layer = layers.get(&name).ok_or("missing release asset")?;
    if a.path != expected || a.size != layer.size || a.sha256 != digest_hex(&layer.digest)? {
        return Err("signed asset does not match OCI transport");
    }
    Ok(())
}
