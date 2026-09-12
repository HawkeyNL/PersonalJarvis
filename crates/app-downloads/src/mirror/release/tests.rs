use super::*;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sha2::Sha256;

// Deliberately synthetic, test-only signing material. Never production keys.
fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[42; 32])
}
pub fn key() -> String {
    let mut bytes = b"Edfixture1".to_vec();
    bytes.extend_from_slice(signing_key().verifying_key().as_bytes());
    STANDARD.encode(format!(
        "untrusted comment: test-only\n{}\n",
        STANDARD.encode(bytes)
    ))
}
pub fn sign(bytes: &[u8]) -> String {
    let signature = signing_key().sign(&Blake2b512::digest(bytes)).to_bytes();
    let mut document = b"EDfixture1".to_vec();
    document.extend_from_slice(&signature);
    let mut global = signature.to_vec();
    global.extend_from_slice(b"test-only");
    STANDARD.encode(format!(
        "untrusted comment: test-only\n{}\ntrusted comment: test-only\n{}\n",
        STANDARD.encode(document),
        STANDARD.encode(signing_key().sign(&global).to_bytes())
    ))
}

pub struct Fixture {
    pub config: ReleaseConfig,
    pub files: BTreeMap<String, Vec<u8>>,
    pub oci: Vec<u8>,
}
impl Fixture {
    pub fn new(version: &str, code: u32) -> Self {
        let mut files = BTreeMap::new();
        let mut entries = Vec::new();
        let mut installers = Vec::new();
        for (platform, arch, suffix) in [
            ("linux", "x86_64", ".AppImage"),
            ("windows", "x86_64", ".exe"),
            ("macos", "arm64", ".app.tar.gz"),
            ("android", "universal", ".apk"),
            ("macos", "arm64", ".dmg"),
            ("android", "universal", ".aab"),
            ("ios", "arm64", "_unsigned.ipa"),
        ] {
            let name = format!("Jarvis_{version}_{platform}_{arch}{suffix}");
            let bytes = format!("fixture-not-installable:{name}").into_bytes();
            let artifact = json!({"path": format!("releases/v{version}/{platform}-{arch}/{name}"), "size": bytes.len(), "sha256": hex::encode(Sha256::digest(&bytes))});
            let mut entry =
                json!({"platform": platform, "architecture": arch, "artifact": artifact});
            if suffix == ".dmg" || suffix == ".aab" || platform == "ios" {
                entry["distribution"] = json!(if suffix == ".dmg" {
                    "home-node-installer"
                } else if platform == "ios" {
                    "manual-owner-signing"
                } else {
                    "app-store-bundle"
                });
                installers.push(entry);
            } else {
                entry["distribution"] = json!(if platform == "android" {
                    "home-node-apk"
                } else {
                    "home-node-updater"
                });
                if platform == "android" {
                    entry["signature"] = json!({"scheme": "android-apk-signing-certificate-sha256", "value": "a".repeat(64)});
                    entry["metadata"] = json!({"version_code": code});
                } else {
                    let sig = sign(&bytes);
                    entry["signature"] = json!({"scheme": "tauri-minisign", "value": sig});
                    files.insert(format!("{name}.sig"), sig.into_bytes());
                }
                entries.push(entry);
            }
            files.insert(name, bytes);
        }
        let doc = json!({"schema_version": 1, "release": {"version": version, "tag": format!("app-v{version}"), "source_revision": "b".repeat(40), "product": "clients", "channel": "stable", "released_at": "2026-01-01T00:00:00Z", "client_protocol": 1, "minimum_client_protocol": 1}, "artifacts": entries, "installers": installers});
        let bytes = serde_json::to_vec(&doc).unwrap();
        files.insert("latest.json.sig".into(), sign(&bytes).into_bytes());
        files.insert("latest.json".into(), bytes);
        let mut fixture = Self {
            config: ReleaseConfig {
                identity: Config {
                    schema_version: 1,
                    github_username: "fixture".into(),
                    version: version.into(),
                    source_revision: "b".repeat(40),
                    manifest_digest: String::new(),
                },
                track_stable: false,
                tauri_signing_public_key: key(),
                android_signing_certificate_sha256: "a".repeat(64),
            },
            files,
            oci: Vec::new(),
        };
        fixture.rebuild_oci();
        fixture
    }
    pub fn rebuild_oci(&mut self) {
        let layers: Vec<_> = self.files.iter().map(|(name, bytes)| json!({"mediaType": if name == "latest.json" {"application/json"} else {"application/octet-stream"}, "digest": format!("sha256:{}", hex::encode(Sha256::digest(bytes))), "size": bytes.len(), "annotations": {"org.opencontainers.image.title": name}})).collect();
        self.oci = serde_json::to_vec(&json!({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json", "artifactType": RELEASE_TYPE, "annotations": {"org.opencontainers.image.source": "https://github.com/HawkeyNL/PersonalJarvisApp", "org.opencontainers.image.revision": self.config.identity.source_revision, "org.opencontainers.image.version": self.config.identity.version}, "layers": layers})).unwrap();
        self.config.identity.manifest_digest =
            format!("sha256:{}", hex::encode(Sha256::digest(&self.oci)));
    }
    pub fn validate(&self) -> Result<Document> {
        let layers = parse_oci(&self.oci, &self.config.identity)?;
        validate_document(
            &self.files["latest.json"],
            std::str::from_utf8(&self.files["latest.json.sig"]).unwrap(),
            &self.config,
            &layers,
        )
    }
}

#[test]
fn signed_complete_matrix_and_streaming_signature() {
    let f = Fixture::new("0.1.0", 1);
    assert_eq!(f.validate().unwrap().android_code(), 1);
    assert!(verify_signature(&b"hello"[..], &sign(b"hello"), &key()).is_ok());
    assert!(verify_signature(&b"modified"[..], &sign(b"hello"), &key()).is_err());
}

#[test]
fn missing_platform_traversal_unsigned_ios_and_oversize_rejected() {
    for name in [
        "Jarvis_0.1.0_android_universal.apk",
        "latest.json.sig",
        "Jarvis_0.1.0_macos_arm64.dmg",
        "Jarvis_0.1.0_ios_arm64_unsigned.ipa",
    ] {
        let mut f = Fixture::new("0.1.0", 1);
        f.files.remove(name);
        f.rebuild_oci();
        assert!(f.validate().is_err());
    }
    for name in [
        "../escape",
        "Jarvis_0.1.0_ios_arm64_signed.ipa",
        "extra.env",
    ] {
        let mut f = Fixture::new("0.1.0", 1);
        f.files.insert(name.into(), b"fixture".to_vec());
        f.rebuild_oci();
        assert!(f.validate().is_err());
    }
    let f = Fixture::new("0.1.0", 1);
    let mut raw: serde_json::Value = serde_json::from_slice(&f.oci).unwrap();
    raw["layers"][0]["size"] = json!(MAX_IPA + 1);
    let bytes = serde_json::to_vec(&raw).unwrap();
    let mut config = f.config;
    config.identity.manifest_digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    assert!(parse_oci(&bytes, &config.identity).is_err());
}

#[test]
fn tampered_manifest_cannot_be_approved_by_registry_digest() {
    let mut f = Fixture::new("0.1.0", 1);
    f.files.get_mut("latest.json").unwrap().push(b' ');
    f.rebuild_oci();
    assert!(f.validate().is_err());
}

#[test]
fn signed_bad_policy_and_paths_rejected() {
    for mutation in ["path", "signer", "matrix", "protocol", "code"] {
        let mut f = Fixture::new("0.1.0", 1);
        let mut doc: serde_json::Value = serde_json::from_slice(&f.files["latest.json"]).unwrap();
        match mutation {
            "path" => doc["artifacts"][0]["artifact"]["path"] = json!("../../fixture"),
            "signer" => doc["artifacts"][3]["signature"]["value"] = json!("c".repeat(64)),
            "matrix" => doc["artifacts"][0]["platform"] = json!("ios"),
            "protocol" => doc["release"]["minimum_client_protocol"] = json!(2),
            _ => doc["artifacts"][3]["metadata"]["version_code"] = json!(0),
        }
        let bytes = serde_json::to_vec(&doc).unwrap();
        f.files
            .insert("latest.json.sig".into(), sign(&bytes).into_bytes());
        f.files.insert("latest.json".into(), bytes);
        f.rebuild_oci();
        assert!(f.validate().is_err(), "{mutation}");
    }
}

#[test]
fn stable_discovery_is_not_signature_approval() {
    let mut f = Fixture::new("0.1.0", 1);
    assert!(f.config.discover(&f.oci).is_err());
    f.config.track_stable = true;
    f.config.discover(&f.oci).unwrap();
    f.config.identity.version = "0.2.0".into();
    assert!(f.config.discover(&f.oci).is_err());
}

#[test]
fn release_configuration_is_nested_strict_and_never_accepts_token_or_source_url() {
    let f = Fixture::new("0.1.0", 1);
    let mut value = json!({
        "identity": {"schema_version": 1, "github_username": "fixture", "version": "0.1.0",
            "source_revision": f.config.identity.source_revision, "manifest_digest": f.config.identity.manifest_digest},
        "tauri_signing_public_key": key(), "android_signing_certificate_sha256": "a".repeat(64), "track_stable": false
    });
    serde_json::from_value::<ReleaseConfig>(value.clone())
        .unwrap()
        .validate()
        .unwrap();
    for field in ["token", "source_url", "destination", "helper"] {
        value[field] = json!("fixture-not-a-secret");
        assert!(serde_json::from_value::<ReleaseConfig>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove(field);
    }
}
