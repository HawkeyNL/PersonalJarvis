use jarvis_app_downloads::mirror::{self, Config, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::{symlink, PermissionsExt},
};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

struct Fixture {
    config: Config,
    manifest: Vec<u8>,
    descriptor: Vec<u8>,
    ipa: Vec<u8>,
}
impl Fixture {
    fn new(version: &str) -> Self {
        let ipa = b"test-only-reviewed-IPA-fixture".to_vec();
        let descriptor = serde_json::to_vec(&json!({
            "schema_version":1,"version":version,"source_revision":"a".repeat(40),
            "platform":"ios","architecture":"arm64","build_number":1,
            "distribution":"manual-owner-signing","apple_signed":false,"automatic_install":false,
            "artifact":{"name":format!("Jarvis_{version}_ios_arm64_unsigned.ipa"),"size":ipa.len(),
                "sha256":hex::encode(Sha256::digest(&ipa))}
        }))
        .unwrap();
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
            "artifactType":mirror::ARTIFACT_TYPE,
            "annotations":{"org.opencontainers.image.revision":"a".repeat(40),
                "org.opencontainers.image.source":"https://github.com/HawkeyNL/PersonalJarvisApp"},
            "layers":[
                {"mediaType":"application/octet-stream","digest":digest(&ipa),"size":ipa.len(),
                    "annotations":{"org.opencontainers.image.title":format!("Jarvis_{version}_ios_arm64_unsigned.ipa")}},
                {"mediaType":"application/json","digest":digest(&descriptor),"size":descriptor.len(),
                    "annotations":{"org.opencontainers.image.title":"ios-candidate.json"}}
            ]
        })).unwrap();
        let config = Config {
            schema_version: 1,
            github_username: "fixture-owner".into(),
            version: version.into(),
            source_revision: "a".repeat(40),
            manifest_digest: digest(&manifest),
        };
        Self {
            config,
            manifest,
            descriptor,
            ipa,
        }
    }
    fn publish(&self, store: &Store) -> mirror::Result<()> {
        let (ipa, descriptor) = mirror::parse_manifest(&self.manifest, &self.config)?;
        mirror::validate_candidate(&self.descriptor, &descriptor, &ipa, &self.config)?;
        let mut stage = store.stage(&self.config)?;
        stage.file.write_all(&self.ipa).unwrap();
        store.publish(stage, &self.config, &ipa, &self.manifest, &self.descriptor)
    }
    fn change_manifest(&mut self, change: impl FnOnce(&mut Value)) {
        let mut value: Value = serde_json::from_slice(&self.manifest).unwrap();
        change(&mut value);
        self.manifest = serde_json::to_vec(&value).unwrap();
        self.config.manifest_digest = digest(&self.manifest);
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.path().join("ios")).unwrap();
    fs::set_permissions(root.path().join("ios"), fs::Permissions::from_mode(0o755)).unwrap();
    let store = Store::open(root.path(), unsafe { libc::geteuid() }).unwrap();
    (root, store)
}

#[test]
fn full_verified_import_is_idempotent_and_no_token_enters_public_index() {
    let (root, store) = store();
    let fixture = Fixture::new("0.1.0");
    fixture.publish(&store).unwrap();
    fixture.publish(&store).unwrap();
    let index = fs::read_to_string(root.path().join("index.html")).unwrap();
    assert!(index.contains("/downloads/ios/v0.1.0/Jarvis_0.1.0_ios_arm64_unsigned.ipa"));
    assert!(!index.contains("fixture-owner"));
    assert_eq!(
        fs::read(
            root.path()
                .join("ios/v0.1.0")
                .join(fixture.config.filename())
        )
        .unwrap(),
        fixture.ipa
    );
}

#[test]
fn failed_import_preserves_prior_index_and_cleans_staging() {
    let (root, store) = store();
    Fixture::new("0.1.0").publish(&store).unwrap();
    let old = fs::read(root.path().join("index.html")).unwrap();
    let mut bad = Fixture::new("0.1.1");
    bad.ipa[0] ^= 1;
    assert!(bad.publish(&store).is_err());
    assert_eq!(fs::read(root.path().join("index.html")).unwrap(), old);
    assert_eq!(fs::read_dir(root.path().join("ios")).unwrap().count(), 1);
}

#[test]
fn tampered_manifest_rejected_before_staging() {
    let (_, store) = store();
    let mut bad = Fixture::new("0.1.0");
    bad.manifest.push(b' ');
    assert!(bad.publish(&store).is_err());
}

#[test]
fn unsafe_extra_missing_duplicate_and_oversized_layers_fail_closed() {
    type Mutation = Box<dyn Fn(&mut Value)>;
    let cases: Vec<Mutation> = vec![
        Box::new(|v| {
            v["layers"][0]["annotations"]["org.opencontainers.image.title"] = json!("../secret")
        }),
        Box::new(|v| v["layers"][0]["size"] = json!(mirror::MAX_IPA + 1)),
        Box::new(|v| {
            v["layers"].as_array_mut().unwrap().pop();
        }),
        Box::new(|v| v["layers"][1] = v["layers"][0].clone()),
        Box::new(|v| v["layers"][0]["annotations"]["io.deis.oras.content.unpack"] = json!("true")),
        Box::new(|v| v["artifactType"] = json!("other")),
    ];
    for change in cases {
        let mut f = Fixture::new("0.1.0");
        f.change_manifest(change);
        assert!(mirror::parse_manifest(&f.manifest, &f.config).is_err());
    }
}

#[test]
fn automatic_install_or_false_signing_claim_rejected_even_if_hash_matches() {
    for field in ["apple_signed", "automatic_install"] {
        let mut f = Fixture::new("0.1.0");
        let mut value: Value = serde_json::from_slice(&f.descriptor).unwrap();
        value[field] = json!(true);
        f.descriptor = serde_json::to_vec(&value).unwrap();
        let sha = digest(&f.descriptor);
        let size = f.descriptor.len();
        f.change_manifest(|v| {
            v["layers"][1]["digest"] = json!(sha);
            v["layers"][1]["size"] = json!(size);
        });
        let (ipa, descriptor) = mirror::parse_manifest(&f.manifest, &f.config).unwrap();
        assert!(mirror::validate_candidate(&f.descriptor, &descriptor, &ipa, &f.config).is_err());
    }
}

#[test]
fn symlink_generation_and_unsafe_token_files_are_rejected() {
    let (root, store) = store();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("ios/v0.1.0")).unwrap();
    assert!(Fixture::new("0.1.0").publish(&store).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    let token = outside.path().join("token");
    fs::write(&token, b"fixture-token-not-real").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    let owner = unsafe { libc::geteuid() };
    assert!(mirror::read_protected(&token, owner, true, 1024).is_err());
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(mirror::read_protected(&token, owner, true, 1024).is_ok());
    let link = outside.path().join("link");
    symlink(&token, &link).unwrap();
    assert!(mirror::read_protected(&link, owner, true, 1024).is_err());
    fs::hard_link(&token, outside.path().join("hardlink")).unwrap();
    assert!(mirror::read_protected(&token, owner, true, 1024).is_err());
}

#[test]
fn concurrent_writer_and_immutable_version_replacement_are_refused() {
    let (root, store) = store();
    assert!(Store::open(root.path(), unsafe { libc::geteuid() }).is_err());
    let fixture = Fixture::new("0.1.0");
    fixture.publish(&store).unwrap();
    let mut other = Fixture::new("0.1.0");
    other.change_manifest(|v| v["unused"] = json!("different reviewed bytes"));
    assert!(other.publish(&store).is_err());
}

#[test]
fn older_explicit_candidate_does_not_reorder_latest_first_index() {
    let (root, store) = store();
    Fixture::new("0.2.0").publish(&store).unwrap();
    Fixture::new("0.1.0").publish(&store).unwrap();
    let index = fs::read_to_string(root.path().join("index.html")).unwrap();
    assert!(index.find("v0.2.0").unwrap() < index.find("v0.1.0").unwrap());
}
