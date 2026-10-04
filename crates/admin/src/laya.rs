//! Owner-triggered install and on/off switch for the optional local Laya
//! System-1 classifier (see docs/LAYA_INTENT_ROUTING.md).
//!
//! Every downloaded byte is pinned in this binary: the reviewed wheel lock and
//! model manifest are repository files compiled in with `include_str!`. Only
//! `install` uses the network; the release-owned `provision-laya` then installs
//! offline under its unchanged trust rules. Nothing from the model repository
//! is executed, and Core keeps treating every Laya label as untrusted advice.

use std::{
    collections::BTreeSet,
    fs::{self, DirBuilder, File},
    io::{self, Read, Write},
    os::unix::fs::{fchown, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    admin_helpers::{trusted_admin_helper_command, AdminHelper},
    ai_accounts::validate_system_executable,
    mutation_lock, trusted_command, CONFIG_LOCK,
};

const LOCK: &str = include_str!("../../../deploy/laya/requirements.lock");
const MODELS: &str = include_str!("../../../deploy/laya/models.sha256");
const LAYA_VERSION: &str = "0.3.20";
const REVISION: &str = "55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851";
const MODEL_SOURCE: &str = "https://huggingface.co/convaiinnovations/laya/resolve";
const WHEEL_SOURCES: [&str; 2] = [
    "https://files.pythonhosted.org/packages/",
    "https://download.pytorch.org/whl/cpu/",
];
/// Sum of the ten pinned model files; shown before the owner confirms.
const MODEL_BYTES: u64 = 1_524_397_210;
const ARTIFACT_LIMIT: u64 = 1 << 30;
/// Staging, a second model copy and the venv need about 5 GB; keep headroom.
const REQUIRED_FREE: u64 = 8 << 30;
const SOCKET: &str = "jarvis-laya.socket";
const SERVICE: &str = "jarvis-laya.service";
const UPDATER_LOCK: &str = "/run/jarvis-updater.lock";
const CURL: &str = "/usr/bin/curl";
const LOGGER: &str = "/usr/bin/logger";
const PYTHON: &str = "/usr/bin/python3";

#[derive(Debug, Subcommand)]
pub(crate) enum LayaCommand {
    /// Read-only: pinned versions, unit states, Core mode, last probe and disk use.
    Status,
    /// Download and verify the pinned wheels and model files, provision offline,
    /// then enable Laya in shadow mode.
    Install,
    /// Enable the socket, start and probe the service; mode off becomes shadow.
    Enable,
    /// Set Core's mode to off, then stop and disable the socket and service.
    Disable,
    /// Set JARVIS_LAYA_MODE and restart Core. Shadow and primary require an
    /// enabled socket and a healthy Laya.
    Mode { mode: LayaMode },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum LayaMode {
    Off,
    Shadow,
    Primary,
}

impl LayaMode {
    fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Primary => "primary",
        }
    }
}

/// Fixed production paths. Only tests construct another layout.
struct Layout {
    cache: PathBuf,
    runtime: PathBuf,
    models: PathBuf,
    core_env: PathBuf,
    owner: u32,
    required_free: u64,
}

fn production() -> Layout {
    Layout {
        cache: "/var/cache/jarvis-laya".into(),
        runtime: "/opt/jarvis/laya".into(),
        models: "/var/lib/jarvis-laya/models".into(),
        core_env: "/etc/jarvis/core.env".into(),
        owner: 0,
        required_free: REQUIRED_FREE,
    }
}

impl Layout {
    fn stage(&self) -> PathBuf {
        self.cache.join("staging")
    }
    fn probe_record(&self) -> PathBuf {
        self.cache.join("last-probe.json")
    }
}

/// Host side effects, injectable so the state machine is tested rootless.
trait Host {
    fn preflight(&self) -> Result<()>;
    fn fetch(&self, url: &str, output: &Path, limit: u64) -> Result<()>;
    fn provision(&self) -> Result<()>;
    fn systemctl(&self, args: &[&str]) -> Result<()>;
    fn unit_state(&self, query: &str, unit: &str) -> String;
    fn restart_core(&self) -> Result<()>;
    fn probe(&self) -> Result<()>;
    fn audit(&self, record: &str) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Artifact {
    url: String,
    /// Destination relative to the staging directory.
    path: String,
    sha256: String,
    limit: u64,
    exact_size: Option<u64>,
}

#[derive(Debug, Serialize)]
struct LayaStatus {
    installed: bool,
    laya_version: &'static str,
    model_revision: &'static str,
    socket_enabled: String,
    socket_active: String,
    service_active: String,
    mode: String,
    last_probe: Option<ProbeRecord>,
    download_bytes: u64,
    disk_bytes: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProbeRecord {
    at: u64,
    ok: bool,
}

pub(crate) fn run(command: LayaCommand, json: bool) -> Result<()> {
    let layout = production();
    let host = System;
    if let LayaCommand::Status = command {
        let status = status(&layout, &host)?;
        if json {
            println!("{}", serde_json::to_string(&status)?);
        } else {
            print_status(&status);
        }
        return Ok(());
    }
    if json {
        bail!("--json is supported only for read-only laya status");
    }
    // Serialize with Core configuration edits and with updates, which
    // replace the release that owns provision-laya and restart Core.
    let _config = mutation_lock(CONFIG_LOCK)?;
    let _updater = mutation_lock(UPDATER_LOCK)?;
    let message = execute(&layout, &host, command)?;
    println!("jarvis laya: {message}");
    Ok(())
}

fn execute(layout: &Layout, host: &dyn Host, command: LayaCommand) -> Result<String> {
    let action = match command {
        LayaCommand::Status => bail!("status is read-only"),
        LayaCommand::Install => "install",
        LayaCommand::Enable => "enable",
        LayaCommand::Disable => "disable",
        LayaCommand::Mode { mode } => match mode {
            LayaMode::Off => "mode-off",
            LayaMode::Shadow => "mode-shadow",
            LayaMode::Primary => "mode-primary",
        },
    };
    host.audit(&format!("action={action} outcome=initiated"))
        .context("audit event could not be recorded; nothing changed")?;
    let result = match command {
        LayaCommand::Status => unreachable!("handled above"),
        LayaCommand::Install => install(layout, host),
        LayaCommand::Enable => enable(layout, host),
        LayaCommand::Disable => disable(layout, host),
        LayaCommand::Mode { mode } => set_mode_checked(layout, host, mode),
    };
    let outcome = if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    };
    host.audit(&format!("action={action} outcome={outcome}"))
        .context("Laya action may have completed but its audit event failed")?;
    result
}

fn install(layout: &Layout, host: &dyn Host) -> Result<String> {
    host.preflight()?;
    if !provisioned(layout)? {
        for path in [&layout.cache, &layout.runtime, &layout.models] {
            check_free(path, layout.required_free)?;
        }
        let artifacts = pinned_artifacts()?;
        let downloaded = stage_artifacts(layout, host, &artifacts)?;
        println!(
            "jarvis laya: {} pinned artifacts verified ({downloaded} downloaded); provisioning offline",
            artifacts.len()
        );
        host.provision()?;
        ensure!(
            provisioned(layout)?,
            "provisioner finished but the pinned Laya runtime is not active"
        );
    }
    enable(layout, host)?;
    Ok(format!(
        "Laya {LAYA_VERSION} (model {}) installed and enabled; Core mode: {}",
        &REVISION[..12],
        read_mode(layout)?.name()
    ))
}

fn enable(layout: &Layout, host: &dyn Host) -> Result<String> {
    ensure!(
        provisioned(layout)?,
        "Laya is not installed; run: sudo jarvis laya install"
    );
    host.systemctl(&["enable", "--now", SOCKET])?;
    let started = host
        .systemctl(&["start", SERVICE])
        .and_then(|()| probe(layout, host));
    if let Err(error) = started {
        let _ = host.systemctl(&["disable", "--now", SERVICE, SOCKET]);
        return Err(error.context("Laya did not become healthy and was turned off again"));
    }
    if read_mode(layout)? == LayaMode::Off {
        set_mode(layout, host, LayaMode::Shadow)?;
    }
    Ok(format!(
        "Laya enabled; Core mode: {}",
        read_mode(layout)?.name()
    ))
}

fn disable(layout: &Layout, host: &dyn Host) -> Result<String> {
    // Core first stops consulting Laya; a failed restart leaves units as-is.
    set_mode(layout, host, LayaMode::Off)?;
    host.systemctl(&["disable", "--now", SERVICE, SOCKET])?;
    Ok("Laya stopped and disabled; Core mode: off".into())
}

fn set_mode_checked(layout: &Layout, host: &dyn Host, mode: LayaMode) -> Result<String> {
    if mode != LayaMode::Off {
        ensure!(
            host.unit_state("is-enabled", SOCKET) == "enabled",
            "Laya is not enabled; run: sudo jarvis laya enable"
        );
        probe(layout, host)?;
    }
    let changed = set_mode(layout, host, mode)?;
    Ok(format!(
        "Core mode: {}{}",
        mode.name(),
        if changed { "" } else { " (unchanged)" }
    ))
}

fn probe(layout: &Layout, host: &dyn Host) -> Result<()> {
    let result = host.probe();
    let record = ProbeRecord {
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs()),
        ok: result.is_ok(),
    };
    // The record is informational; never mask the probe result with it.
    let _ = owned_directory(layout, &layout.cache).and_then(|()| {
        write_atomic(
            &layout.probe_record(),
            &serde_json::to_vec(&record)?,
            0o644,
            None,
        )
    });
    result.context("Laya health probe failed")
}

/// The pinned runtime counts as installed only when the managed link points
/// at the exact release and the model snapshot carries the exact manifest.
fn provisioned(layout: &Layout) -> Result<bool> {
    let release = format!("laya-{LAYA_VERSION}-{REVISION}");
    let current = layout.runtime.join("current");
    match fs::read_link(&current) {
        Ok(target) if target == Path::new("releases").join(&release) => {}
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            bail!("{} is not a managed symlink", current.display())
        }
        Err(error) => return Err(error).context("inspect active Laya runtime"),
    }
    let python = layout
        .runtime
        .join("releases")
        .join(&release)
        .join("bin/python");
    if fs::symlink_metadata(python).is_err() {
        return Ok(false);
    }
    let manifest = layout.models.join(REVISION).join("models.sha256");
    match fs::symlink_metadata(&manifest) {
        Ok(metadata) if metadata.is_file() && metadata.len() == MODELS.len() as u64 => {
            Ok(fs::read(&manifest)? == MODELS.as_bytes())
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect pinned Laya model snapshot"),
    }
}

fn pinned_artifacts() -> Result<Vec<Artifact>> {
    let mut artifacts = wheel_artifacts(LOCK)?;
    artifacts.extend(model_artifacts(MODELS)?);
    Ok(artifacts)
}

fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Parses `# <https-url> <bytes>` source comments, each followed by exactly
/// one `name==version \` pin and one `--hash=sha256:` line.
fn wheel_artifacts(lock: &str) -> Result<Vec<Artifact>> {
    let mut artifacts = Vec::new();
    let mut names = BTreeSet::new();
    let mut source: Option<(String, u64)> = None;
    let mut pinned = false;
    for line in lock.lines() {
        if let Some(rest) = line.strip_prefix("# https://") {
            ensure!(source.is_none(), "lock source comment without a pin");
            let (url, size) = rest
                .split_once(' ')
                .context("lock source comment lacks a size")?;
            let url = format!("https://{url}");
            let size: u64 = size.parse().context("lock source size is invalid")?;
            ensure!(
                (1..=ARTIFACT_LIMIT).contains(&size),
                "lock source size is out of bounds"
            );
            ensure!(
                WHEEL_SOURCES.iter().any(|prefix| url.starts_with(prefix))
                    && url
                        .bytes()
                        .all(|byte| { byte.is_ascii_alphanumeric() || b":/._%+-".contains(&byte) }),
                "lock source is not an allowed HTTPS wheel URL: {url}"
            );
            let name = url
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .replace("%2B", "+");
            ensure!(
                name.ends_with(".whl")
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
                    && names.insert(name.clone()),
                "lock source names an unsafe or duplicate wheel: {name}"
            );
            source = Some((url, size));
        } else if line.starts_with('#') || line.trim().is_empty() {
            ensure!(source.is_none() && !pinned, "lock comment interrupts a pin");
        } else if let Some(digest) = line.trim().strip_prefix("--hash=sha256:") {
            ensure!(pinned && sha256_hex(digest), "lock hash is malformed");
            let (url, size) = source.take().context("lock hash without a source")?;
            let name = url
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .replace("%2B", "+");
            artifacts.push(Artifact {
                url,
                path: format!("wheels/{name}"),
                sha256: digest.to_owned(),
                limit: size,
                exact_size: Some(size),
            });
            pinned = false;
        } else {
            ensure!(
                source.is_some() && !pinned && line.contains("==") && line.ends_with(" \\"),
                "lock line is not a single hash-pinned requirement: {line}"
            );
            pinned = true;
        }
    }
    ensure!(
        source.is_none() && !pinned && !artifacts.is_empty(),
        "lock is incomplete"
    );
    Ok(artifacts)
}

/// Only configuration/tokenizer JSON and safetensors, below the revision.
fn model_artifacts(manifest: &str) -> Result<Vec<Artifact>> {
    let mut artifacts = Vec::new();
    let mut paths = BTreeSet::new();
    for line in manifest.lines() {
        let (digest, path) = line
            .split_once("  ")
            .context("model manifest line is malformed")?;
        ensure!(sha256_hex(digest), "model manifest hash is malformed");
        let safe = path.split('/').all(|name| {
            !name.is_empty()
                && !name.starts_with('.')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        });
        ensure!(
            safe && (path.ends_with(".json") || path.ends_with(".safetensors"))
                && paths.insert(path.to_owned()),
            "model manifest names an unsafe or duplicate file: {path}"
        );
        artifacts.push(Artifact {
            url: format!("{MODEL_SOURCE}/{REVISION}/{path}"),
            path: format!("models/{REVISION}/{path}"),
            sha256: digest.to_owned(),
            limit: ARTIFACT_LIMIT,
            exact_size: None,
        });
    }
    ensure!(!artifacts.is_empty(), "model manifest is empty");
    Ok(artifacts)
}

/// Places verified artifacts into the documented root-owned staging layout.
/// Already verified files are kept, so an interrupted install resumes.
/// Returns how many artifacts were downloaded.
fn stage_artifacts(layout: &Layout, host: &dyn Host, artifacts: &[Artifact]) -> Result<usize> {
    let stage = layout.stage();
    let mut directories = BTreeSet::from([
        PathBuf::new(),
        PathBuf::from("wheels"),
        PathBuf::from("models"),
    ]);
    for artifact in artifacts {
        let mut parent = Path::new(&artifact.path).parent();
        while let Some(directory) = parent.filter(|path| !path.as_os_str().is_empty()) {
            directories.insert(directory.to_owned());
            parent = directory.parent();
        }
    }
    owned_directory(layout, &layout.cache)?;
    for directory in &directories {
        owned_directory(layout, &stage.join(directory))?;
    }
    let mut expected: BTreeSet<PathBuf> =
        artifacts.iter().map(|a| PathBuf::from(&a.path)).collect();
    expected.extend(["requirements.lock", "models.sha256"].map(PathBuf::from));
    expected.extend(directories.iter().cloned());
    check_tree(layout, &stage, Path::new(""), &expected)?;

    // Never `$TMPDIR`: a root-only work directory beside staging.
    let work = tempfile::Builder::new()
        .prefix(".download.")
        .tempdir_in(&layout.cache)
        .context("create private Laya download directory")?;
    fs::set_permissions(work.path(), fs::Permissions::from_mode(0o700))?;
    let mut downloaded = 0;
    for artifact in artifacts {
        let destination = stage.join(&artifact.path);
        if staged(layout, &destination, artifact)? {
            continue;
        }
        let candidate = work.path().join("artifact");
        host.fetch(&artifact.url, &candidate, artifact.limit)?;
        verify_file(&candidate, artifact)?;
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644))?;
        fs::rename(&candidate, &destination)
            .with_context(|| format!("place verified {}", artifact.path))?;
        downloaded += 1;
    }
    write_atomic(
        &stage.join("requirements.lock"),
        LOCK.as_bytes(),
        0o644,
        None,
    )?;
    write_atomic(&stage.join("models.sha256"), MODELS.as_bytes(), 0o644, None)?;
    Ok(downloaded)
}

/// `true` when an already staged file matches its pin; a differing regular
/// file is re-downloaded, anything unsafe is refused.
fn staged(layout: &Layout, path: &Path, artifact: &Artifact) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("inspect staged Laya artifact"),
    };
    ensure!(
        metadata.is_file() && metadata.uid() == layout.owner && metadata.mode() & 0o022 == 0,
        "staged Laya artifact is unsafe: {}",
        path.display()
    );
    Ok(verify_file(path, artifact).is_ok())
}

fn verify_file(path: &Path, artifact: &Artifact) -> Result<()> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open {}", artifact.path))?;
    ensure!(
        file.metadata()?.is_file(),
        "{} is not a regular file",
        artifact.path
    );
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let mut size = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        ensure!(
            size <= artifact.limit,
            "{} exceeds its size bound",
            artifact.path
        );
        hasher.update(&buffer[..count]);
    }
    ensure!(
        artifact.exact_size.is_none_or(|expected| expected == size),
        "{} has an unexpected size",
        artifact.path
    );
    ensure!(
        hex::encode(hasher.finalize()) == artifact.sha256,
        "{} does not match its pinned SHA-256",
        artifact.path
    );
    Ok(())
}

/// Creates a missing directory 0755; an existing one must be a real,
/// owner-controlled directory that nobody else can write.
fn owned_directory(layout: &Layout, path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && metadata.uid() == layout.owner && metadata.mode() & 0o022 == 0,
            "Laya directory has unsafe ownership, permissions or type: {}",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new()
                .mode(0o755)
                .create(path)
                .with_context(|| format!("create {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        Err(error) => return Err(error).context("inspect Laya directory"),
    }
    Ok(())
}

/// Refuses symlinks, special files, writable entries and anything not pinned.
fn check_tree(
    layout: &Layout,
    root: &Path,
    relative: &Path,
    expected: &BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let metadata = entry.metadata()?;
        ensure!(
            expected.contains(&path)
                && (metadata.is_dir() || metadata.is_file())
                && metadata.uid() == layout.owner
                && metadata.mode() & 0o022 == 0,
            "unexpected or unsafe entry in Laya staging: {}; review and remove it",
            path.display()
        );
        if metadata.is_dir() {
            check_tree(layout, root, &path, expected)?;
        }
    }
    Ok(())
}

fn check_free(path: &Path, required: u64) -> Result<()> {
    // The first existing ancestor carries the file system that will be used.
    let existing = path
        .ancestors()
        .find(|candidate| candidate.exists())
        .context("no existing ancestor")?;
    let name = std::ffi::CString::new(existing.as_os_str().as_encoded_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `name` is NUL-terminated and `stats` is a valid out pointer.
    ensure!(
        unsafe { libc::statvfs(name.as_ptr(), stats.as_mut_ptr()) } == 0,
        "could not inspect free disk space"
    );
    // SAFETY: statvfs succeeded and initialized the structure.
    let stats = unsafe { stats.assume_init() };
    let free = stats.f_bavail.saturating_mul(stats.f_frsize);
    ensure!(
        free >= required,
        "{} has {} MiB free; Laya needs at least {} MiB",
        existing.display(),
        free >> 20,
        required >> 20
    );
    Ok(())
}

fn tree_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| tree_bytes(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

/// Core's protected configuration: a root-owned 0640 regular file.
struct CoreEnv {
    bytes: Vec<u8>,
    gid: u32,
}

fn read_env(layout: &Layout) -> Result<CoreEnv> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&layout.core_env)
        .context("open protected Core configuration")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == layout.owner
            && metadata.mode() & 0o777 == 0o640
            && metadata.len() <= 64 * 1024,
        "protected Core configuration has unsafe ownership, permissions or size"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(CoreEnv {
        bytes,
        gid: metadata.gid(),
    })
}

const MODE_KEY: &[u8] = b"JARVIS_LAYA_MODE=";

fn mode_in(bytes: &[u8]) -> Result<LayaMode> {
    let mut values = bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.strip_prefix(MODE_KEY));
    let value = values.next();
    ensure!(
        values.next().is_none(),
        "protected Core configuration contains duplicate JARVIS_LAYA_MODE"
    );
    match value {
        None | Some(b"off") => Ok(LayaMode::Off),
        Some(b"shadow") => Ok(LayaMode::Shadow),
        Some(b"primary") => Ok(LayaMode::Primary),
        Some(_) => bail!("protected Core configuration has an unsupported JARVIS_LAYA_MODE"),
    }
}

fn with_mode(bytes: &[u8], mode: LayaMode) -> Vec<u8> {
    let line = [MODE_KEY, mode.name().as_bytes()].concat();
    let mut output = Vec::with_capacity(bytes.len() + line.len() + 1);
    let mut replaced = false;
    for (index, part) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            output.push(b'\n');
        }
        if part.starts_with(MODE_KEY) {
            output.extend_from_slice(&line);
            replaced = true;
        } else {
            output.extend_from_slice(part);
        }
    }
    if !replaced {
        if !output.is_empty() && !output.ends_with(b"\n") {
            output.push(b'\n');
        }
        output.extend_from_slice(&line);
        output.push(b'\n');
    }
    output
}

fn read_mode(layout: &Layout) -> Result<LayaMode> {
    mode_in(&read_env(layout)?.bytes)
}

/// Atomically replaces JARVIS_LAYA_MODE, restarts Core and waits for
/// readiness; on failure the previous bytes are restored and Core restarted.
/// Returns whether anything changed.
fn set_mode(layout: &Layout, host: &dyn Host, mode: LayaMode) -> Result<bool> {
    let original = read_env(layout)?;
    if mode_in(&original.bytes)? == mode {
        return Ok(false);
    }
    let ownership = Some((layout.owner, original.gid));
    write_atomic(
        &layout.core_env,
        &with_mode(&original.bytes, mode),
        0o640,
        ownership,
    )?;
    if let Err(error) = host.restart_core() {
        write_atomic(&layout.core_env, &original.bytes, 0o640, ownership)
            .context("Core is not ready and the previous configuration could not be restored")?;
        let recovered = host.restart_core();
        return Err(error.context(if recovered.is_ok() {
            format!(
                "Core did not become ready with Laya mode {}; previous mode restored",
                mode.name()
            )
        } else {
            "Core did not become ready; previous mode restored but Core is still not ready \
             (run: sudo jarvis health)"
                .to_owned()
        }));
    }
    Ok(true)
}

/// Same-directory temporary file, fsync, rename and directory fsync.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32, owner: Option<(u32, u32)>) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    let mut file = tempfile::Builder::new()
        .prefix(".laya.")
        .tempfile_in(parent)
        .with_context(|| format!("create temporary file beside {}", path.display()))?;
    if let Some((uid, gid)) = owner {
        fchown(file.as_file(), Some(uid), Some(gid))?;
    }
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace {}", path.display()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn status(layout: &Layout, host: &dyn Host) -> Result<LayaStatus> {
    let last_probe = fs::symlink_metadata(layout.probe_record())
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= 1024)
        .and_then(|_| fs::read(layout.probe_record()).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let wheel_bytes: u64 = wheel_artifacts(LOCK)?
        .iter()
        .filter_map(|artifact| artifact.exact_size)
        .sum();
    Ok(LayaStatus {
        installed: provisioned(layout).unwrap_or(false),
        laya_version: LAYA_VERSION,
        model_revision: REVISION,
        socket_enabled: host.unit_state("is-enabled", SOCKET),
        socket_active: host.unit_state("is-active", SOCKET),
        service_active: host.unit_state("is-active", SERVICE),
        mode: read_mode(layout).map_or_else(|_| "unreadable".into(), |mode| mode.name().into()),
        last_probe,
        download_bytes: wheel_bytes + MODEL_BYTES,
        disk_bytes: [&layout.cache, &layout.runtime, &layout.models]
            .into_iter()
            .map(|path| tree_bytes(path))
            .sum(),
    })
}

fn print_status(status: &LayaStatus) {
    println!("Laya (local System-1 classifier)");
    println!(
        "Installed       {}",
        if status.installed {
            format!(
                "yes (laya {}, model {})",
                status.laya_version,
                &status.model_revision[..12]
            )
        } else {
            "no".into()
        }
    );
    println!(
        "Socket          {} / {}",
        status.socket_enabled, status.socket_active
    );
    println!("Service         {}", status.service_active);
    println!("Core mode       {}", status.mode);
    println!(
        "Last probe      {}",
        status.last_probe.as_ref().map_or_else(
            || "never".into(),
            |probe| format!(
                "{} at {} (unix time)",
                if probe.ok { "healthy" } else { "failed" },
                probe.at
            )
        )
    );
    println!("Disk used       {} MiB", status.disk_bytes >> 20);
    println!(
        "Install size    {} MiB download",
        status.download_bytes >> 20
    );
}

struct System;

impl System {
    /// Bounded stdout of a fixed root-run program with a cleared environment.
    fn output(program: &str, args: &[&str], limit: u64) -> Result<(bool, Vec<u8>)> {
        let mut child = trusted_command(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start {program}"))?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .context("missing stdout")?
            .take(limit)
            .read_to_end(&mut bytes)?;
        Ok((child.wait()?.success(), bytes))
    }
}

impl Host for System {
    fn preflight(&self) -> Result<()> {
        validate_system_executable(PYTHON)?;
        let (ok, _) = Self::output(
            PYTHON,
            &[
                "-I",
                "-c",
                "import ensurepip, platform, sys\n\
                 raise SystemExit(sys.version_info[:2] != (3, 14) or platform.machine() != 'x86_64')",
            ],
            0,
        )?;
        ensure!(
            ok,
            "the pinned wheels need CPython 3.14 on x86_64 with ensurepip; \
             install the distribution's python3.14-venv package first"
        );
        for tool in ["/usr/bin/unshare", "/usr/sbin/runuser"] {
            validate_system_executable(tool)?;
        }
        Ok(())
    }

    fn fetch(&self, url: &str, output: &Path, limit: u64) -> Result<()> {
        validate_system_executable(CURL)?;
        // `-q` (first) ignores any curlrc. Hugging Face serves weights via an
        // HTTPS redirect; every byte is hash-verified after the download.
        let status = trusted_command(CURL)
            .args(["-q", "--fail", "--silent", "--show-error", "--location"])
            .args(["--proto", "=https", "--proto-redir", "=https", "--tlsv1.2"])
            .args([
                "--max-redirs",
                "5",
                "--connect-timeout",
                "20",
                "--max-time",
                "1800",
            ])
            .args(["--max-filesize", &limit.to_string(), "--output"])
            .arg(output)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .context("start Laya download")?;
        ensure!(status.success(), "download failed: {url}");
        Ok(())
    }

    fn provision(&self) -> Result<()> {
        // The provisioner of the active verified release, resolved through
        // the same ownership and checksum-bound trust path as admin helpers.
        let status = trusted_admin_helper_command(AdminHelper::LayaProvisioner)?
            .stdin(Stdio::null())
            .status()
            .context("start provision-laya")?;
        ensure!(status.success(), "provision-laya failed with {status}");
        Ok(())
    }

    fn systemctl(&self, args: &[&str]) -> Result<()> {
        let status = trusted_command("systemctl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .context("start systemctl")?;
        ensure!(status.success(), "systemctl {} failed", args.join(" "));
        Ok(())
    }

    fn unit_state(&self, query: &str, unit: &str) -> String {
        // is-enabled/is-active print the state even with a non-zero exit.
        Self::output("systemctl", &[query, unit], 64)
            .ok()
            .and_then(|(_, bytes)| String::from_utf8(bytes).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| {
                (1..=32).contains(&value.len())
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            })
            .unwrap_or_else(|| "unknown".into())
    }

    fn restart_core(&self) -> Result<()> {
        self.systemctl(&["restart", "jarvis-core.service"])?;
        validate_system_executable(CURL)?;
        for _ in 0..30 {
            let ready = Self::output(
                CURL,
                &[
                    "-q",
                    "--fail",
                    "--silent",
                    "--output",
                    "/dev/null",
                    "--max-time",
                    "2",
                    "http://127.0.0.1:8080/readyz",
                ],
                0,
            )
            .is_ok_and(|(ok, _)| ok);
            if ready {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(2));
        }
        bail!("Core did not report /readyz within 60 seconds")
    }

    fn probe(&self) -> Result<()> {
        validate_system_executable(CURL)?;
        // A socket-activated first request loads both checkpoints.
        let (ok, body) = Self::output(
            CURL,
            &[
                "-q",
                "--fail",
                "--silent",
                "--max-time",
                "180",
                "--unix-socket",
                "/run/jarvis-laya.sock",
                "http://jarvis-laya.local/health",
            ],
            4096,
        )?;
        ensure!(ok, "Laya did not answer its health endpoint");
        healthy(&body)
    }

    fn audit(&self, record: &str) -> Result<()> {
        validate_system_executable(LOGGER)?;
        let status = trusted_command(LOGGER)
            .args([
                "--tag",
                "jarvis-laya",
                "--priority",
                "authpriv.notice",
                "--",
                record,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        ensure!(status.success(), "Laya audit event could not be recorded");
        Ok(())
    }
}

/// Laya's `/health` must report both pinned checkpoints loaded.
fn healthy(body: &[u8]) -> Result<()> {
    #[derive(Deserialize)]
    struct Health {
        status: String,
        loaded: Vec<String>,
    }
    let health: Health = serde_json::from_slice(body).context("Laya health is malformed")?;
    ensure!(
        health.status == "ok"
            && ["english", "multilingual"]
                .iter()
                .all(|name| health.loaded.iter().any(|loaded| loaded == name)),
        "Laya is not serving both pinned checkpoints"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::{BTreeMap, VecDeque},
        os::unix::fs::symlink,
    };

    const ENV: &str =
        "JARVIS_ENVIRONMENT=production\nJARVIS_LAYA_MODE=off\nJARVIS_SURREAL_PASSWORD=fixture\n";

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        downloads: BTreeMap<String, Vec<u8>>,
        socket_enabled: Cell<bool>,
        restarts: RefCell<VecDeque<bool>>,
        probe_fails: Cell<bool>,
        audit_fails: Cell<bool>,
        installs: Option<(PathBuf, PathBuf)>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
        fn record(&self, call: String) {
            self.calls.borrow_mut().push(call);
        }
    }

    impl Host for Fake {
        fn preflight(&self) -> Result<()> {
            self.record("preflight".into());
            Ok(())
        }
        fn fetch(&self, url: &str, output: &Path, _limit: u64) -> Result<()> {
            self.record(format!("fetch {url}"));
            fs::write(output, self.downloads.get(url).context("unknown url")?)?;
            Ok(())
        }
        fn provision(&self) -> Result<()> {
            self.record("provision".into());
            let (runtime, models) = self.installs.as_ref().context("no provisioner")?;
            let release = format!("laya-{LAYA_VERSION}-{REVISION}");
            fs::create_dir_all(runtime.join("releases").join(&release).join("bin"))?;
            fs::write(
                runtime.join("releases").join(&release).join("bin/python"),
                "",
            )?;
            symlink(
                Path::new("releases").join(&release),
                runtime.join("current"),
            )?;
            fs::create_dir_all(models.join(REVISION))?;
            fs::write(models.join(REVISION).join("models.sha256"), MODELS)?;
            Ok(())
        }
        fn systemctl(&self, args: &[&str]) -> Result<()> {
            self.record(format!("systemctl {}", args.join(" ")));
            match args.first() {
                Some(&"enable") => self.socket_enabled.set(true),
                Some(&"disable") => self.socket_enabled.set(false),
                _ => {}
            }
            Ok(())
        }
        fn unit_state(&self, _query: &str, _unit: &str) -> String {
            if self.socket_enabled.get() {
                "enabled"
            } else {
                "disabled"
            }
            .into()
        }
        fn restart_core(&self) -> Result<()> {
            self.record("restart".into());
            ensure!(
                self.restarts.borrow_mut().pop_front().unwrap_or(true),
                "not ready"
            );
            Ok(())
        }
        fn probe(&self) -> Result<()> {
            self.record("probe".into());
            ensure!(!self.probe_fails.get(), "unhealthy");
            Ok(())
        }
        fn audit(&self, record: &str) -> Result<()> {
            ensure!(!self.audit_fails.get(), "logger failed");
            self.record(format!("audit {record}"));
            Ok(())
        }
    }

    fn fixture() -> (tempfile::TempDir, Layout) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join("etc")).unwrap();
        let layout = Layout {
            cache: root.join("cache"),
            runtime: root.join("laya"),
            models: root.join("models"),
            core_env: root.join("etc/core.env"),
            // SAFETY: geteuid has no preconditions.
            owner: unsafe { libc::geteuid() },
            required_free: 0,
        };
        write_env(&layout, ENV);
        (directory, layout)
    }

    fn write_env(layout: &Layout, contents: &str) {
        fs::write(&layout.core_env, contents).unwrap();
        fs::set_permissions(&layout.core_env, fs::Permissions::from_mode(0o640)).unwrap();
    }

    fn env(layout: &Layout) -> String {
        fs::read_to_string(&layout.core_env).unwrap()
    }

    fn installed_fake(layout: &Layout) -> Fake {
        Fake {
            installs: Some((layout.runtime.clone(), layout.models.clone())),
            ..Fake::default()
        }
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn committed_pins_are_complete_and_match_the_provisioner() {
        let wheels = wheel_artifacts(LOCK).unwrap();
        let models = model_artifacts(MODELS).unwrap();
        assert_eq!((wheels.len(), models.len()), (44, 10));
        let laya = wheels
            .iter()
            .find(|wheel| wheel.path == "wheels/laya-0.3.20-py3-none-any.whl")
            .unwrap();
        let provisioner = include_str!("../../../deploy/systemd/provision-laya.sh");
        assert!(provisioner.contains(&format!("readonly revision={REVISION}\n")));
        assert!(provisioner.contains(&format!("readonly wheel_sha={}\n", laya.sha256)));
        assert!(include_str!("../../../deploy/systemd/laya-offline.py").contains(REVISION));
        assert!(LOCK.contains("\nlaya[serve]==0.3.20 \\\n"));
        let torch = wheels
            .iter()
            .find(|wheel| wheel.path.starts_with("wheels/torch-"))
            .unwrap();
        assert!(torch
            .url
            .starts_with("https://download.pytorch.org/whl/cpu/torch-2.14.1%2Bcpu-cp314"));
        assert!(wheels.iter().all(|wheel| !wheel.path.contains("nvidia")));
        let wheel_bytes: u64 = wheels.iter().filter_map(|wheel| wheel.exact_size).sum();
        assert!(wheel_bytes < 300 << 20, "{wheel_bytes}");
        for checkpoint in ["model.safetensors", "multilingual/model.safetensors"] {
            assert!(models
                .iter()
                .any(|model| model.path == format!("models/{REVISION}/{checkpoint}")));
        }
        assert!(models
            .iter()
            .all(|model| !model.path.contains("typed-decisions")
                && model.url == format!("{MODEL_SOURCE}/{}", &model.path["models/".len()..])));
    }

    #[test]
    fn lock_and_manifest_parsers_fail_closed() {
        let hash = "a".repeat(64);
        let pypi = "https://files.pythonhosted.org/packages/aa/bb/cc";
        let valid =
            format!("# {pypi}/x-1-py3-none-any.whl 10\nx==1 \\\n    --hash=sha256:{hash}\n");
        assert_eq!(wheel_artifacts(&valid).unwrap().len(), 1);
        for lock in [
            valid.replace("https://", "http://"),
            valid.replace("files.pythonhosted.org", "evil.example"),
            valid.replace("py3-none-any.whl", "tar.gz"),
            valid.replace(" 10\n", " 0\n"),
            valid.replace(" 10\n", &format!(" {}\n", ARTIFACT_LIMIT + 1)),
            valid.replace(&hash, "A".repeat(64).as_str()),
            valid.replace("x==1 \\\n", "x>=1 \\\n"),
            format!("{valid}    --hash=sha256:{hash}\n"),
            format!("{valid}{valid}"),
            format!("--index-url https://evil.example/simple\n{valid}"),
            format!("# {pypi}/x-1-py3-none-any.whl 10\nx==1 \\\n"),
            format!("# {pypi}/../x-1-py3-none-any.whl 10\nx==1 \\\n    --hash=sha256:{hash}\n")
                .replace("/../x", "/x?y=1&x"),
            String::new(),
        ] {
            assert!(wheel_artifacts(&lock).is_err(), "{lock}");
        }
        assert!(model_artifacts(&format!("{hash}  multilingual/config.json\n")).is_ok());
        for manifest in [
            format!("{hash}  ../config.json"),
            format!("{hash}  /config.json"),
            format!("{hash}  model.bin"),
            format!("{hash}  .cache/config.json"),
            format!("{hash}  a//config.json"),
            format!("{hash} config.json"),
            format!("{}  config.json", &hash[1..]),
            format!("{hash}  config.json\n{hash}  config.json"),
            String::new(),
        ] {
            assert!(model_artifacts(&manifest).is_err(), "{manifest}");
        }
    }

    fn small_artifacts(fake: &mut Fake) -> Vec<Artifact> {
        let wheel = b"wheel bytes".to_vec();
        let model = b"{\"model\": true}".to_vec();
        fake.downloads.insert("https://w".into(), wheel.clone());
        fake.downloads.insert("https://m".into(), model.clone());
        vec![
            Artifact {
                url: "https://w".into(),
                path: "wheels/w-1-py3-none-any.whl".into(),
                sha256: digest(&wheel),
                limit: wheel.len() as u64,
                exact_size: Some(wheel.len() as u64),
            },
            Artifact {
                url: "https://m".into(),
                path: format!("models/{REVISION}/multilingual/config.json"),
                sha256: digest(&model),
                limit: ARTIFACT_LIMIT,
                exact_size: None,
            },
        ]
    }

    #[test]
    fn staging_verifies_resumes_and_refuses_tampering() {
        let (_directory, layout) = fixture();
        let mut fake = Fake::default();
        let artifacts = small_artifacts(&mut fake);
        let stage = layout.stage();
        let wheel = stage.join(&artifacts[0].path);

        assert_eq!(stage_artifacts(&layout, &fake, &artifacts).unwrap(), 2);
        assert_eq!(
            fs::read_to_string(stage.join("requirements.lock")).unwrap(),
            LOCK
        );
        assert_eq!(
            fs::read_to_string(stage.join("models.sha256")).unwrap(),
            MODELS
        );
        assert_eq!(fs::metadata(&wheel).unwrap().mode() & 0o777, 0o644);
        assert_eq!(
            stage_artifacts(&layout, &fake, &artifacts).unwrap(),
            0,
            "resumes"
        );

        fs::write(&wheel, b"tampered!!!").unwrap();
        assert_eq!(stage_artifacts(&layout, &fake, &artifacts).unwrap(), 1);
        assert_eq!(fs::read(&wheel).unwrap(), b"wheel bytes");

        // Tampered and oversize downloads are never placed.
        fs::remove_file(&wheel).unwrap();
        fake.downloads
            .insert("https://w".into(), b"evil bytes!".to_vec());
        let error = stage_artifacts(&layout, &fake, &artifacts).unwrap_err();
        assert!(format!("{error:#}").contains("SHA-256"), "{error:#}");
        assert!(!wheel.exists());
        fake.downloads
            .insert("https://w".into(), b"wheel bytes and more".to_vec());
        let error = stage_artifacts(&layout, &fake, &artifacts).unwrap_err();
        assert!(format!("{error:#}").contains("size bound"), "{error:#}");
        assert!(!wheel.exists());
        fake.downloads
            .insert("https://w".into(), b"wheel bytes".to_vec());
        assert_eq!(
            fs::read_dir(&layout.cache).unwrap().count(),
            1,
            "work directory removed"
        );

        // Unpinned, linked or writable entries stop staging before any fetch.
        let calls = fake.calls().len();
        fs::write(stage.join("wheels/extra-1-py3-none-any.whl"), b"x").unwrap();
        assert!(stage_artifacts(&layout, &fake, &artifacts).is_err());
        fs::remove_file(stage.join("wheels/extra-1-py3-none-any.whl")).unwrap();
        symlink("/etc/passwd", &wheel).unwrap();
        assert!(stage_artifacts(&layout, &fake, &artifacts).is_err());
        fs::remove_file(&wheel).unwrap();
        fs::set_permissions(stage.join("wheels"), fs::Permissions::from_mode(0o775)).unwrap();
        assert!(stage_artifacts(&layout, &fake, &artifacts).is_err());
        assert_eq!(fake.calls().len(), calls);
        fs::set_permissions(stage.join("wheels"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(stage_artifacts(&layout, &fake, &artifacts).unwrap(), 1);
    }

    #[test]
    fn mode_edit_is_atomic_and_rolls_back_on_failed_core_restart() {
        let (_directory, layout) = fixture();
        let fake = Fake::default();
        assert!(set_mode(&layout, &fake, LayaMode::Shadow).unwrap());
        assert_eq!(env(&layout), ENV.replace("MODE=off", "MODE=shadow"));
        assert_eq!(
            fs::metadata(&layout.core_env).unwrap().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::read_dir(layout.core_env.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        assert!(
            !set_mode(&layout, &fake, LayaMode::Shadow).unwrap(),
            "unchanged"
        );
        assert_eq!(fake.calls(), ["restart"]);

        fake.restarts.borrow_mut().extend([false, true]);
        let error = set_mode(&layout, &fake, LayaMode::Primary).unwrap_err();
        assert!(
            format!("{error:#}").contains("previous mode restored"),
            "{error:#}"
        );
        assert_eq!(env(&layout), ENV.replace("MODE=off", "MODE=shadow"));
        assert_eq!(fake.calls(), ["restart", "restart", "restart"]);
        assert_eq!(
            fs::read_dir(layout.core_env.parent().unwrap())
                .unwrap()
                .count(),
            1
        );

        write_env(&layout, "JARVIS_A=1");
        assert!(set_mode(&layout, &fake, LayaMode::Shadow).unwrap());
        assert_eq!(env(&layout), "JARVIS_A=1\nJARVIS_LAYA_MODE=shadow\n");

        for unsafe_env in [
            "JARVIS_LAYA_MODE=off\nJARVIS_LAYA_MODE=shadow\n",
            "JARVIS_LAYA_MODE=on\n",
        ] {
            write_env(&layout, unsafe_env);
            assert!(set_mode(&layout, &fake, LayaMode::Primary).is_err());
            assert_eq!(env(&layout), unsafe_env);
        }
        write_env(&layout, ENV);
        fs::set_permissions(&layout.core_env, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(set_mode(&layout, &fake, LayaMode::Shadow).is_err());
        fs::rename(&layout.core_env, layout.cache.with_file_name("real.env")).unwrap();
        symlink(layout.cache.with_file_name("real.env"), &layout.core_env).unwrap();
        assert!(set_mode(&layout, &fake, LayaMode::Shadow).is_err());
    }

    #[test]
    fn disable_turns_core_off_before_stopping_units() {
        let (_directory, layout) = fixture();
        write_env(&layout, &ENV.replace("MODE=off", "MODE=primary"));
        let fake = Fake::default();
        fake.socket_enabled.set(true);
        fake.restarts.borrow_mut().extend([false, true]);
        assert!(disable(&layout, &fake).is_err());
        assert!(
            fake.socket_enabled.get(),
            "units untouched when Core is not ready"
        );
        assert!(env(&layout).contains("MODE=primary"));

        disable(&layout, &fake).unwrap();
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Off);
        assert_eq!(
            fake.calls()[2..],
            [
                "restart",
                "systemctl disable --now jarvis-laya.service jarvis-laya.socket"
            ]
        );
    }

    #[test]
    fn enable_requires_install_probes_and_defaults_to_shadow() {
        let (_directory, layout) = fixture();
        let fake = installed_fake(&layout);
        assert!(enable(&layout, &fake).is_err());
        assert!(fake.calls().is_empty());

        fake.provision().unwrap();
        fake.probe_fails.set(true);
        assert!(enable(&layout, &fake).is_err());
        assert!(
            !fake.socket_enabled.get(),
            "a failed probe turns Laya off again"
        );
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Off);

        fake.probe_fails.set(false);
        enable(&layout, &fake).unwrap();
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Shadow);
        assert!(fake.socket_enabled.get());
        let record: ProbeRecord =
            serde_json::from_slice(&fs::read(layout.probe_record()).unwrap()).unwrap();
        assert!(record.ok);
        write_env(&layout, &ENV.replace("MODE=off", "MODE=primary"));
        enable(&layout, &fake).unwrap();
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Primary, "kept");
    }

    #[test]
    fn mode_requires_enabled_socket_and_healthy_laya() {
        let (_directory, layout) = fixture();
        let fake = Fake::default();
        assert!(set_mode_checked(&layout, &fake, LayaMode::Primary).is_err());
        fake.socket_enabled.set(true);
        fake.probe_fails.set(true);
        assert!(set_mode_checked(&layout, &fake, LayaMode::Shadow).is_err());
        assert_eq!(fake.calls(), ["probe"]);
        assert_eq!(env(&layout), ENV);
        fake.probe_fails.set(false);
        set_mode_checked(&layout, &fake, LayaMode::Primary).unwrap();
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Primary);
        fake.socket_enabled.set(false);
        set_mode_checked(&layout, &fake, LayaMode::Off).unwrap();
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Off);
    }

    #[test]
    fn install_is_idempotent_and_audited() {
        let (_directory, layout) = fixture();
        let fake = installed_fake(&layout);
        fake.provision().unwrap();
        fake.audit_fails.set(true);
        assert!(execute(&layout, &fake, LayaCommand::Install).is_err());
        assert_eq!(
            fake.calls(),
            ["provision"],
            "nothing changes without an audit trail"
        );

        fake.audit_fails.set(false);
        execute(&layout, &fake, LayaCommand::Install).unwrap();
        let calls = fake.calls();
        assert_eq!(calls[1], "audit action=install outcome=initiated");
        assert_eq!(
            calls.last().unwrap(),
            "audit action=install outcome=succeeded"
        );
        assert_eq!(calls.iter().filter(|call| *call == "provision").count(), 1);
        assert!(!calls.iter().any(|call| call.starts_with("fetch")));
        assert_eq!(read_mode(&layout).unwrap(), LayaMode::Shadow);
        assert!(provisioned(&layout).unwrap());
        let status = status(&layout, &fake).unwrap();
        assert!(status.installed && status.mode == "shadow");
        assert!(status.download_bytes > MODEL_BYTES);
    }

    #[test]
    fn health_requires_both_checkpoints() {
        assert!(
            healthy(br#"{"status":"ok","loaded":["english","multilingual"],"device":"auto"}"#)
                .is_ok()
        );
        assert!(healthy(br#"{"status":"ok","loaded":["english"]}"#).is_err());
        assert!(healthy(br#"{"status":"loading","loaded":["english","multilingual"]}"#).is_err());
        assert!(healthy(b"not json").is_err());
    }
}
