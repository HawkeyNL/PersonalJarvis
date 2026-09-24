//! Transactional full release staging, separate public copies and authenticated
//! mirror activation. Never link the protected mirror into Caddy's public root.
use super::release::{self, Document, ReleaseConfig};
use super::store::{open_regular, sync_directory, verify_file, write_new};
use super::{read_protected, validate_directory, Layer, Result, MAX_METADATA};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use tempfile::TempDir;

#[cfg(test)]
mod tests;

pub struct ReleaseStore {
    root: PathBuf,
    owner: u32,
    _lock: File,
}
pub struct Stage {
    directory: TempDir,
}

fn directory(path: &Path, owner: u32) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        fs::create_dir(path).map_err(|_| "cannot create managed directory")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|_| "cannot set directory mode")?;
    }
    validate_directory(path, owner)
}

impl ReleaseStore {
    pub fn open(root: &Path, owner: u32) -> Result<Self> {
        validate_directory(root, owner)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(".sync.lock"))
            .map_err(|_| "cannot open release lock")?;
        let meta = lock.metadata().map_err(|_| "cannot inspect release lock")?;
        if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
            return Err("unsafe release lock");
        }
        lock.try_lock_exclusive()
            .map_err(|_| "another release sync is running")?;
        directory(&root.join("releases"), owner)?;
        Ok(Self {
            root: root.to_owned(),
            owner,
            _lock: lock,
        })
    }
    pub fn stage(&self) -> Result<Stage> {
        let directory = tempfile::Builder::new()
            .prefix(".release-staging-")
            .tempdir_in(&self.root)
            .map_err(|_| "cannot create release staging")?;
        Ok(Stage { directory })
    }
    pub fn stage_file(&self, stage: &Stage, name: &str) -> Result<File> {
        if name.is_empty()
            || name.len() > 160
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || name.starts_with('.')
        {
            return Err("unsafe staging name");
        }
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(stage.directory.path().join(name))
            .map_err(|_| "cannot create staged file")
    }
    pub fn path<'a>(&self, stage: &'a Stage) -> &'a Path {
        stage.directory.path()
    }

    pub async fn prepare(
        &self,
        stage: Stage,
        config: &ReleaseConfig,
        oci: &[u8],
        public_root: &Path,
    ) -> Result<PathBuf> {
        // Never accept a Boolean caller claim that the APK was verified.
        super::apk::verify(
            &stage.directory.path().join(format!(
                "Jarvis_{}_android_universal.apk",
                config.identity.version
            )),
            &config.android_signing_certificate_sha256,
        )
        .await?;
        self.prepare_verified(stage, config, oci, public_root)
    }

    fn prepare_verified(
        &self,
        stage: Stage,
        config: &ReleaseConfig,
        oci: &[u8],
        public_root: &Path,
    ) -> Result<PathBuf> {
        let layers = release::parse_oci(oci, &config.identity)?;
        let staged = stage.directory.path();
        for (name, layer) in &layers {
            verify_file(&staged.join(name), self.owner, layer)?;
        }
        let bytes = read_protected(&staged.join("latest.json"), self.owner, false, MAX_METADATA)?;
        let sig = read_protected(&staged.join("latest.json.sig"), self.owner, false, 16384)?;
        let sig = std::str::from_utf8(&sig).map_err(|_| "invalid release signature")?;
        let doc = release::validate_document(&bytes, sig, config, &layers)?;
        for e in &doc.artifacts {
            if e.platform != "android" {
                let name = e
                    .artifact
                    .path
                    .rsplit('/')
                    .next()
                    .ok_or("missing filename")?;
                let detached = read_protected(
                    &staged.join(format!("{name}.sig")),
                    self.owner,
                    false,
                    16384,
                )?;
                if std::str::from_utf8(&detached)
                    .map_err(|_| "invalid signature")?
                    .trim()
                    != e.signature.value.trim()
                {
                    return Err("detached and signed-manifest signatures disagree");
                }
                release::verify_signature(
                    open_regular(&staged.join(name), self.owner, false)?,
                    &e.signature.value,
                    &config.tauri_signing_public_key,
                )?;
            }
        }
        self.check_progression(&doc)?;
        let generation = staged.join("generation");
        directory(&generation, self.owner)?;
        for (platform, architecture, a) in doc.files() {
            let name = a.path.rsplit('/').next().ok_or("missing asset name")?;
            let target = generation.join(format!("{platform}-{architecture}"));
            directory(&target, self.owner)?;
            fs::rename(staged.join(name), target.join(name))
                .map_err(|_| "cannot assemble generation")?;
            fs::set_permissions(target.join(name), fs::Permissions::from_mode(0o644))
                .map_err(|_| "cannot set asset mode")?;
            sync_directory(&target)?;
        }
        write_new(&generation.join("manifest.json"), &bytes)?;
        write_new(&generation.join("manifest.json.sig"), sig.as_bytes())?;
        write_new(&generation.join("oci.json"), oci)?;
        sync_directory(&generation)?;
        let destination = self
            .root
            .join("releases")
            .join(format!("v{}", doc.release.version));
        if fs::symlink_metadata(&destination).is_ok() {
            self.verify_existing(&destination, &doc, &bytes, &layers)?;
        } else {
            fs::rename(&generation, &destination)
                .map_err(|_| "cannot install immutable generation")?;
            sync_directory(&self.root.join("releases"))?;
        }
        // Public publication happens before active updater switch. Failure may
        // leave an extra verified installer, never a partially active update.
        publish_public(public_root, self.owner, &destination, &doc, &bytes, &layers)?;
        Ok(destination)
    }

    fn check_progression(&self, doc: &Document) -> Result<()> {
        let current = self.root.join("current");
        if fs::symlink_metadata(&current).is_err() {
            return Ok(());
        }
        let relative =
            fs::read_link(&current).map_err(|_| "active generation must be a managed symlink")?;
        let version = relative
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| v.strip_prefix('v'))
            .ok_or("invalid active release")?;
        let old_version =
            super::super::stable_version(version).map_err(|_| "invalid active release")?;
        if relative != Path::new("releases").join(format!("v{version}")) {
            return Err("active generation escapes managed releases");
        }
        let path = self.root.join(&relative);
        validate_directory(&path, self.owner)?;
        let bytes = read_protected(&path.join("manifest.json"), self.owner, false, MAX_METADATA)?;
        let old: Document =
            serde_json::from_slice(&bytes).map_err(|_| "invalid previous release metadata")?;
        let next =
            super::super::stable_version(&doc.release.version).map_err(|_| "invalid release")?;
        if old.release.version != version
            || next < old_version
            || (next > old_version && doc.android_code() <= old.android_code())
        {
            return Err("application or Android version would downgrade or repeat");
        }
        Ok(())
    }
    fn verify_existing(
        &self,
        path: &Path,
        doc: &Document,
        bytes: &[u8],
        layers: &BTreeMap<String, Layer>,
    ) -> Result<()> {
        verify_generation(path, self.owner, doc, bytes, layers)
    }
    pub fn activate(&self, destination: &Path) -> Result<()> {
        if destination.parent() != Some(self.root.join("releases").as_path()) {
            return Err("unmanaged release target");
        }
        validate_directory(destination, self.owner)?;
        let temporary = tempfile::Builder::new()
            .prefix(".activation-")
            .tempdir_in(&self.root)
            .map_err(|_| "cannot stage activation")?;
        let name = destination.file_name().ok_or("missing release name")?;
        symlink(
            Path::new("releases").join(name),
            temporary.path().join("current"),
        )
        .map_err(|_| "cannot stage current link")?;
        fs::rename(temporary.path().join("current"), self.root.join("current"))
            .map_err(|_| "cannot activate release")?;
        sync_directory(&self.root)
    }
}

fn verify_generation(
    path: &Path,
    owner: u32,
    doc: &Document,
    bytes: &[u8],
    layers: &BTreeMap<String, Layer>,
) -> Result<()> {
    validate_directory(path, owner)?;
    if read_protected(&path.join("manifest.json"), owner, false, MAX_METADATA)? != bytes {
        return Err("immutable version already contains different bytes");
    }
    for (platform, architecture, a) in doc.files() {
        let target = path.join(format!("{platform}-{architecture}"));
        validate_directory(&target, owner)?;
        let name = a.path.rsplit('/').next().ok_or("missing filename")?;
        verify_file(
            &target.join(name),
            owner,
            layers.get(name).ok_or("missing layer")?,
        )?;
    }
    Ok(())
}

fn publish_public(
    root: &Path,
    owner: u32,
    source: &Path,
    doc: &Document,
    bytes: &[u8],
    layers: &BTreeMap<String, Layer>,
) -> Result<()> {
    validate_directory(root, owner)?;
    let releases = root.join("releases");
    directory(&releases, owner)?;
    let destination = releases.join(format!("v{}", doc.release.version));
    if fs::symlink_metadata(&destination).is_ok() {
        return verify_generation(&destination, owner, doc, bytes, layers);
    }
    let temporary = tempfile::Builder::new()
        .prefix(".public-staging-")
        .tempdir_in(root)
        .map_err(|_| "cannot stage public installers")?;
    for (platform, architecture, a) in doc.files() {
        let name = a.path.rsplit('/').next().ok_or("missing filename")?;
        let target = format!("{platform}-{architecture}");
        directory(&temporary.path().join(&target), owner)?;
        let mut input = open_regular(&source.join(&target).join(name), owner, false)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o644)
            .open(temporary.path().join(&target).join(name))
            .map_err(|_| "cannot stage installer")?;
        std::io::copy(&mut input, &mut output).map_err(|_| "installer copy failed")?;
        output
            .set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|_| "cannot set public installer mode")?;
        output.sync_all().map_err(|_| "cannot sync installer")?;
        sync_directory(&temporary.path().join(&target))?;
    }
    write_new(&temporary.path().join("manifest.json"), bytes)?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755))
        .map_err(|_| "cannot set installer directory mode")?;
    verify_generation(temporary.path(), owner, doc, bytes, layers)?;
    sync_directory(temporary.path())?;
    fs::rename(temporary.path(), &destination).map_err(|_| "cannot publish installers")?;
    sync_directory(&releases)
}

pub(super) fn public_links(root: &Path, owner: u32) -> Result<String> {
    let releases = root.join("releases");
    if fs::symlink_metadata(&releases).is_err() {
        return Ok(String::new());
    }
    validate_directory(&releases, owner)?;
    let mut versions = Vec::new();
    for entry in fs::read_dir(&releases).map_err(|_| "cannot read public inventory")? {
        let entry = entry.map_err(|_| "cannot read public inventory")?;
        let name = entry.file_name();
        let version = name
            .to_str()
            .and_then(|n| n.strip_prefix('v'))
            .ok_or("invalid public inventory")?;
        let version =
            super::super::stable_version(version).map_err(|_| "invalid public version")?;
        validate_directory(&entry.path(), owner)?;
        versions.push(version);
        if versions.len() > 1000 {
            return Err("public inventory exceeds limit");
        }
    }
    versions.sort();
    if versions.is_empty() {
        return Ok(String::new());
    }
    let latest = versions.last().expect("nonempty versions");
    let mut html = String::from("<div class=\"release-browser\"><h2>Jarvis clients</h2><label class=\"version-picker\" for=\"release-version\">Versie</label><select id=\"release-version\" class=\"version-picker\">");
    for version in versions.iter().rev() {
        let selected = if version == latest { " selected" } else { "" };
        let label = if version == latest { " — Latest" } else { "" };
        html.push_str(&format!(
            "<option value=\"{version}\"{selected}>v{version}{label}</option>"
        ));
    }
    html.push_str("</select><style>@supports selector(:has(option:checked)) {");
    for version in versions.iter().rev() {
        // Only validated numeric SemVer enters CSS/HTML. No scripts or CSP
        // relaxation: older browsers simply show all releases as a fallback.
        html.push_str(&format!(".release-browser:has(option[value=\"{version}\"]:checked) .client-release:not([data-version=\"{version}\"]) {{ display: none; }}"));
    }
    html.push_str("}</style>");
    for version in versions.iter().rev() {
        let badge = if version == latest {
            " <span class=\"latest-badge\">Latest · nieuwste versie</span>"
        } else {
            ""
        };
        html.push_str(&format!("<div class=\"client-release\" data-version=\"{version}\"><h3>Jarvis {version}{badge}</h3><ul>"));
        for (target, suffix, label) in [
            ("linux-x86_64", ".AppImage", "Linux"),
            ("windows-x86_64", ".exe", "Windows"),
            ("macos-arm64", ".dmg", "macOS"),
            ("android-universal", ".apk", "Android"),
            ("ios-arm64", "_unsigned.ipa", "iOS — zelf ondertekenen"),
        ] {
            let name = format!("Jarvis_{version}_{}{suffix}", target.replace('-', "_"));
            validate_directory(&releases.join(format!("v{version}/{target}")), owner)?;
            open_regular(
                &releases.join(format!("v{version}/{target}/{name}")),
                owner,
                false,
            )?;
            html.push_str(&format!("<li><a href=\"/downloads/releases/v{version}/{target}/{name}\">Jarvis {version} — {label}</a></li>"));
        }
        html.push_str("</ul></div>");
    }
    html.push_str("</div>");
    Ok(html)
}
