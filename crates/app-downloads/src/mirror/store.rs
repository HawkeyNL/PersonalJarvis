use super::{digest_hex, Config, Layer, Result, MAX_METADATA};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;

pub fn validate_directory(path: &Path, owner: u32) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|_| "required managed directory is missing")?;
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err("unsafe managed directory ownership, permissions or type");
    }
    Ok(())
}

fn open_regular(path: &Path, owner: u32, private: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "required managed file is missing or unsafe")?;
    let meta = file.metadata().map_err(|_| "cannot inspect managed file")?;
    let forbidden = if private { 0o077 } else { 0o022 };
    if !meta.is_file() || meta.uid() != owner || meta.mode() & forbidden != 0 || meta.nlink() != 1 {
        return Err("unsafe managed file ownership, permissions or type");
    }
    Ok(file)
}

pub fn read_protected(path: &Path, owner: u32, private: bool, max: usize) -> Result<Vec<u8>> {
    let file = open_regular(path, owner, private)?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read managed file")?;
    if bytes.len() > max {
        return Err("managed file exceeds size limit");
    }
    Ok(bytes)
}

pub struct Store {
    root: PathBuf,
    owner: u32,
    _lock: File,
}

pub struct Stage {
    directory: TempDir,
    pub file: File,
}

impl Store {
    pub fn open(root: &Path, owner: u32) -> Result<Self> {
        validate_directory(root, owner)?;
        validate_directory(&root.join("ios"), owner)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(".sync.lock"))
            .map_err(|_| "cannot open mirror lock")?;
        let meta = lock.metadata().map_err(|_| "cannot inspect mirror lock")?;
        if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
            return Err("unsafe mirror lock");
        }
        lock.try_lock_exclusive()
            .map_err(|_| "another mirror operation is running")?;
        Ok(Self {
            root: root.to_owned(),
            owner,
            _lock: lock,
        })
    }

    pub fn stage(&self, config: &Config) -> Result<Stage> {
        config.validate()?;
        let directory = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(self.root.join("ios"))
            .map_err(|_| "cannot stage candidate")?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot protect staging directory")?;
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path().join(config.filename()))
            .map_err(|_| "cannot stage IPA")?;
        Ok(Stage { directory, file })
    }

    /// Fully verify before exposing the canonical name. A failed import cannot
    /// replace a prior version/index. This initial importer does not delete history.
    pub fn publish(
        &self,
        stage: Stage,
        config: &Config,
        ipa: &Layer,
        manifest: &[u8],
        descriptor: &[u8],
    ) -> Result<()> {
        let (expected_ipa, expected_descriptor) = super::parse_manifest(manifest, config)?;
        if expected_ipa.digest != ipa.digest || expected_ipa.size != ipa.size {
            return Err("IPA changed during staging");
        }
        super::validate_candidate(descriptor, &expected_descriptor, ipa, config)?;
        let path = stage.directory.path().join(config.filename());
        verify_file(&path, self.owner, ipa)?;
        stage.file.sync_all().map_err(|_| "cannot sync candidate")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(|_| "cannot set candidate mode")?;
        write_new(&stage.directory.path().join("manifest.json"), manifest)?;
        write_new(
            &stage.directory.path().join("ios-candidate.json"),
            descriptor,
        )?;
        // Keep only non-secret approval identity, never the account name/token.
        write_new(
            &stage.directory.path().join("approved.json"),
            &serde_json::to_vec(&serde_json::json!({
                "version": config.version, "source_revision": config.source_revision,
                "manifest_digest": config.manifest_digest
            }))
            .map_err(|_| "cannot serialize approval")?,
        )?;
        fs::set_permissions(stage.directory.path(), fs::Permissions::from_mode(0o755))
            .map_err(|_| "cannot set generation mode")?;
        sync_directory(stage.directory.path())?;
        let final_path = self.root.join("ios").join(format!("v{}", config.version));
        if fs::symlink_metadata(&final_path).is_ok() {
            validate_directory(&final_path, self.owner)?;
            if read_protected(
                &final_path.join("manifest.json"),
                self.owner,
                false,
                MAX_METADATA,
            )? != manifest
            {
                return Err("version already exists with different bytes; never replace an immutable version");
            }
            verify_file(&final_path.join(config.filename()), self.owner, ipa)?;
        } else {
            fs::rename(stage.directory.path(), &final_path)
                .map_err(|_| "candidate activation failed")?;
            sync_directory(&self.root.join("ios"))?;
        }
        self.render_index()
    }

    fn render_index(&self) -> Result<()> {
        let mut versions = Vec::new();
        for item in
            fs::read_dir(self.root.join("ios")).map_err(|_| "cannot inspect candidate inventory")?
        {
            let item = item.map_err(|_| "cannot inspect candidate inventory")?;
            let name = item.file_name();
            let name = name.to_str().ok_or("invalid candidate directory name")?;
            if name.starts_with(".staging-") {
                continue;
            }
            let version = name
                .strip_prefix('v')
                .ok_or("unexpected candidate directory")?;
            let parsed = super::super::stable_version(version)
                .map_err(|_| "invalid candidate directory version")?;
            validate_directory(&item.path(), self.owner)?;
            let approved = read_protected(
                &item.path().join("approved.json"),
                self.owner,
                false,
                MAX_METADATA,
            )?;
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Approved {
                version: String,
                source_revision: String,
                manifest_digest: String,
            }
            let a: Approved =
                serde_json::from_slice(&approved).map_err(|_| "invalid installed approval")?;
            let c = Config {
                schema_version: 1,
                github_username: "unused".into(),
                version: a.version,
                source_revision: a.source_revision,
                manifest_digest: a.manifest_digest,
            };
            if c.version != version {
                return Err("candidate directory/approval mismatch");
            }
            let manifest = read_protected(
                &item.path().join("manifest.json"),
                self.owner,
                false,
                MAX_METADATA,
            )?;
            let (ipa, meta) = super::parse_manifest(&manifest, &c)?;
            let descriptor = read_protected(
                &item.path().join("ios-candidate.json"),
                self.owner,
                false,
                MAX_METADATA,
            )?;
            super::validate_candidate(&descriptor, &meta, &ipa, &c)?;
            verify_file(&item.path().join(c.filename()), self.owner, &ipa)?;
            versions.push(parsed);
            if versions.len() > 1000 {
                return Err("candidate inventory exceeds limit");
            }
        }
        versions.sort();
        let mut html = String::from("<!doctype html><html lang=\"nl\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Jarvis downloads</title><body><h1>Jarvis iOS-testbuilds</h1><p>Deze IPA vereist lokale ondertekening door de eigenaar. Geen automatische installatie, Apple-distributiesignature of complete productrelease.</p><ul>");
        for version in versions.iter().rev() {
            // Version parsed as numeric SemVer; no untrusted HTML or URL input.
            html.push_str(&format!("<li><a href=\"/downloads/ios/v{version}/Jarvis_{version}_ios_arm64_unsigned.ipa\">Jarvis {version} — IPA voor zelf ondertekenen</a></li>"));
        }
        html.push_str("</ul><p>Updates voor aangemelde clients blijven gescheiden van deze publieke installatiebestanden.</p></body></html>\n");
        let temporary = tempfile::Builder::new()
            .prefix(".index-")
            .tempfile_in(&self.root)
            .map_err(|_| "cannot stage index")?;
        temporary
            .as_file()
            .write_all(html.as_bytes())
            .map_err(|_| "cannot write index")?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| "cannot sync index")?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|_| "cannot set index mode")?;
        let index = self.root.join("index.html");
        if fs::symlink_metadata(&index).is_ok() {
            open_regular(&index, self.owner, false)?;
        }
        temporary
            .persist(index)
            .map_err(|_| "cannot activate index")?;
        sync_directory(&self.root)
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(path)
        .map_err(|_| "cannot stage metadata")?;
    file.write_all(bytes).map_err(|_| "cannot write metadata")?;
    file.sync_all().map_err(|_| "cannot sync metadata")
}

fn verify_file(path: &Path, owner: u32, layer: &Layer) -> Result<()> {
    let mut file = open_regular(path, owner, false)?;
    if file.metadata().map_err(|_| "cannot inspect IPA")?.len() != layer.size {
        return Err("IPA size mismatch");
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|_| "cannot read IPA")?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if hex::encode(hash.finalize()) != digest_hex(&layer.digest)? {
        return Err("IPA checksum mismatch");
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| "cannot sync managed directory")
}
