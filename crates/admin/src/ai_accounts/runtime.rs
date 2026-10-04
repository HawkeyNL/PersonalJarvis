//! Owner-triggered install of the official Claude Code CLI runtime.
//!
//! Integrity comes from Anthropic's detached signature over the release
//! manifest, checked against a release key pinned in this binary, plus the
//! manifest's SHA-256 for this platform. The candidate is never executed here:
//! the hardened, unprivileged status/connect/worker paths check its version
//! before any use, and a binary that misbehaves is recoverable by rollback.
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{fchown, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    audit_record, bounded_status_output, validate_root_executable, validate_system_executable,
    worker_command, AccountProvider,
};

const CURL: &str = "/usr/bin/curl";
const GPG: &str = "/usr/bin/gpg";
const GPGV: &str = "/usr/bin/gpgv";
const LOCK: &str = "/run/jarvis-provider-runtime.lock";
const POINTER_LIMIT: u64 = 64;
const MANIFEST_LIMIT: u64 = 1024 * 1024;
const SIGNATURE_LIMIT: u64 = 64 * 1024;
const BINARY_LIMIT: u64 = 400 * 1024 * 1024;

const CLAUDE_RELEASE_KEY: &str = include_str!("../../../../deploy/keys/claude-code-release.asc");

/// Fixed install locations and trust anchors. Only tests construct another
/// layout; no runtime input can change the production source or destination.
struct Layout<'a> {
    base_url: &'a str,
    /// curl `--proto` value; production accepts HTTPS only.
    protocol: &'a str,
    key: &'a str,
    fingerprint: &'a str,
    target: &'a str,
    previous: &'a str,
    /// Ancestors at or below this directory must be owned and not writable by
    /// others. Production checks every ancestor up to `/`.
    trusted_root: &'a str,
    owner: (u32, u32),
}

const PRODUCTION: Layout<'static> = Layout {
    base_url: "https://downloads.claude.ai/claude-code-releases",
    protocol: "=https",
    key: CLAUDE_RELEASE_KEY,
    fingerprint: "31DDDE24DDFAB679F42D7BD2BAA929FF1A7ECACE",
    target: "/usr/local/bin/claude",
    previous: "/usr/local/lib/jarvis/claude.previous",
    trusted_root: "/",
    owner: (0, 0),
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum RuntimeProvider {
    Claude,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum Channel {
    Stable,
    Latest,
}

impl Channel {
    fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Latest => "latest",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum RuntimeCommand {
    /// Read-only: installed version, version gate, ownership and channel availability.
    Status { provider: RuntimeProvider },
    /// Download, verify (pinned release key and checksum) and atomically install.
    Install {
        provider: RuntimeProvider,
        #[arg(long, value_enum, conflicts_with = "version")]
        channel: Option<Channel>,
        #[arg(long)]
        version: Option<String>,
    },
    /// Restore the runtime that the last install replaced.
    Rollback { provider: RuntimeProvider },
}

enum VersionRequest {
    Channel(Channel),
    Exact(String),
}

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    provider: &'static str,
    installed: bool,
    version: Option<String>,
    gate_ok: bool,
    safe_ownership: bool,
    latest_stable: Option<String>,
    update_available: bool,
    rollback_available: bool,
}

pub(in crate::ai_accounts) fn run(command: RuntimeCommand, json: bool) -> Result<()> {
    match command {
        RuntimeCommand::Status {
            provider: RuntimeProvider::Claude,
        } => {
            let status = status();
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                println!(
                    "claude runtime: installed={} version={} gate_ok={} safe_ownership={} latest_stable={} update_available={} rollback_available={}",
                    status.installed,
                    status.version.as_deref().unwrap_or("unknown"),
                    status.gate_ok,
                    status.safe_ownership,
                    status.latest_stable.as_deref().unwrap_or("unknown"),
                    status.update_available,
                    status.rollback_available
                );
            }
            Ok(())
        }
        RuntimeCommand::Install {
            provider: RuntimeProvider::Claude,
            channel,
            version,
        } => {
            let request = match version {
                Some(version) => VersionRequest::Exact(version),
                None => VersionRequest::Channel(channel.unwrap_or(Channel::Stable)),
            };
            // Only a strict version or a fixed channel name enters the audit log.
            let label = match &request {
                VersionRequest::Exact(version) if strict_version(version).is_some() => {
                    version.clone()
                }
                VersionRequest::Exact(_) => "invalid".to_owned(),
                VersionRequest::Channel(channel) => channel.name().to_owned(),
            };
            audited("runtime-install", &label, || {
                let _lock = crate::mutation_lock(LOCK)?;
                let version = install(&PRODUCTION, &request, platform()?)?;
                println!(
                    "claude runtime {version} is installed and verified (signed manifest, SHA-256)"
                );
                Ok(version)
            })
        }
        RuntimeCommand::Rollback {
            provider: RuntimeProvider::Claude,
        } => audited("runtime-rollback", "previous", || {
            let _lock = crate::mutation_lock(LOCK)?;
            rollback(&PRODUCTION)?;
            println!("claude runtime restored from the previous install");
            Ok("previous".to_owned())
        }),
    }
}

fn audited(action: &str, label: &str, operation: impl FnOnce() -> Result<String>) -> Result<()> {
    let record = |version: &str, outcome: &str| {
        audit_record(&format!(
            "provider=claude action={action} version={version} outcome={outcome}"
        ))
    };
    record(label, "initiated")?;
    let result = operation();
    match &result {
        Ok(version) => record(version, "succeeded"),
        Err(_) => record(label, "failed"),
    }
    .context("runtime action may have completed but its audit event failed")?;
    result.map(|_| ())
}

fn status() -> RuntimeStatus {
    let layout = &PRODUCTION;
    let installed = fs::symlink_metadata(layout.target).is_ok();
    let safe_ownership = validate_root_executable(layout.target).is_ok();
    // Executed only as the dedicated identity in the hardened transient service.
    let version = safe_ownership
        .then(|| worker_command(AccountProvider::Claude, &["--version"], false).ok())
        .flatten()
        .and_then(|mut command| bounded_status_output(&mut command).ok().flatten())
        .and_then(|output| reported_version(&output));
    let gate_ok = version.as_deref().is_some_and(reviewed);
    let latest_stable = tempfile::tempdir().ok().and_then(|dir| {
        let pointer = dir.path().join("stable");
        fetch(layout, "stable", &pointer, POINTER_LIMIT, 10).ok()?;
        pointer_version(&fs::read(pointer).ok()?)
    });
    let update_available = latest_stable.as_deref().is_some_and(|latest| {
        reviewed(latest)
            && version
                .as_deref()
                .and_then(strict_version)
                .is_none_or(|installed| strict_version(latest) > Some(installed))
    });
    let rollback_available = owned_regular_file(layout, Path::new(layout.previous)).is_ok();
    RuntimeStatus {
        provider: "claude",
        installed,
        version,
        gate_ok,
        safe_ownership,
        latest_stable,
        update_available,
        rollback_available,
    }
}

fn install(layout: &Layout, request: &VersionRequest, platform: &str) -> Result<String> {
    check_destination(layout)?;
    // Never `$TMPDIR`: the work directory lives in the root-only rollback
    // directory so no other user can swap verified files by path.
    let work = tempfile::Builder::new()
        .prefix("jarvis-runtime.")
        .tempdir_in(rollback_directory(layout)?)
        .context("create private runtime work directory")?;
    fs::set_permissions(work.path(), fs::Permissions::from_mode(0o700))?;
    let version = match request {
        VersionRequest::Exact(version) => version.clone(),
        VersionRequest::Channel(channel) => {
            let pointer = work.path().join("pointer");
            fetch(layout, channel.name(), &pointer, POINTER_LIMIT, 30)?;
            pointer_version(&fs::read(&pointer)?)
                .context("release channel did not name a strict version")?
        }
    };
    if strict_version(&version).is_none() {
        bail!("runtime version must be a strict MAJOR.MINOR.PATCH version");
    }
    if !reviewed(&version) {
        bail!("Claude Code {version} is outside the reviewed 2.1.248+ (2.1 line) contract");
    }
    let manifest = work.path().join("manifest.json");
    let signature = work.path().join("manifest.json.sig");
    fetch(
        layout,
        &format!("{version}/manifest.json"),
        &manifest,
        MANIFEST_LIMIT,
        60,
    )?;
    fetch(
        layout,
        &format!("{version}/manifest.json.sig"),
        &signature,
        SIGNATURE_LIMIT,
        60,
    )?;
    verify_signature(layout, work.path(), &manifest, &signature)?;
    let (checksum, size) = manifest_entry(&fs::read(&manifest)?, &version, platform)?;
    // Reinstalling the active version must not replace the rollback copy.
    if fs::symlink_metadata(layout.target).is_ok() {
        let (digest, length) = copy_hashed(
            &mut open_no_follow(Path::new(layout.target))?,
            &mut io::sink(),
        )?;
        if length == size && digest.eq_ignore_ascii_case(&checksum) {
            return Ok(version);
        }
    }
    let binary = work.path().join("claude");
    fetch(
        layout,
        &format!("{version}/{platform}/claude"),
        &binary,
        size,
        900,
    )?;
    check_destination(layout)?;
    replace_target(layout, &binary, Some((&checksum, size)))?;
    Ok(version)
}

fn rollback(layout: &Layout) -> Result<()> {
    check_destination(layout)?;
    let previous = Path::new(layout.previous);
    owned_regular_file(layout, previous).context("no previous runtime is available")?;
    remove_stale(layout)?;
    // Restored as-is: this is whatever binary the last install replaced, which
    // may predate the verified installer. Its version is gated before any use.
    replace_target(layout, previous, None)?;
    fs::remove_file(previous).context("remove restored previous runtime")?;
    sync_directory(parent(previous)?)
}

/// Stages `source` beside the target, verifies it while copying, keeps the
/// current runtime as the rollback copy and renames the stage into place.
fn replace_target(layout: &Layout, source: &Path, expected: Option<(&str, u64)>) -> Result<()> {
    let target = Path::new(layout.target);
    let directory = parent(target)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".claude.")
        .tempfile_in(directory)
        .context("stage runtime beside its destination")?;
    let (digest, length) = copy_hashed(&mut open_no_follow(source)?, staged.as_file_mut())?;
    if let Some((checksum, size)) = expected {
        if length != size || !digest.eq_ignore_ascii_case(checksum) {
            bail!("downloaded runtime does not match the signed manifest checksum");
        }
    }
    let file = staged.as_file();
    fchown(file, Some(layout.owner.0), Some(layout.owner.1))?;
    file.set_permissions(fs::Permissions::from_mode(0o755))?;
    file.sync_all()?;
    if fs::symlink_metadata(target).is_ok() && expected.is_some() {
        keep_previous(layout)?;
    }
    staged
        .persist(target)
        .map_err(|error| error.error)
        .context("activate runtime")?;
    sync_directory(directory)
}

/// The root-only (`0700`) directory holding the rollback copy and work files.
/// Leftovers of an install killed mid-way are removed here and beside the
/// target; both directories are only writable by root.
fn rollback_directory<'a>(layout: &'a Layout) -> Result<&'a Path> {
    let previous = Path::new(layout.previous);
    let directory = parent(previous)?;
    if fs::symlink_metadata(directory).is_err() {
        DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .context("create root-only rollback directory")?;
    }
    check_tree(layout, previous)?;
    remove_stale(layout)?;
    Ok(directory)
}

fn remove_stale(layout: &Layout) -> Result<()> {
    for directory in [
        parent(Path::new(layout.target))?,
        parent(Path::new(layout.previous))?,
    ] {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let kind = entry.file_type()?;
            if name.starts_with(".claude.") && kind.is_file() {
                fs::remove_file(entry.path())?;
            } else if name.starts_with("jarvis-runtime.") && kind.is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
    }
    Ok(())
}

fn keep_previous(layout: &Layout) -> Result<()> {
    let previous = Path::new(layout.previous);
    let directory = parent(previous)?;
    check_tree(layout, previous)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".claude.")
        .tempfile_in(directory)?;
    copy_hashed(
        &mut open_no_follow(Path::new(layout.target))?,
        staged.as_file_mut(),
    )?;
    let file = staged.as_file();
    fchown(file, Some(layout.owner.0), Some(layout.owner.1))?;
    file.set_permissions(fs::Permissions::from_mode(0o700))?;
    file.sync_all()?;
    staged
        .persist(previous)
        .map_err(|error| error.error)
        .context("keep previous runtime")?;
    sync_directory(directory)
}

/// Refuses a symlinked or foreign-owned target and any unsafe ancestor of the
/// target or the rollback copy before anything is downloaded or changed.
fn check_destination(layout: &Layout) -> Result<()> {
    let target = Path::new(layout.target);
    check_tree(layout, target)?;
    if fs::symlink_metadata(target).is_ok() {
        owned_regular_file(layout, target)?;
    }
    let previous = Path::new(layout.previous);
    if fs::symlink_metadata(parent(previous)?).is_ok() {
        check_tree(layout, previous)?;
    } else {
        check_tree(layout, parent(previous)?)?;
    }
    Ok(())
}

fn check_tree(layout: &Layout, path: &Path) -> Result<()> {
    for ancestor in path
        .ancestors()
        .skip(1)
        .take_while(|ancestor| ancestor.starts_with(layout.trusted_root))
    {
        let metadata = fs::symlink_metadata(ancestor).context("inspect runtime directory")?;
        if !metadata.is_dir()
            || metadata.uid() != layout.owner.0
            || metadata.permissions().mode() & 0o022 != 0
        {
            bail!("runtime directory {} is unsafe", ancestor.display());
        }
    }
    Ok(())
}

fn owned_regular_file(layout: &Layout, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).context("inspect runtime file")?;
    if !metadata.is_file()
        || metadata.uid() != layout.owner.0
        || metadata.permissions().mode() & 0o022 != 0
    {
        bail!(
            "runtime file {} has unsafe type, ownership or permissions",
            path.display()
        );
    }
    check_tree(layout, path)
}

fn fetch(layout: &Layout, path: &str, output: &Path, limit: u64, seconds: u32) -> Result<()> {
    validate_system_executable(CURL)?;
    // `-q` (first) ignores any curlrc; redirects are never followed.
    let status = Command::new(CURL)
        .args([
            "-q",
            "--fail",
            "--silent",
            "--proto",
            layout.protocol,
            "--tlsv1.2",
        ])
        .args([
            "--connect-timeout",
            "10",
            "--max-time",
            &seconds.to_string(),
        ])
        .args(["--max-filesize", &limit.to_string(), "--output"])
        .arg(output)
        .arg(format!("{}/{path}", layout.base_url))
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("start runtime download")?;
    if !status.success() {
        bail!("runtime download failed: {path}");
    }
    let metadata = fs::symlink_metadata(output).context("inspect runtime download")?;
    if !metadata.is_file() || metadata.len() > limit {
        bail!("runtime download exceeded its size bound: {path}");
    }
    Ok(())
}

fn verify_signature(layout: &Layout, work: &Path, manifest: &Path, signature: &Path) -> Result<()> {
    validate_system_executable(GPG)?;
    validate_system_executable(GPGV)?;
    let home = work.join("gnupg");
    DirBuilder::new().mode(0o700).create(&home)?;
    let armored = work.join("release-key.asc");
    let keyring = work.join("release-key.gpg");
    fs::write(&armored, layout.key)?;
    let dearmored = Command::new(GPG)
        .args(["--batch", "--no-options", "--homedir"])
        .arg(&home)
        .arg("--output")
        .arg(&keyring)
        .arg("--dearmor")
        .arg(&armored)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("start release key preparation")?;
    if !dearmored.success() {
        bail!("pinned release key could not be prepared");
    }
    let output = Command::new(GPGV)
        .arg("--homedir")
        .arg(&home)
        .args(["--status-fd", "1", "--keyring"])
        .arg(&keyring)
        .arg(signature)
        .arg(manifest)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("start release signature verification")?;
    let status = String::from_utf8_lossy(&output.stdout);
    let good = status
        .lines()
        .any(|line| line.starts_with("[GNUPG:] GOODSIG "));
    // The last VALIDSIG field is the primary key fingerprint.
    let pinned = status.lines().any(|line| {
        line.starts_with("[GNUPG:] VALIDSIG ")
            && line.split_whitespace().last() == Some(layout.fingerprint)
    });
    if !output.status.success() || !good || !pinned {
        bail!("release manifest signature is not valid for the pinned Claude Code release key");
    }
    Ok(())
}

fn manifest_entry(bytes: &[u8], version: &str, platform: &str) -> Result<(String, u64)> {
    let manifest: serde_json::Value =
        serde_json::from_slice(bytes).context("parse signed release manifest")?;
    // Binds the signature to the requested version: an older signed manifest
    // served under a newer path is refused.
    if manifest.get("version").and_then(serde_json::Value::as_str) != Some(version) {
        bail!("signed release manifest is for a different version");
    }
    let entry = manifest
        .get("platforms")
        .and_then(|platforms| platforms.get(platform))
        .context("signed release manifest has no entry for this platform")?;
    let checksum = entry
        .get("checksum")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("signed release manifest has no valid SHA-256 checksum")?;
    let size = entry
        .get("size")
        .and_then(serde_json::Value::as_u64)
        .filter(|size| (1..=BINARY_LIMIT).contains(size))
        .context("signed release manifest size is missing or exceeds the runtime bound")?;
    if entry.get("binary").and_then(serde_json::Value::as_str) != Some("claude") {
        bail!("signed release manifest names an unexpected binary");
    }
    Ok((checksum.to_owned(), size))
}

fn platform() -> Result<&'static str> {
    let (glibc, musl, platforms) = match std::env::consts::ARCH {
        "x86_64" => (
            "/lib64/ld-linux-x86-64.so.2",
            "/lib/ld-musl-x86_64.so.1",
            ("linux-x64", "linux-x64-musl"),
        ),
        "aarch64" => (
            "/lib/ld-linux-aarch64.so.1",
            "/lib/ld-musl-aarch64.so.1",
            ("linux-arm64", "linux-arm64-musl"),
        ),
        _ => bail!("this architecture has no official Claude Code runtime"),
    };
    if Path::new(glibc).exists() {
        Ok(platforms.0)
    } else if Path::new(musl).exists() {
        Ok(platforms.1)
    } else {
        bail!("could not determine the host C library for the Claude Code runtime")
    }
}

/// `MAJOR.MINOR.PATCH` with ASCII digits only and no leading zeros.
fn strict_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.').map(|part| {
        let canonical = !part.is_empty()
            && part.len() <= 9
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'));
        canonical.then(|| part.parse::<u64>().ok()).flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn pointer_version(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    strict_version(text).map(|_| text.to_owned())
}

/// The single strict version in bounded `claude --version` output.
fn reported_version(output: &str) -> Option<String> {
    let versions = output
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|candidate| strict_version(candidate).is_some())
        .collect::<Vec<_>>();
    match versions.as_slice() {
        [version] => Some((*version).to_owned()),
        _ => None,
    }
}

fn reviewed(version: &str) -> bool {
    jarvis_llm::claude_worker_protocol::reviewed_claude_version(version.as_bytes())
}

fn copy_hashed(source: &mut File, destination: &mut impl Write) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut length = 0u64;
    loop {
        let count = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("read runtime"),
        };
        length += count as u64;
        if length > BINARY_LIMIT {
            bail!("runtime exceeds its size bound");
        }
        hasher.update(&buffer[..count]);
        destination.write_all(&buffer[..count])?;
    }
    Ok((hex::encode(hasher.finalize()), length))
}

fn open_no_follow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent().context("runtime path has no parent")
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .context("persist runtime directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const PLATFORM: &str = "linux-x64";

    fn gpg(home: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new(GPG)
            .args(["--batch", "--no-options", "--homedir"])
            .arg(home)
            .args(["--pinentry-mode", "loopback", "--passphrase", ""])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn fingerprint(home: &Path) -> String {
        let listing = String::from_utf8(gpg(home, &["--with-colons", "--list-keys"])).unwrap();
        listing
            .lines()
            .find_map(|line| line.strip_prefix("fpr:::::::::"))
            .unwrap()
            .trim_end_matches(':')
            .to_owned()
    }

    /// Rootless signed release served over `file://`, with a throwaway
    /// signing key generated for this test only.
    struct Fixture {
        root: tempfile::TempDir,
        signer: std::path::PathBuf,
        key: String,
        fingerprint: String,
        /// Release directory, target, rollback copy and `file://` base URL.
        paths: [String; 4],
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .arg("--homedir")
                .arg(&self.signer)
                .args(["--kill", "gpg-agent"])
                .status();
        }
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let signer = root.path().join("signer");
            DirBuilder::new().mode(0o700).create(&signer).unwrap();
            for directory in ["bin", "lib", "releases"] {
                fs::create_dir(root.path().join(directory)).unwrap();
                fs::set_permissions(
                    root.path().join(directory),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
            }
            gpg(
                &signer,
                &[
                    "--quick-gen-key",
                    "Jarvis Runtime Fixture <fixture@invalid>",
                    "ed25519",
                    "sign",
                    "never",
                ],
            );
            let key = String::from_utf8(gpg(&signer, &["--armor", "--export"])).unwrap();
            let fingerprint = fingerprint(&signer);
            let path = |name: &str| root.path().join(name).to_str().unwrap().to_owned();
            let paths = [
                path("releases"),
                path("bin/claude"),
                path("lib/jarvis/claude.previous"),
                format!("file://{}", path("releases")),
            ];
            Self {
                root,
                signer,
                key,
                fingerprint,
                paths,
            }
        }

        fn layout(&self) -> Layout<'_> {
            let owner = unsafe { (libc::geteuid(), libc::getegid()) };
            Layout {
                base_url: &self.paths[3],
                protocol: "=file",
                key: &self.key,
                fingerprint: &self.fingerprint,
                target: &self.paths[1],
                previous: &self.paths[2],
                trusted_root: self.root.path().to_str().unwrap(),
                owner,
            }
        }

        fn release(&self, version: &str, binary: &[u8]) -> std::path::PathBuf {
            let size = binary.len() as u64;
            let checksum = hex::encode(Sha256::digest(binary));
            self.release_with(version, binary, &checksum, size)
        }

        fn release_with(
            &self,
            version: &str,
            binary: &[u8],
            checksum: &str,
            size: u64,
        ) -> std::path::PathBuf {
            let directory = Path::new(&self.paths[0]).join(version);
            fs::create_dir_all(directory.join(PLATFORM)).unwrap();
            fs::write(directory.join(PLATFORM).join("claude"), binary).unwrap();
            let manifest = serde_json::json!({
                "version": version,
                "platforms": { PLATFORM: { "binary": "claude", "checksum": checksum, "size": size } }
            });
            let manifest_path = directory.join("manifest.json");
            fs::write(&manifest_path, manifest.to_string()).unwrap();
            let signature = directory.join("manifest.json.sig");
            let _ = fs::remove_file(&signature);
            gpg(
                &self.signer,
                &[
                    "--detach-sign",
                    "--output",
                    signature.to_str().unwrap(),
                    manifest_path.to_str().unwrap(),
                ],
            );
            manifest_path
        }

        fn pointer(&self, channel: &str, contents: &str) {
            fs::write(Path::new(&self.paths[0]).join(channel), contents).unwrap();
        }

        fn target(&self) -> Option<Vec<u8>> {
            fs::read(&self.paths[1]).ok()
        }

        fn previous(&self) -> Option<Vec<u8>> {
            fs::read(&self.paths[2]).ok()
        }

        fn bin_entries(&self) -> Vec<String> {
            let mut entries = fs::read_dir(self.root.path().join("bin"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            entries.sort();
            entries
        }

        /// Asserts that `request` fails with `expected` in its error chain and
        /// leaves the target, rollback copy and target directory untouched.
        fn assert_refused(&self, request: VersionRequest, expected: &str) {
            let before = (self.target(), self.previous(), self.bin_entries());
            let error = install(&self.layout(), &request, PLATFORM).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
            assert_eq!((self.target(), self.previous(), self.bin_entries()), before);
        }
    }

    fn exact(version: &str) -> VersionRequest {
        VersionRequest::Exact(version.to_owned())
    }

    #[test]
    fn pinned_release_key_has_the_published_fingerprint() {
        let home = tempfile::tempdir().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let key = home.path().join("key.asc");
        fs::write(&key, CLAUDE_RELEASE_KEY).unwrap();
        let listing = String::from_utf8(gpg(
            home.path(),
            &["--with-colons", "--show-keys", key.to_str().unwrap()],
        ))
        .unwrap();
        let fingerprints = listing
            .lines()
            .filter_map(|line| line.strip_prefix("fpr:::::::::"))
            .collect::<Vec<_>>();
        assert_eq!(fingerprints, [format!("{}:", PRODUCTION.fingerprint)]);
        assert_eq!(
            listing
                .lines()
                .filter(|line| line.starts_with("pub:"))
                .count(),
            1
        );
    }

    #[test]
    fn production_source_and_destination_are_fixed() {
        assert_eq!(
            PRODUCTION.base_url,
            "https://downloads.claude.ai/claude-code-releases"
        );
        assert_eq!(PRODUCTION.protocol, "=https");
        assert_eq!(PRODUCTION.target, AccountProvider::Claude.binary());
        assert_eq!(PRODUCTION.owner, (0, 0));
        assert_eq!(PRODUCTION.trusted_root, "/");
    }

    #[test]
    fn versions_are_strict_and_gated() {
        assert_eq!(strict_version("2.1.285"), Some((2, 1, 285)));
        for invalid in [
            "2.1",
            "2.1.285.1",
            "v2.1.285",
            "2.1.285-beta",
            "02.1.285",
            "2.1.",
            " 2.1.285",
            "",
        ] {
            assert_eq!(strict_version(invalid), None, "{invalid}");
        }
        assert_eq!(pointer_version(b"2.1.285\n").as_deref(), Some("2.1.285"));
        assert_eq!(pointer_version(b"2.1.285\n\n"), None);
        assert_eq!(pointer_version(b"<html>"), None);
        assert_eq!(
            reported_version("2.1.285 (Claude Code)\n").as_deref(),
            Some("2.1.285")
        );
        assert_eq!(reported_version("2.1.285 or 2.1.286"), None);
        assert!(reviewed("2.1.248") && !reviewed("2.1.247") && !reviewed("2.2.0"));
    }

    #[test]
    fn signed_release_installs_keeps_previous_and_rolls_back() {
        let fixture = Fixture::new();
        fixture.release("2.1.300", b"runtime-300");
        fixture.pointer("stable", "2.1.300\n");
        let layout = fixture.layout();
        let request = VersionRequest::Channel(Channel::Stable);
        assert_eq!(install(&layout, &request, PLATFORM).unwrap(), "2.1.300");
        assert_eq!(fixture.target().as_deref(), Some(&b"runtime-300"[..]));
        let mode = fs::metadata(layout.target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert_eq!(fixture.previous(), None);
        assert_eq!(fixture.bin_entries(), ["claude"]);

        fixture.release("2.1.301", b"runtime-301");
        // Leftovers of a killed install are removed under the lock.
        fs::write(
            Path::new(layout.target).with_file_name(".claude.stale"),
            b"x",
        )
        .unwrap();
        assert_eq!(
            install(&layout, &exact("2.1.301"), PLATFORM).unwrap(),
            "2.1.301"
        );
        assert_eq!(fixture.target().as_deref(), Some(&b"runtime-301"[..]));
        assert_eq!(fixture.previous().as_deref(), Some(&b"runtime-300"[..]));
        let lib = Path::new(layout.previous).parent().unwrap();
        assert_eq!(
            fs::metadata(lib).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // The work directory lived in the rollback directory and is gone.
        assert_eq!(fs::read_dir(lib).unwrap().count(), 1);
        assert_eq!(fixture.bin_entries(), ["claude"]);

        // Reinstalling the active version keeps the real rollback copy.
        assert_eq!(
            install(&layout, &exact("2.1.301"), PLATFORM).unwrap(),
            "2.1.301"
        );
        assert_eq!(fixture.previous().as_deref(), Some(&b"runtime-300"[..]));

        rollback(&layout).unwrap();
        assert_eq!(fixture.target().as_deref(), Some(&b"runtime-300"[..]));
        assert_eq!(fixture.previous(), None);
        assert_eq!(fixture.bin_entries(), ["claude"]);
        assert!(rollback(&layout).is_err());
    }

    #[test]
    fn unverifiable_releases_are_refused_without_changes() {
        let fixture = Fixture::new();
        fixture.release("2.1.300", b"runtime-300");
        install(&fixture.layout(), &exact("2.1.300"), PLATFORM).unwrap();

        // Manifest altered after signing.
        let manifest = fixture.release("2.1.301", b"runtime-301");
        let tampered = fs::read_to_string(&manifest).unwrap() + " ";
        fs::write(&manifest, tampered).unwrap();
        fixture.assert_refused(exact("2.1.301"), "signature");

        // Correct signature, binary does not match the signed checksum.
        fixture.release_with("2.1.302", b"runtime-302", &"0".repeat(64), 11);
        fixture.assert_refused(exact("2.1.302"), "checksum");

        // Served binary is larger than the signed size.
        let checksum = hex::encode(Sha256::digest(b"runtime-303"));
        fixture.release_with("2.1.303", b"runtime-303", &checksum, 4);
        fixture.assert_refused(exact("2.1.303"), "download failed");

        // Signed size above the runtime bound is refused before download.
        fixture.release_with("2.1.304", b"x", &checksum, BINARY_LIMIT + 1);
        fixture.assert_refused(exact("2.1.304"), "size");

        // Signed by a key other than the pinned one.
        let other = Fixture::new();
        let foreign = other.release("2.1.305", b"runtime-305");
        let directory = Path::new(&fixture.paths[0]).join("2.1.305");
        fs::create_dir_all(directory.join(PLATFORM)).unwrap();
        fs::copy(&foreign, directory.join("manifest.json")).unwrap();
        fs::copy(
            foreign.with_extension("json.sig"),
            directory.join("manifest.json.sig"),
        )
        .unwrap();
        fs::write(directory.join(PLATFORM).join("claude"), b"runtime-305").unwrap();
        fixture.assert_refused(exact("2.1.305"), "signature");

        // A validly signed older manifest served under another version path.
        let older = Path::new(&fixture.paths[0]).join("2.1.300");
        let replayed = Path::new(&fixture.paths[0]).join("2.1.306");
        fs::create_dir_all(replayed.join(PLATFORM)).unwrap();
        for file in ["manifest.json", "manifest.json.sig"] {
            fs::copy(older.join(file), replayed.join(file)).unwrap();
        }
        fs::copy(
            older.join(PLATFORM).join("claude"),
            replayed.join(PLATFORM).join("claude"),
        )
        .unwrap();
        fixture.assert_refused(exact("2.1.306"), "different version");

        // Versions outside the reviewed gate never reach the network: these
        // releases do not exist, so only the gate can produce the error.
        fixture.assert_refused(exact("2.2.0"), "reviewed");
        fixture.assert_refused(exact("2.1.247"), "reviewed");
        fixture.assert_refused(exact("2.1.300-beta"), "strict");
        fixture.pointer("latest", "2.1.300-beta\n");
        fixture.assert_refused(VersionRequest::Channel(Channel::Latest), "strict version");
        fixture.pointer("latest", "3.0.0\n");
        fixture.assert_refused(VersionRequest::Channel(Channel::Latest), "reviewed");
        assert_eq!(fixture.target().as_deref(), Some(&b"runtime-300"[..]));
    }

    #[test]
    fn unsafe_destinations_are_refused_without_changes() {
        let fixture = Fixture::new();
        fixture.release("2.1.300", b"runtime-300");
        let layout = fixture.layout();

        let decoy = fixture.root.path().join("decoy");
        fs::write(&decoy, b"decoy").unwrap();
        symlink(&decoy, layout.target).unwrap();
        fixture.assert_refused(exact("2.1.300"), "unsafe");
        assert_eq!(fs::read(&decoy).unwrap(), b"decoy");
        fs::remove_file(layout.target).unwrap();

        let bin = Path::new(layout.target).parent().unwrap();
        fs::set_permissions(bin, fs::Permissions::from_mode(0o777)).unwrap();
        fixture.assert_refused(exact("2.1.300"), "unsafe");
        fs::set_permissions(bin, fs::Permissions::from_mode(0o755)).unwrap();

        fs::create_dir(fixture.root.path().join("lib/jarvis")).unwrap();
        fs::set_permissions(
            fixture.root.path().join("lib/jarvis"),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        fixture.assert_refused(exact("2.1.300"), "unsafe");
        assert_eq!(fixture.target(), None);
    }
}
