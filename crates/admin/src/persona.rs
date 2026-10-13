//! Owner-only persona administration: status, install from the signed private
//! checkout and rollback to a version in the root-only history.
//!
//! Every mutation runs the protected installer with the exact SHA-256 that was
//! validated here, restarts Core, checks `/readyz` and restores the previous
//! persona when Core does not become ready. Output and audit records carry
//! hashes, sizes and times only, never persona text.

use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::Subcommand;
use jarvis_privileged::persona::{validate_persona, PERSONA_MAX_BYTES};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    ai_accounts::validate_system_executable, confirm, laya::restart_core_ready, mutation_lock,
    trusted_command, CONFIG_LOCK,
};

const PERSONA: &str = "/etc/jarvis/Jarvis.md";
const HISTORY: &str = "/etc/jarvis/persona-history";
const INSTALLER: &str = "/usr/local/libexec/jarvis/install-private-config";
const SOURCE_CONFIG: &str = "/etc/jarvis/private-agent-updater.env";
const ALLOWED_SIGNERS: &str = "/etc/jarvis/private-agent-allowed-signers";
const SOURCE_REPOSITORY: &str = "HawkeyNL/PersonalJarvisAgents";
const SOURCE_PERSONA: &str = "personaljarvis/jarvis-core/Jarvis.md";
// Serialize with release updates and the private agent updater, which also
// restart Core and read the private checkout.
const UPDATER_LOCK: &str = "/run/jarvis-updater.lock";
const AGENT_LOCK: &str = "/run/jarvis-private-agent-update.lock";
const GIT: &str = "/usr/bin/git";
const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";
const LOGGER: &str = "/usr/bin/logger";

#[derive(Debug, Subcommand)]
pub(crate) enum PersonaCommand {
    /// Read-only: installed, candidate and history hashes, sizes and times.
    Status,
    /// Install the persona of the signed private checkout commit, restart Core
    /// and verify readiness; the previous persona is restored on failure.
    Install {
        /// Install only when the candidate has this SHA-256 (see `status`).
        #[arg(long, value_parser = parse_sha256)]
        expect_sha256: Option<String>,
        /// Skip the confirmation; requires --expect-sha256.
        #[arg(long, requires = "expect_sha256")]
        yes: bool,
    },
    /// Restore a persona from the history by its full SHA-256, restart Core and
    /// verify readiness; the previous persona is restored on failure.
    Rollback {
        #[arg(long, value_parser = parse_sha256)]
        to: String,
        #[arg(long)]
        yes: bool,
    },
}

fn parse_sha256(input: &str) -> Result<String, String> {
    if valid_sha256(input) {
        Ok(input.to_owned())
    } else {
        Err("must be a full lowercase SHA-256".to_owned())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

struct Layout {
    persona: PathBuf,
    history: PathBuf,
    owner: u32,
}

impl Layout {
    fn production() -> Self {
        Self {
            persona: PERSONA.into(),
            history: HISTORY.into(),
            owner: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Signature {
    Verified,
    Untrusted,
    NoAllowedSigners,
}

/// The persona blob of the private checkout's current commit, read at most
/// one byte past the size cap.
#[derive(Clone, Debug)]
struct Source {
    root: PathBuf,
    commit: String,
    signature: Signature,
    bytes: Vec<u8>,
}

/// Host side effects, injectable so the state machine is tested rootless.
trait Host {
    fn source(&self) -> Result<Source>;
    fn install(&self, source_root: &Path, sha256: &str) -> Result<()>;
    fn restore(&self, sha256: &str) -> Result<()>;
    fn restart_core(&self) -> Result<()>;
    fn audit(&self, record: &str) -> Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Version {
    sha256: String,
    bytes: u64,
    /// Unix time of the last change; for history entries, the last use.
    modified: u64,
    /// Why the content would be refused; a fixed message, never content.
    problem: Option<String>,
}

#[derive(Debug, Serialize)]
struct CandidateStatus {
    commit: String,
    signature: Signature,
    /// Absent when the candidate exceeds the size cap.
    sha256: Option<String>,
    bytes: Option<usize>,
    problem: Option<String>,
}

#[derive(Debug, Serialize)]
struct HistoryEntry {
    #[serde(flatten)]
    version: Version,
    installed: bool,
}

#[derive(Debug, Serialize)]
struct Status {
    max_bytes: usize,
    installed: Option<Version>,
    candidate: Option<CandidateStatus>,
    candidate_error: Option<String>,
    history: Vec<HistoryEntry>,
}

#[derive(Debug, Eq, PartialEq)]
enum Plan {
    Unchanged(String),
    Install {
        source: PathBuf,
        commit: String,
        from: Option<String>,
        to: String,
    },
    Rollback {
        from: Option<String>,
        to: String,
    },
}

pub(crate) fn run(command: PersonaCommand, json: bool) -> Result<()> {
    let layout = Layout::production();
    let host = System;
    let (yes, command) = match command {
        PersonaCommand::Status => {
            let status = status(&layout, &host)?;
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                print_status(&status);
            }
            return Ok(());
        }
        _ if json => bail!("--json is supported only for read-only persona status"),
        PersonaCommand::Install { yes, .. } | PersonaCommand::Rollback { yes, .. } => {
            (yes, command)
        }
    };
    let plan = || match &command {
        PersonaCommand::Install { expect_sha256, .. } => {
            plan_install(&layout, &host, expect_sha256.as_deref())
        }
        PersonaCommand::Rollback { to, .. } => plan_rollback(&layout, to),
        PersonaCommand::Status => unreachable!("handled above"),
    };
    // Confirm before taking the locks, so an unanswered prompt does not block
    // updates or backups; then plan again under the locks.
    let confirmed = plan()?;
    if let Plan::Install { from, to, .. } | Plan::Rollback { from, to } = &confirmed {
        if !yes {
            confirm(&format!(
                "Replace persona {} with {} and restart Core?",
                from.as_deref().map_or("(none)", short),
                short(to)
            ))?;
        }
    }
    let _config = mutation_lock(CONFIG_LOCK)?;
    let _updater = mutation_lock(UPDATER_LOCK)?;
    let _agents = mutation_lock(AGENT_LOCK)?;
    let plan = plan()?;
    ensure!(
        plan == confirmed,
        "the persona or candidate changed after confirmation; nothing changed"
    );
    ignore_interruptions();
    println!("jarvis persona: {}", apply(&layout, &host, &plan)?);
    Ok(())
}

/// An interrupted activation could leave a new persona in place without the
/// readiness check, restore or final audit record. The command is short and
/// bounded by the readiness timeout, so it runs to completion.
fn ignore_interruptions() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: sets the ignore disposition; no handler code is installed.
        unsafe {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
}

/// Hash a file without following a final symlink. Only the first bytes up to
/// one past the cap are kept, for validation; the rest is only hashed.
fn inspect(path: &Path, owner: u32) -> Result<Option<Version>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == owner,
        "{} is not a root-owned regular file",
        path.display()
    );
    let mut head = Vec::new();
    (&file)
        .take(PERSONA_MAX_BYTES as u64 + 1)
        .read_to_end(&mut head)?;
    let mut hasher = Sha256::new();
    hasher.update(&head);
    let rest = io::copy(&mut &file, &mut hasher)?;
    Ok(Some(Version {
        sha256: hex::encode(hasher.finalize()),
        bytes: head.len() as u64 + rest,
        modified: u64::try_from(metadata.mtime()).unwrap_or(0),
        problem: validate_persona(&head).err().map(|error| error.to_string()),
    }))
}

fn installed_sha256(layout: &Layout) -> Result<Option<String>> {
    Ok(inspect(&layout.persona, layout.owner)?.map(|version| version.sha256))
}

fn history(layout: &Layout, installed: Option<&str>) -> Result<Vec<HistoryEntry>> {
    let entries = match fs::read_dir(&layout.history) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read persona history"),
    };
    let mut history = Vec::new();
    for entry in entries {
        let entry = entry.context("read persona history")?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str().filter(|name| valid_sha256(name)) else {
            continue;
        };
        // Unsafe entries are left out; rollback refuses them as well.
        let Ok(Some(mut version)) = inspect(&entry.path(), layout.owner) else {
            continue;
        };
        if version.sha256 != name {
            version.sha256 = name.to_owned();
            version.problem = Some("content does not match its hash".into());
        }
        history.push(HistoryEntry {
            installed: installed == Some(version.sha256.as_str()),
            version,
        });
    }
    history.sort_by(|a, b| {
        b.version
            .modified
            .cmp(&a.version.modified)
            .then_with(|| a.version.sha256.cmp(&b.version.sha256))
    });
    Ok(history)
}

fn status(layout: &Layout, host: &dyn Host) -> Result<Status> {
    let installed = inspect(&layout.persona, layout.owner)?;
    let (candidate, candidate_error) = match host.source() {
        Ok(source) => {
            let within_cap = source.bytes.len() <= PERSONA_MAX_BYTES;
            (
                Some(CandidateStatus {
                    commit: short(&source.commit).to_owned(),
                    signature: source.signature,
                    sha256: within_cap.then(|| sha256_hex(&source.bytes)),
                    bytes: within_cap.then_some(source.bytes.len()),
                    problem: validate_persona(&source.bytes)
                        .err()
                        .map(|error| error.to_string()),
                }),
                None,
            )
        }
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let history = history(
        layout,
        installed.as_ref().map(|version| version.sha256.as_str()),
    )?;
    Ok(Status {
        max_bytes: PERSONA_MAX_BYTES,
        installed,
        candidate,
        candidate_error,
        history,
    })
}

fn print_status(status: &Status) {
    let problem = |problem: &Option<String>| {
        problem.as_deref().map_or_else(
            || "valid".to_owned(),
            |problem| format!("INVALID: {problem}"),
        )
    };
    println!("Jarvis persona (size limit {} bytes)", status.max_bytes);
    match &status.installed {
        Some(version) => println!(
            "Installed   {}  {} bytes  changed {} (unix time)  {}",
            version.sha256,
            version.bytes,
            version.modified,
            problem(&version.problem)
        ),
        None => println!("Installed   none"),
    }
    match (&status.candidate, &status.candidate_error) {
        (Some(candidate), _) => {
            println!(
                "Candidate   {}  commit {}  signature {}  {}",
                candidate.sha256.as_deref().unwrap_or("(too large)"),
                candidate.commit,
                match candidate.signature {
                    Signature::Verified => "verified",
                    Signature::Untrusted => "untrusted",
                    Signature::NoAllowedSigners => "no allowed signers",
                },
                problem(&candidate.problem)
            );
            let installed = status.installed.as_ref().map(|version| &version.sha256);
            if candidate.sha256.is_some() && candidate.sha256.as_ref() != installed {
                println!("            differs from the installed persona");
            }
        }
        (None, Some(error)) => println!("Candidate   unavailable: {error}"),
        (None, None) => println!("Candidate   unavailable"),
    }
    println!(
        "History     {} versions, newest first",
        status.history.len()
    );
    for entry in &status.history {
        println!(
            "  {}  {} bytes  used {} (unix time)  {}{}",
            entry.version.sha256,
            entry.version.bytes,
            entry.version.modified,
            problem(&entry.version.problem),
            if entry.installed { "  installed" } else { "" }
        );
    }
}

fn plan_install(layout: &Layout, host: &dyn Host, expect: Option<&str>) -> Result<Plan> {
    let source = host.source()?;
    ensure!(
        source.signature == Signature::Verified,
        "private checkout commit {} is not signed by an allowed signer",
        short(&source.commit)
    );
    validate_persona(&source.bytes).context("candidate persona is refused")?;
    let to = sha256_hex(&source.bytes);
    if let Some(expect) = expect {
        ensure!(
            expect == to,
            "candidate persona {} does not match --expect-sha256",
            short(&to)
        );
    }
    let from = installed_sha256(layout)?;
    Ok(if from.as_deref() == Some(to.as_str()) {
        Plan::Unchanged(to)
    } else {
        Plan::Install {
            source: source.root,
            commit: source.commit,
            from,
            to,
        }
    })
}

fn plan_rollback(layout: &Layout, to: &str) -> Result<Plan> {
    ensure!(valid_sha256(to), "rollback target must be a full SHA-256");
    let entry = inspect(&layout.history.join(to), layout.owner)?
        .with_context(|| format!("persona {} is not in the history", short(to)))?;
    ensure!(
        entry.sha256 == to,
        "history entry {} does not match its hash",
        short(to)
    );
    if let Some(problem) = entry.problem {
        bail!("history entry {} is refused: {problem}", short(to));
    }
    let from = installed_sha256(layout)?;
    Ok(if from.as_deref() == Some(to) {
        Plan::Unchanged(to.to_owned())
    } else {
        Plan::Rollback {
            from,
            to: to.to_owned(),
        }
    })
}

fn apply(layout: &Layout, host: &dyn Host, plan: &Plan) -> Result<String> {
    let (action, from, to) = match plan {
        Plan::Unchanged(sha256) => {
            return Ok(format!("persona {} is already installed", short(sha256)))
        }
        Plan::Install {
            commit, from, to, ..
        } => (
            format!("action=install commit={commit}"),
            from.as_deref(),
            to.as_str(),
        ),
        Plan::Rollback { from, to } => ("action=rollback".to_owned(), from.as_deref(), to.as_str()),
    };
    let record = |outcome: &str| {
        format!(
            "{action} from={} to={to} outcome={outcome}",
            from.unwrap_or("none")
        )
    };
    host.audit(&record("initiated"))
        .context("audit event could not be recorded; nothing changed")?;
    let (outcome, result) = activate(layout, host, plan, from, to);
    if let Err(error) = host.audit(&record(outcome)) {
        // Keep the real outcome visible: it may be a failed restore.
        let outcome = match result {
            Ok(message) => message,
            Err(failure) => format!("{failure:#}"),
        };
        bail!("persona audit event failed ({error:#}) after: {outcome}");
    }
    result
}

fn activate(
    layout: &Layout,
    host: &dyn Host,
    plan: &Plan,
    from: Option<&str>,
    to: &str,
) -> (&'static str, Result<String>) {
    let installed = match plan {
        Plan::Install { source, .. } => host.install(source, to),
        _ => host.restore(to),
    };
    let failure = match installed {
        Ok(()) => match host.restart_core() {
            Ok(()) => {
                return (
                    "succeeded",
                    Ok(format!("persona {} is active and Core is ready", short(to))),
                )
            }
            Err(error) => error.context(format!("Core did not become ready with {}", short(to))),
        },
        Err(error) => {
            // The installer replaces the persona as one of its last steps;
            // restore only when it got that far.
            let current = installed_sha256(layout).ok().flatten();
            if current.as_deref() == from {
                return (
                    "failed",
                    Err(error.context("persona installer failed; the persona is unchanged")),
                );
            }
            error.context("persona installer failed after replacing the persona")
        }
    };
    let Some(from) = from else {
        return (
            "failed",
            Err(anyhow!(
                "{failure:#}; there is no previous persona to restore"
            )),
        );
    };
    match host.restore(from).and_then(|()| host.restart_core()) {
        Ok(()) => (
            "restored",
            Err(anyhow!(
                "{failure:#}; restored the previous persona {} and Core is ready",
                short(from)
            )),
        ),
        Err(error) => (
            "restore-failed",
            Err(anyhow!(
                "{failure:#}; restoring the previous persona {} also failed: {error:#}",
                short(from)
            )),
        ),
    }
}

/// The private checkout path from the root-only private agent updater
/// configuration, parsed with the same rules as the updater.
fn parse_source_config(text: &str) -> Result<String> {
    let (mut source, mut repository, mut signed) = (None, None, None);
    // Split on '\n' only, like the updater's `read`: a '\r' stays in the
    // value and fails validation there and here.
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        let Some((key, value)) = line.split_once('=') else {
            ensure!(
                line.is_empty(),
                "private agent configuration contains an unsupported entry"
            );
            continue;
        };
        let slot = match key {
            "JARVIS_PRIVATE_AGENT_SOURCE" => &mut source,
            "JARVIS_PRIVATE_AGENT_REPOSITORY" => &mut repository,
            "JARVIS_PRIVATE_AGENT_REQUIRE_SIGNED" => &mut signed,
            _ => bail!("private agent configuration contains an unsupported entry"),
        };
        ensure!(
            slot.replace(value).is_none(),
            "private agent configuration contains a duplicate entry"
        );
    }
    ensure!(
        repository == Some(SOURCE_REPOSITORY) && matches!(signed, None | Some("true" | "false")),
        "private agent configuration is invalid"
    );
    source
        .filter(|path| {
            path.starts_with('/')
                && path
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
        })
        .map(str::to_owned)
        .context("private agent checkout path is invalid")
}

fn root_controlled(metadata: &fs::Metadata) -> bool {
    !metadata.file_type().is_symlink() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0
}

/// Most entries walked below a checkout's `.git`.
const GIT_TREE_LIMIT: usize = 200_000;

/// Every entry below `path` (not following symlinks) must belong to `owner`
/// and, unless it is a symlink, be writable by nobody else: a writable
/// config, object or ref could change what root's Git reads or runs.
fn owner_controlled_tree(path: &Path, owner: u32, budget: &mut usize) -> Result<()> {
    ensure!(
        *budget > 0,
        "the private checkout's Git directory is too large to check"
    );
    *budget -= 1;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.uid() == owner
            && (metadata.file_type().is_symlink() || metadata.mode() & 0o022 == 0),
        "{} is not root-controlled",
        path.display()
    );
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            owner_controlled_tree(&entry?.path(), owner, budget)?;
        }
    }
    Ok(())
}

/// Overrides for Git running as root in the private checkout: the checkout
/// cannot select hooks, fsmonitor or a signature program, and only SSH
/// signatures checked against `signers` verify. `gpg.format` selects only the
/// signing backend; verification follows the signature's own type, so the
/// OpenPGP and X.509 programs are disabled outright.
fn git_overrides(signers: &str) -> Vec<String> {
    [
        "core.hooksPath=/dev/null",
        "core.fsmonitor=false",
        &format!("gpg.ssh.program={SSH_KEYGEN}"),
        "gpg.openpgp.program=/bin/false",
        "gpg.x509.program=/bin/false",
        &format!("gpg.ssh.allowedSignersFile={signers}"),
    ]
    .into_iter()
    .flat_map(|setting| ["-c".to_owned(), setting.to_owned()])
    .collect()
}

struct System;

impl System {
    fn configured_source() -> Result<PathBuf> {
        let metadata = fs::symlink_metadata(SOURCE_CONFIG)
            .context("private agent configuration is unavailable")?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == 0
                && metadata.gid() == 0
                && metadata.mode() & 0o777 == 0o600
                && metadata.len() <= 4096,
            "private agent configuration is unsafe"
        );
        let path = parse_source_config(
            &fs::read_to_string(SOURCE_CONFIG).context("read private agent configuration")?,
        )?;
        let root = fs::canonicalize(path).context("private agent checkout is unavailable")?;
        // Git runs as root in this checkout: no other user may control it.
        for ancestor in root.ancestors() {
            ensure!(
                fs::symlink_metadata(ancestor).is_ok_and(|metadata| root_controlled(&metadata)),
                "private agent checkout path is not root-controlled: {}",
                ancestor.display()
            );
        }
        ensure!(
            fs::symlink_metadata(root.join(".git")).is_ok_and(|metadata| metadata.is_dir()),
            "private agent checkout is not a Git checkout"
        );
        let mut budget = GIT_TREE_LIMIT;
        owner_controlled_tree(&root.join(".git"), 0, &mut budget)
            .context("private agent checkout is not root-controlled")?;
        Ok(root)
    }

    fn allowed_signers_safe() -> bool {
        fs::symlink_metadata(ALLOWED_SIGNERS).is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.len() > 0
                && metadata.uid() == 0
                && metadata.gid() == 0
                && (metadata.mode() & 0o7777) | 0o044 == 0o644
        })
    }

    /// Git with `git_overrides`. Output is read up to `limit` bytes.
    fn git(root: &Path, args: &[&str], limit: u64) -> Result<(bool, Vec<u8>)> {
        let mut child = trusted_command(GIT)
            .arg("-C")
            .arg(root)
            .args(git_overrides(ALLOWED_SIGNERS))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start git")?;
        let mut bytes = Vec::new();
        // Dropping the pipe after `limit` bytes stops a larger output.
        child
            .stdout
            .take()
            .context("missing git output")?
            .take(limit)
            .read_to_end(&mut bytes)?;
        Ok((child.wait()?.success(), bytes))
    }

    fn installer(args: &[&OsStr]) -> Result<()> {
        validate_system_executable(INSTALLER)?;
        let status = trusted_command(INSTALLER)
            .args(args)
            .stdin(Stdio::null())
            .status()
            .context("start the persona installer")?;
        ensure!(status.success(), "persona installer exited with {status}");
        Ok(())
    }
}

impl Host for System {
    fn source(&self) -> Result<Source> {
        let root = Self::configured_source()?;
        validate_system_executable(GIT)?;
        let (ok, head) = Self::git(&root, &["rev-parse", "--verify", "HEAD^{commit}"], 128)?;
        let commit = String::from_utf8(head)
            .ok()
            .map(|head| head.trim_end().to_owned())
            .filter(|commit| {
                commit.len() == 40
                    && commit
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .filter(|_| ok)
            .context("private agent checkout has no valid commit")?;
        let signature = if !Self::allowed_signers_safe() {
            Signature::NoAllowedSigners
        } else if validate_system_executable(SSH_KEYGEN).is_ok()
            && Self::git(&root, &["verify-commit", &commit], 4096)?.0
        {
            Signature::Verified
        } else {
            Signature::Untrusted
        };
        // Read the blob of this exact commit, not the working tree: the
        // installer must then find the same bytes (`--expect-sha256`).
        let limit = PERSONA_MAX_BYTES as u64 + 1;
        let (ok, bytes) = Self::git(
            &root,
            &["cat-file", "blob", &format!("{commit}:{SOURCE_PERSONA}")],
            limit,
        )?;
        ensure!(
            ok || bytes.len() as u64 == limit,
            "commit {} has no persona",
            short(&commit)
        );
        Ok(Source {
            root,
            commit,
            signature,
            bytes,
        })
    }

    fn install(&self, source_root: &Path, sha256: &str) -> Result<()> {
        Self::installer(&[
            OsStr::new("--source"),
            source_root.as_os_str(),
            OsStr::new("--expect-sha256"),
            OsStr::new(sha256),
        ])
    }

    fn restore(&self, sha256: &str) -> Result<()> {
        Self::installer(&[OsStr::new("--restore"), OsStr::new(sha256)])
    }

    fn restart_core(&self) -> Result<()> {
        restart_core_ready()
    }

    /// Records only fixed `key=value` fields with hashes.
    fn audit(&self, record: &str) -> Result<()> {
        validate_system_executable(LOGGER)?;
        let status = trusted_command(LOGGER)
            .args([
                "--tag",
                "jarvis-persona",
                "--priority",
                "authpriv.notice",
                "--",
                record,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        ensure!(
            status.success(),
            "persona audit event could not be recorded"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        fs::File,
        os::unix::fs::{symlink, PermissionsExt},
        time::{Duration, UNIX_EPOCH},
    };

    const OLD: &[u8] = b"# Jarvis\nold persona marker\n";
    const NEW: &[u8] = b"# Jarvis\nnew persona marker\n";

    struct Fake {
        persona: PathBuf,
        history: PathBuf,
        source: Option<Source>,
        calls: RefCell<Vec<String>>,
        restarts: RefCell<VecDeque<bool>>,
        installer_fails: Cell<bool>,
        installer_fails_after_replace: Cell<bool>,
        restore_fails: Cell<bool>,
        /// Audit records containing this text fail.
        audit_fails: Cell<Option<&'static str>>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
        fn record(&self, call: String) {
            self.calls.borrow_mut().push(call);
        }
        /// Mirrors the installer: archive the current version, then replace.
        fn replace(&self, bytes: &[u8]) {
            if let Ok(current) = fs::read(&self.persona) {
                fs::write(self.history.join(sha256_hex(&current)), current).unwrap();
            }
            fs::write(self.history.join(sha256_hex(bytes)), bytes).unwrap();
            fs::write(&self.persona, bytes).unwrap();
        }
    }

    impl Host for Fake {
        fn source(&self) -> Result<Source> {
            self.source.clone().context("no source")
        }
        fn install(&self, _source_root: &Path, sha256: &str) -> Result<()> {
            self.record(format!("install {}", short(sha256)));
            ensure!(!self.installer_fails.get(), "installer failed");
            let bytes = self.source.as_ref().unwrap().bytes.clone();
            assert_eq!(sha256_hex(&bytes), sha256);
            self.replace(&bytes);
            ensure!(
                !self.installer_fails_after_replace.get(),
                "post-check failed"
            );
            Ok(())
        }
        fn restore(&self, sha256: &str) -> Result<()> {
            self.record(format!("restore {}", short(sha256)));
            ensure!(!self.restore_fails.get(), "restore failed");
            let bytes = fs::read(self.history.join(sha256))?;
            self.replace(&bytes);
            Ok(())
        }
        fn restart_core(&self) -> Result<()> {
            self.record("restart".into());
            ensure!(
                self.restarts.borrow_mut().pop_front().unwrap_or(true),
                "not ready"
            );
            Ok(())
        }
        fn audit(&self, record: &str) -> Result<()> {
            ensure!(
                !self
                    .audit_fails
                    .get()
                    .is_some_and(|text| record.contains(text)),
                "logger failed"
            );
            self.record(format!("audit {record}"));
            Ok(())
        }
    }

    fn fixture(installed: Option<&[u8]>, candidate: &[u8]) -> (tempfile::TempDir, Layout, Fake) {
        let directory = tempfile::tempdir().unwrap();
        let history = directory.path().join("persona-history");
        fs::create_dir(&history).unwrap();
        let layout = Layout {
            persona: directory.path().join("Jarvis.md"),
            history: history.clone(),
            owner: fs::metadata(directory.path()).unwrap().uid(),
        };
        let fake = Fake {
            persona: layout.persona.clone(),
            history,
            source: Some(Source {
                root: directory.path().join("checkout"),
                commit: "a".repeat(40),
                signature: Signature::Verified,
                bytes: candidate.to_vec(),
            }),
            calls: RefCell::default(),
            restarts: RefCell::default(),
            installer_fails: Cell::default(),
            installer_fails_after_replace: Cell::default(),
            restore_fails: Cell::default(),
            audit_fails: Cell::default(),
        };
        if let Some(installed) = installed {
            fake.replace(installed);
        }
        (directory, layout, fake)
    }

    fn installed(layout: &Layout) -> Option<String> {
        installed_sha256(layout).unwrap()
    }

    fn install(layout: &Layout, fake: &Fake) -> Result<String> {
        apply(layout, fake, &plan_install(layout, fake, None)?)
    }

    fn audit(action: &str, from: &[u8], to: &[u8], outcome: &str) -> String {
        format!(
            "audit action={action}{} from={} to={} outcome={outcome}",
            if action == "install" {
                format!(" commit={}", "a".repeat(40))
            } else {
                String::new()
            },
            sha256_hex(from),
            sha256_hex(to)
        )
    }

    #[test]
    fn install_activates_the_signed_candidate_and_audits_hashes() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        install(&layout, &fake).unwrap();
        assert_eq!(installed(&layout), Some(sha256_hex(NEW)));
        assert_eq!(
            fake.calls(),
            [
                audit("install", OLD, NEW, "initiated"),
                format!("install {}", short(&sha256_hex(NEW))),
                "restart".into(),
                audit("install", OLD, NEW, "succeeded"),
            ]
        );
    }

    #[test]
    fn install_refuses_unsigned_invalid_or_unexpected_candidates_before_any_change() {
        for signature in [Signature::Untrusted, Signature::NoAllowedSigners] {
            let (_directory, layout, mut fake) = fixture(Some(OLD), NEW);
            fake.source.as_mut().unwrap().signature = signature;
            assert!(plan_install(&layout, &fake, None).is_err());
        }
        let (_directory, layout, fake) = fixture(Some(OLD), b"no heading\n");
        assert!(plan_install(&layout, &fake, None).is_err());
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        let error = plan_install(&layout, &fake, Some(&sha256_hex(OLD))).unwrap_err();
        assert!(error.to_string().contains("--expect-sha256"));
        assert!(plan_install(&layout, &fake, Some(&sha256_hex(NEW))).is_ok());
        assert!(fake.calls().is_empty());
        assert_eq!(installed(&layout), Some(sha256_hex(OLD)));
    }

    #[test]
    fn unchanged_candidate_is_a_no_op() {
        let (_directory, layout, fake) = fixture(Some(NEW), NEW);
        let plan = plan_install(&layout, &fake, None).unwrap();
        assert_eq!(plan, Plan::Unchanged(sha256_hex(NEW)));
        apply(&layout, &fake, &plan).unwrap();
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn nothing_changes_without_an_audit_trail() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.audit_fails.set(Some(""));
        assert!(install(&layout, &fake).is_err());
        assert!(fake.calls().is_empty());
        assert_eq!(installed(&layout), Some(sha256_hex(OLD)));
    }

    #[test]
    fn readiness_failure_restores_the_previous_persona() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.restarts.borrow_mut().extend([false, true]);
        let error = install(&layout, &fake).unwrap_err().to_string();
        assert!(error.contains("restored the previous persona"), "{error}");
        assert_eq!(installed(&layout), Some(sha256_hex(OLD)));
        assert_eq!(
            fake.calls()[1..],
            [
                format!("install {}", short(&sha256_hex(NEW))),
                "restart".into(),
                format!("restore {}", short(&sha256_hex(OLD))),
                "restart".into(),
                audit("install", OLD, NEW, "restored"),
            ]
        );
    }

    #[test]
    fn failed_restore_is_reported_and_audited() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.restarts.borrow_mut().extend([false, false]);
        let error = install(&layout, &fake).unwrap_err().to_string();
        assert!(error.contains("also failed"), "{error}");
        assert_eq!(
            fake.calls().last().unwrap(),
            &audit("install", OLD, NEW, "restore-failed")
        );
    }

    #[test]
    fn first_install_without_previous_reports_the_failure() {
        let (_directory, layout, fake) = fixture(None, NEW);
        fake.restarts.borrow_mut().push_back(false);
        let error = install(&layout, &fake).unwrap_err().to_string();
        assert!(error.contains("no previous persona"), "{error}");
        assert!(!fake.calls().iter().any(|call| call.starts_with("restore")));
        assert!(fake
            .calls()
            .last()
            .unwrap()
            .ends_with(&format!("from=none to={} outcome=failed", sha256_hex(NEW))));
    }

    #[test]
    fn installer_failure_restores_only_when_the_persona_was_replaced() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.installer_fails.set(true);
        let error = install(&layout, &fake).unwrap_err().to_string();
        assert!(error.contains("unchanged"), "{error}");
        assert!(!fake.calls().iter().any(|call| call == "restart"));
        assert_eq!(
            fake.calls().last().unwrap(),
            &audit("install", OLD, NEW, "failed")
        );

        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.installer_fails_after_replace.set(true);
        assert!(install(&layout, &fake).is_err());
        assert_eq!(installed(&layout), Some(sha256_hex(OLD)));
        assert_eq!(
            fake.calls().last().unwrap(),
            &audit("install", OLD, NEW, "restored")
        );
    }

    #[test]
    fn rollback_accepts_only_intact_valid_history_entries() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        install(&layout, &fake).unwrap();
        // Unknown hash.
        assert!(plan_rollback(&layout, &"0".repeat(64)).is_err());
        // Content that does not match its name.
        let tampered = sha256_hex(b"# Jarvis\nother\n");
        fs::write(layout.history.join(&tampered), NEW).unwrap();
        assert!(plan_rollback(&layout, &tampered).is_err());
        // Intact but invalid content.
        let invalid: &[u8] = b"no heading\n";
        fs::write(layout.history.join(sha256_hex(invalid)), invalid).unwrap();
        assert!(plan_rollback(&layout, &sha256_hex(invalid)).is_err());
        // A symlink, even to a valid version.
        let linked = sha256_hex(b"# Jarvis\nlinked\n");
        fs::write(
            layout.persona.with_file_name("linked"),
            b"# Jarvis\nlinked\n",
        )
        .unwrap();
        symlink(
            layout.persona.with_file_name("linked"),
            layout.history.join(&linked),
        )
        .unwrap();
        assert!(plan_rollback(&layout, &linked).is_err());
        // Not a hash at all.
        assert!(plan_rollback(&layout, "../Jarvis.md").is_err());
        // The installed version is a no-op; the previous one is a rollback.
        assert_eq!(
            plan_rollback(&layout, &sha256_hex(NEW)).unwrap(),
            Plan::Unchanged(sha256_hex(NEW))
        );
        let plan = plan_rollback(&layout, &sha256_hex(OLD)).unwrap();
        fake.calls.borrow_mut().clear();
        apply(&layout, &fake, &plan).unwrap();
        assert_eq!(installed(&layout), Some(sha256_hex(OLD)));
        assert_eq!(
            fake.calls(),
            [
                audit("rollback", NEW, OLD, "initiated"),
                format!("restore {}", short(&sha256_hex(OLD))),
                "restart".into(),
                audit("rollback", NEW, OLD, "succeeded"),
            ]
        );
    }

    #[test]
    fn status_lists_history_newest_first_and_never_content() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        install(&layout, &fake).unwrap();
        for (bytes, seconds) in [(OLD, 100), (NEW, 200)] {
            File::options()
                .write(true)
                .open(layout.history.join(sha256_hex(bytes)))
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
        }
        fs::write(layout.history.join("notes.txt"), b"ignored").unwrap();
        let status = status(&layout, &fake).unwrap();
        let hashes: Vec<_> = status
            .history
            .iter()
            .map(|entry| (entry.version.sha256.clone(), entry.installed))
            .collect();
        assert_eq!(hashes, [(sha256_hex(NEW), true), (sha256_hex(OLD), false)]);
        assert_eq!(status.history[0].version.modified, 200);
        let candidate = status.candidate.as_ref().unwrap();
        assert_eq!(candidate.sha256, Some(sha256_hex(NEW)));
        assert_eq!(candidate.commit, "a".repeat(12));
        let json = serde_json::to_string(&status).unwrap();
        assert!(
            !json.contains("marker") && !json.contains("Jarvis"),
            "{json}"
        );
    }

    #[test]
    fn oversized_candidate_reports_no_hash() {
        let mut large = b"# Jarvis\n".to_vec();
        large.resize(PERSONA_MAX_BYTES + 1, b'a');
        let (_directory, layout, fake) = fixture(None, &large);
        let status = status(&layout, &fake).unwrap();
        let candidate = status.candidate.unwrap();
        assert_eq!(candidate.sha256, None);
        assert!(candidate.problem.is_some());
        assert!(plan_install(&layout, &fake, None).is_err());
    }

    #[test]
    fn source_configuration_follows_the_updater_rules() {
        let valid = "JARVIS_PRIVATE_AGENT_SOURCE=/var/lib/jarvis-agents-source\n\
                     JARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n\
                     JARVIS_PRIVATE_AGENT_REQUIRE_SIGNED=true\n";
        assert_eq!(
            parse_source_config(valid).unwrap(),
            "/var/lib/jarvis-agents-source"
        );
        for invalid in [
            "JARVIS_PRIVATE_AGENT_SOURCE=/a\nJARVIS_PRIVATE_AGENT_REPOSITORY=other/repo\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=relative\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=/a b\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=/a\nJARVIS_PRIVATE_AGENT_SOURCE=/b\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=/a\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\nOTHER=1\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=/a\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n# comment\n",
            "JARVIS_PRIVATE_AGENT_SOURCE=/a\nJARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\nJARVIS_PRIVATE_AGENT_REQUIRE_SIGNED=yes\n",
            "JARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n",
        ] {
            assert!(parse_source_config(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn failed_final_audit_still_reports_the_outcome() {
        let (_directory, layout, fake) = fixture(Some(OLD), NEW);
        fake.restarts.borrow_mut().extend([false, false]);
        fake.audit_fails.set(Some("outcome=restore-failed"));
        let error = format!("{:#}", install(&layout, &fake).unwrap_err());
        assert!(
            error.contains("audit event failed") && error.contains("also failed"),
            "{error}"
        );
    }

    #[test]
    fn git_tree_check_refuses_foreign_writable_entries() {
        let directory = tempfile::tempdir().unwrap();
        let git = directory.path().join(".git");
        fs::create_dir_all(git.join("objects/ab")).unwrap();
        fs::write(git.join("config"), b"").unwrap();
        fs::write(git.join("objects/ab/cd"), b"").unwrap();
        let owner = fs::metadata(&git).unwrap().uid();
        for path in [&git, &git.join("objects"), &git.join("objects/ab")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for path in [git.join("config"), git.join("objects/ab/cd")] {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(owner_controlled_tree(&git, owner, &mut 100).is_ok());
        assert!(owner_controlled_tree(&git, owner, &mut 3).is_err());
        assert!(owner_controlled_tree(&git, owner + 1, &mut 100).is_err());
        for (path, mode) in [("config", 0o664), ("objects/ab", 0o777)] {
            fs::set_permissions(git.join(path), fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                owner_controlled_tree(&git, owner, &mut 100).is_err(),
                "{path}"
            );
            let safe = if mode == 0o664 { 0o644 } else { 0o755 };
            fs::set_permissions(git.join(path), fs::Permissions::from_mode(safe)).unwrap();
        }
    }

    /// `gpg.format=ssh` does not stop Git from verifying an OpenPGP signature
    /// with gpg. The overrides must make such a commit fail even when the
    /// OpenPGP program would report a good signature.
    #[test]
    fn only_ssh_signatures_can_verify() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let git = |args: &[&str], input: Option<&[u8]>| {
            let mut child = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            if let Some(input) = input {
                std::io::Write::write_all(&mut child.stdin.take().unwrap(), input).unwrap();
            }
            drop(child.stdin.take());
            let output = child.wait_with_output().unwrap();
            (
                output.status.success(),
                String::from_utf8(output.stdout).unwrap().trim().to_owned(),
            )
        };
        assert!(git(&["init", "--quiet"], None).0);
        let (_, tree) = git(&["mktree"], Some(b""));
        let commit = format!(
            "tree {tree}\nauthor A <a@example.invalid> 0 +0000\n\
             committer A <a@example.invalid> 0 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n fixture\n -----END PGP SIGNATURE-----\n\n\
             fixture\n"
        );
        let (ok, commit) = git(
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            Some(commit.as_bytes()),
        );
        assert!(ok);
        let fake_gpg = root.join("fake-gpg");
        fs::write(
            &fake_gpg,
            "#!/bin/sh\ncat >/dev/null\nprintf '\\n[GNUPG:] GOODSIG 0123456789ABCDEF fixture\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake_gpg, fs::Permissions::from_mode(0o755)).unwrap();
        let program = format!("gpg.openpgp.program={}", fake_gpg.display());
        let signers = root.join("allowed_signers");
        fs::write(&signers, b"").unwrap();
        let signers = signers.to_str().unwrap();
        // Without the overrides the fake program is trusted.
        assert!(git(&["-c", &program, "verify-commit", &commit], None).0);
        let mut args = vec!["-c", &program];
        let overrides = git_overrides(signers);
        args.extend(overrides.iter().map(String::as_str));
        args.extend(["verify-commit", &commit]);
        assert!(!git(&args, None).0);
    }

    #[test]
    fn source_configuration_rejects_carriage_returns() {
        let text = "JARVIS_PRIVATE_AGENT_SOURCE=/a\r\n\
                    JARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents\n";
        assert!(parse_source_config(text).is_err());
    }
}
