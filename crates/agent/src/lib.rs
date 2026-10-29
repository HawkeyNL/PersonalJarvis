//! Agentic execution — policy-gated, sandboxed hands for Jarvis (ADR-029).
//!
//! - **Typed allowlist, no free shell.** Only the [`Action`] variants exist; a
//!   request that doesn't map to one can't be represented, let alone run.
//! - **Risk-classed.** Read-only actions are `Auto`; mutating ones
//!   (`write_file`, `git_commit`, `claude_code`) are `NeedsApproval` and only run
//!   after a device-signed approval (4b), verified in the API.
//! - **Sandboxed.** Every path is resolved inside a single workspace root; no
//!   escape (`..`/symlinks), no secrets (`.env`, keys, `.ssh`) — even to read.
//! - **Off by default + audited.** The kill switch (`JARVIS_AGENT_ENABLED`) and
//!   the audit log live in the API; this crate is the pure policy + executor.
//!
//! The legacy local **Claude Code executor** remains modelled only for previews
//! and compatibility. It is deliberately not allowed to spawn on the Home Node:
//! coding work must move through the OpenSandbox provider boundary first.
//!
//! The Core (`jarvis-core/**`, policy, secrets) is never agent-writable — not via a
//! `write_file`, not via Claude Code, not even with a signed approval.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

const MAX_OUTPUT: usize = 64 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const EXEC_TIMEOUT: Duration = Duration::from_secs(20);
/// The only environment variables a workspace subprocess inherits.
const CHILD_ENV: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL"];
/// A read-only action Jarvis may perform. This enum *is* the allowlist: anything
/// not expressible here cannot be requested (4a has no mutating variants).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// List a directory (relative to the sandbox root).
    ListDir { path: String },
    /// Read a text file (relative to the sandbox root).
    ReadFile { path: String },
    /// Search for a pattern under a path (both relative to the root).
    Grep { pattern: String, path: String },
    /// A read-only git query in the workspace.
    Git { sub: GitRead },
    /// Write (create/overwrite) a text file in the sandbox. **Mutating** —
    /// NeedsApproval. Never allowed into the Core, `.git`, or secrets.
    WriteFile { path: String, content: String },
    /// Stage all changes and commit them. **Mutating** — NeedsApproval.
    GitCommit { message: String },
    /// Drive headless Claude Code as a confined code-executor in the sandbox
    /// (ADR-029 fase 4c). **Mutating** — NeedsApproval; runs only when the CC
    /// executor is deliberately enabled, with deny-rules that block the Core,
    /// `.git`, secrets, the shell (`Bash`) and network tools.
    ClaudeCode { prompt: String },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRead {
    Status,
    Diff,
    Log,
}

/// Risk class of an action (ADR-029 laag 2). 4a only ever produces `Auto`;
/// mutating classes arrive in 4b behind the approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Auto,
    NeedsApproval,
    Denied,
}

/// Map an action to its policy capability + risk class (review P2). This is the
/// single place that says *what kind of thing* an action is; the approval
/// decision itself comes from [`jarvis_policy::decide`].
fn action_capability(action: &Action) -> (jarvis_policy::Capability, jarvis_policy::RiskClass) {
    use jarvis_policy::{Capability, RiskClass as PRisk};
    match action {
        Action::WriteFile { .. } | Action::GitCommit { .. } => {
            (Capability::ManageFiles, PRisk::Mutating)
        }
        Action::ClaudeCode { .. } => (Capability::ExecuteCode, PRisk::Mutating),
        Action::ListDir { .. }
        | Action::ReadFile { .. }
        | Action::Grep { .. }
        | Action::Git { .. } => (Capability::ReadData, PRisk::ReadOnly),
    }
}

/// Classify an action (ADR-029 laag 2). The auto-vs-approval decision is owned by
/// `jarvis-policy` (review P2 — one authoritative policy path); this function is
/// the adapter that maps a [`jarvis_policy::PolicyDecision`] onto the agent's
/// [`RiskClass`]. Path-level denials (escape/secret/Core) are enforced separately
/// at resolution, since they are workspace-specific rather than capability-class.
pub fn classify(action: &Action) -> RiskClass {
    let (capability, risk) = action_capability(action);
    // An unapproved, non-reversible request from a trusted device: policy decides
    // whether it may run automatically or must be signed off.
    let decision = jarvis_policy::decide(&jarvis_policy::PolicyContext {
        capability,
        risk,
        trusted_device: true,
        approved: false,
        reversible: false,
    });
    match decision {
        jarvis_policy::PolicyDecision::Allow => RiskClass::Auto,
        jarvis_policy::PolicyDecision::RequireApproval => RiskClass::NeedsApproval,
        jarvis_policy::PolicyDecision::Deny => RiskClass::Denied,
    }
}

/// Whether an action mutates state (⇒ needs approval + a preview).
pub fn is_mutating(action: &Action) -> bool {
    classify(action) == RiskClass::NeedsApproval
}

/// Stable audit label for an action.
pub fn action_type(a: &Action) -> &'static str {
    match a {
        Action::ListDir { .. } => "list_dir",
        Action::ReadFile { .. } => "read_file",
        Action::Grep { .. } => "grep",
        Action::Git { sub } => match sub {
            GitRead::Status => "git_status",
            GitRead::Diff => "git_diff",
            GitRead::Log => "git_log",
        },
        Action::WriteFile { .. } => "write_file",
        Action::GitCommit { .. } => "git_commit",
        Action::ClaudeCode { .. } => "claude_code",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("no workspace configured")]
    NoWorkspace,
    #[error("path escapes the sandbox")]
    OutsideSandbox,
    #[error("denied: {0}")]
    Denied(String),
    #[error("path not found")]
    NotFound,
    #[error("execution failed: {0}")]
    Exec(String),
    #[error("timed out")]
    Timeout,
}

/// Config for the confined Claude Code executor (ADR-029 fase 4c). Present only
/// when the owner deliberately enables it; absent ⇒ `claude_code` is refused.
#[derive(Debug, Clone)]
pub struct ClaudeCodeCfg {
    /// The `claude` binary to drive.
    pub bin: String,
    /// Model id; empty ⇒ let `claude` choose its own default.
    pub model: String,
}

/// A confined workspace: all file access resolves inside `root` (canonicalized).
pub struct Sandbox {
    root: PathBuf,
    claude_code: Option<ClaudeCodeCfg>,
}

impl Sandbox {
    /// Build a sandbox from a workspace root. Fails if the root doesn't exist.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, AgentError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| AgentError::NoWorkspace)?;
        Ok(Self {
            root,
            claude_code: None,
        })
    }

    /// Opt this sandbox into the Claude Code executor (4c). Without this, a
    /// `claude_code` action is denied even when the agent is enabled.
    pub fn with_claude_code(mut self, cfg: ClaudeCodeCfg) -> Self {
        self.claude_code = Some(cfg);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a relative path inside the sandbox: reject absolutes, `..`/symlink
    /// escapes (via canonicalize + containment), and secret files.
    fn resolve(&self, rel: &str) -> Result<PathBuf, AgentError> {
        if Path::new(rel).is_absolute() {
            return Err(AgentError::OutsideSandbox);
        }
        let canon = self
            .root
            .join(rel)
            .canonicalize()
            .map_err(|_| AgentError::NotFound)?;
        if !canon.starts_with(&self.root) {
            return Err(AgentError::OutsideSandbox);
        }
        if is_secret(&canon) {
            return Err(AgentError::Denied("secret path".into()));
        }
        Ok(canon)
    }

    /// Resolve a path to **write** to: the file need not exist yet, but its parent
    /// must exist inside the sandbox, and the target may never be the Core, a
    /// secret, or inside `.git`. Absolute protected configuration (including
    /// `/etc/jarvis/Jarvis.md`) is rejected before this resolver is entered.
    /// Returns the existing file's metadata, if any, so the writer can check it
    /// opened that same file.
    fn resolve_write(&self, rel: &str) -> Result<(PathBuf, Option<std::fs::Metadata>), AgentError> {
        if Path::new(rel).is_absolute() {
            return Err(AgentError::OutsideSandbox);
        }
        let joined = self.root.join(rel);
        // Lexical guard first: deny Core/.git/secret targets even when the parent
        // dir doesn't exist yet, so the denial is clear and can't be raced.
        if self.is_protected(&joined) || is_secret(&joined) {
            return Err(AgentError::Denied(
                "protected path (Core / .git / secret) — owner-only".into(),
            ));
        }
        let parent = joined.parent().ok_or(AgentError::OutsideSandbox)?;
        let parent_canon = parent.canonicalize().map_err(|_| AgentError::NotFound)?;
        if !parent_canon.starts_with(&self.root) {
            return Err(AgentError::OutsideSandbox);
        }
        let name = joined.file_name().ok_or(AgentError::OutsideSandbox)?;
        let target = parent_canon.join(name);
        // Canonical guard: catches `..`/symlink escapes into the Core or secrets.
        if is_secret(&target) || self.is_protected(&target) {
            return Err(AgentError::Denied(
                "protected path (Core / .git / secret) — owner-only".into(),
            ));
        }
        // The leaf itself is not canonicalized, so a symlink there (even a
        // dangling one) or a hard link would redirect the write past every
        // guard above.
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if !meta.file_type().is_file() || meta.nlink() > 1 => Err(AgentError::Denied(
                "write target is not a plain regular file".into(),
            )),
            Ok(meta) => Ok((target, Some(meta))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((target, None)),
            Err(_) => Err(AgentError::NotFound),
        }
    }

    /// The Core and version-control internals are never agent-writable. `jarvis-core/**`
    /// is Jarvis' constitution (Jarvis.md §30); `.git/**` is not for direct writes.
    fn is_protected(&self, path: &Path) -> bool {
        path.starts_with(self.root.join("jarvis-core"))
            || path.components().any(|c| c.as_os_str() == ".git")
    }
}

/// Secret paths are off-limits even to read, even inside the sandbox (ADR-029).
/// Matching is ASCII case-insensitive on the lossy name, so neither case
/// variants nor non-UTF-8 names slip through.
fn is_secret(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name.starts_with(".env.")
        || [".env", ".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|ext| name.ends_with(ext))
        || ["id_rsa", "id_ed25519", "id_ecdsa"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || name == ".netrc"
        || name == ".git-credentials"
        || path.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c.eq_ignore_ascii_case(".ssh") || c.eq_ignore_ascii_case(".git")
        })
}

/// The result of a successful action.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub action_type: String,
    pub output: String,
    pub truncated: bool,
}

/// Run a read-only action inside the sandbox. Never mutates anything.
pub async fn execute(sandbox: &Sandbox, action: &Action) -> Result<Outcome, AgentError> {
    let at = action_type(action).to_string();
    let raw = match action {
        Action::ListDir { path } => list_dir(sandbox, path)?,
        Action::ReadFile { path } => read_file(sandbox, path)?,
        Action::Grep { pattern, path } => grep(sandbox, pattern, path).await?,
        Action::Git { sub } => git(sandbox, *sub).await?,
        Action::WriteFile { path, content } => write_file(sandbox, path, content)?,
        Action::GitCommit { message } => git_commit(sandbox, message).await?,
        Action::ClaudeCode { prompt } => claude_code(sandbox, prompt).await?,
    };
    let (output, truncated) = cap(raw);
    Ok(Outcome {
        action_type: at,
        output,
        truncated,
    })
}

/// A human-readable preview of what a mutating action would do — shown to the
/// owner before they sign the approval. Also re-validates the target path, so a
/// protected/escaping write is refused at request time, before any pending
/// action exists.
pub async fn preview(sandbox: &Sandbox, action: &Action) -> Result<String, AgentError> {
    match action {
        Action::WriteFile { path, content } => {
            let (_, existing) = sandbox.resolve_write(path)?; // enforces Core/secret denial
            let verb = if existing.is_some() {
                "OVERWRITES"
            } else {
                "new file"
            };
            let head: String = content.chars().take(2000).collect();
            Ok(format!(
                "WriteFile {path} ({verb}, {} bytes)\n--- content ---\n{head}",
                content.len()
            ))
        }
        Action::GitCommit { message } => {
            let status = git(sandbox, GitRead::Status).await?;
            Ok(format!(
                "GitCommit \"{message}\"\n--- state (git status) ---\n{}",
                if status.trim().is_empty() {
                    "(niets te committen)".into()
                } else {
                    status
                }
            ))
        }
        Action::ClaudeCode { .. } => Err(AgentError::Denied(CLAUDE_CODE_DISABLED.into())),
        // Read-only actions don't need a preview; describe them plainly.
        other => Ok(format!("{} (read-only)", action_type(other))),
    }
}

fn write_file(sandbox: &Sandbox, path: &str, content: &str) -> Result<String, AgentError> {
    use std::io::Write;

    let exec = |e: std::io::Error| AgentError::Exec(e.to_string());
    let (target, existing) = sandbox.resolve_write(path)?;
    let mut file = match existing {
        // O_EXCL never follows a symlink planted after `resolve_write`.
        None => std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(exec)?,
        // Open without truncating, then prove it is the file that was checked
        // before any byte changes: a leaf swapped for a link fails here.
        Some(checked) => {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .map_err(exec)?;
            let opened = file.metadata().map_err(exec)?;
            if opened.dev() != checked.dev() || opened.ino() != checked.ino() || opened.nlink() != 1
            {
                return Err(AgentError::Denied(
                    "write target changed while writing".into(),
                ));
            }
            file.set_len(0).map_err(exec)?;
            file
        }
    };
    // ponytail: a parent directory swapped for a symlink after `resolve_write`
    // canonicalized it is still followed; closing that needs openat2
    // (RESOLVE_BENEATH), worth it only if workspaces get concurrent untrusted writers.
    file.write_all(content.as_bytes()).map_err(exec)?;
    Ok(format!("written: {path} ({} bytes)", content.len()))
}

async fn git_commit(sandbox: &Sandbox, message: &str) -> Result<String, AgentError> {
    run_cmd(sandbox.root(), "git", &["add", "-A"]).await?;
    let out = run_cmd(sandbox.root(), "git", &["commit", "-m", message]).await?;
    Ok(out)
}

const CLAUDE_CODE_DISABLED: &str =
    "direct Claude Code execution is disabled — use the OpenSandbox provider";

/// Deny-rules for the Claude Code executor. These are enforced in *every*
/// permission mode (even `bypassPermissions`), so they — not the tool flags —
/// are the guaranteed confinement: the Core, git internals, secrets, the shell
/// (`Bash`) and the network (`WebFetch`/`WebSearch`) are blocked outright.
#[cfg(test)]
fn claude_code_settings() -> &'static str {
    concat!(
        r#"{"permissions":{"deny":["#,
        r#""Read(./jarvis-core/**)","Edit(./jarvis-core/**)","Write(./jarvis-core/**)","#,
        r#""Read(./.git/**)","Edit(./.git/**)","Write(./.git/**)","#,
        r#""Read(**/.env)","Read(**/.env.*)","Edit(**/.env)","Edit(**/.env.*)","Write(**/.env)","Write(**/.env.*)","#,
        r#""Read(**/*.pem)","Read(**/*.key)","Edit(**/*.pem)","Edit(**/*.key)","Write(**/*.pem)","Write(**/*.key)","#,
        r#""Read(**/.ssh/**)","Edit(**/.ssh/**)","Write(**/.ssh/**)","#,
        r#""Bash","WebFetch","WebSearch","Agent""#,
        r#"]}}"#
    )
}

/// Host-process Claude Code was a transitional executor. A workspace path and
/// deny-list cannot give it the process/network boundary required for untrusted
/// repositories, so fail closed until the approved OpenSandbox-backed Codex
/// broker is wired in.
async fn claude_code(_: &Sandbox, _: &str) -> Result<String, AgentError> {
    Err(AgentError::Denied(CLAUDE_CODE_DISABLED.into()))
}

/// Parse `claude -p --output-format json`. An `is_error` result becomes an
/// `Exec` error; otherwise return the assistant's final text.
#[cfg(test)]
fn parse_claude_code_output(stdout: &str) -> Result<String, AgentError> {
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| AgentError::Exec(format!("unparseable claude output: {e}")))?;
    if v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false) {
        let msg = v
            .get("result")
            .and_then(|r| r.as_str())
            .or_else(|| v.get("subtype").and_then(|s| s.as_str()))
            .unwrap_or("claude reported an error");
        return Err(AgentError::Exec(format!("claude: {msg}")));
    }
    let result = match v.get("result") {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    Ok(if result.is_empty() {
        "(claude returned no text)".to_string()
    } else {
        result
    })
}

/// Paths from `git status --short` porcelain (rename → the new path).
#[cfg(test)]
fn changed_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            if line.len() < 4 {
                return None;
            }
            let rest = line[3..].trim();
            let path = rest.rsplit(" -> ").next().unwrap_or(rest);
            Some(path.trim_matches('"').to_string())
        })
        .collect()
}

/// Any changed path that is protected (Core / `.git` / secret) — should always be
/// empty, but if not, the deny-rules failed and the owner must know immediately.
#[cfg(test)]
fn protected_breaches(sandbox: &Sandbox, paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            let joined = sandbox.root().join(p);
            sandbox.is_protected(&joined) || is_secret(&joined)
        })
        .cloned()
        .collect()
}

fn list_dir(sandbox: &Sandbox, path: &str) -> Result<String, AgentError> {
    let dir = sandbox.resolve(path)?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| AgentError::Exec(e.to_string()))?
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = e.file_name().to_string_lossy().into_owned();
            if is_dir {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    Ok(names.join("\n"))
}

fn read_file(sandbox: &Sandbox, path: &str) -> Result<String, AgentError> {
    let file = sandbox.resolve(path)?;
    let meta = std::fs::metadata(&file).map_err(|_| AgentError::NotFound)?;
    if !meta.is_file() {
        return Err(AgentError::Exec("not a regular file".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(AgentError::Denied(format!(
            "file too large ({} bytes)",
            meta.len()
        )));
    }
    std::fs::read_to_string(&file).map_err(|e| AgentError::Exec(e.to_string()))
}

/// `grep` exclusions mirroring [`is_secret`], so a recursive search never opens
/// a file that `read_file` would refuse. grep globs are case-sensitive, so
/// only the lower- and upper-case spellings are excluded here; mixed-case
/// names are still removed from the output by the `is_secret` re-check.
const GREP_SECRET_EXCLUDES: &[&str] = &[
    "--exclude=.env",
    "--exclude=.env.*",
    "--exclude=.ENV.*",
    "--exclude=*.env",
    "--exclude=*.ENV",
    "--exclude=*.pem",
    "--exclude=*.PEM",
    "--exclude=*.key",
    "--exclude=*.KEY",
    "--exclude=*.p12",
    "--exclude=*.P12",
    "--exclude=*.pfx",
    "--exclude=*.PFX",
    "--exclude=id_rsa*",
    "--exclude=id_ed25519*",
    "--exclude=id_ecdsa*",
    "--exclude=.netrc",
    "--exclude=.git-credentials",
    "--exclude-dir=.ssh",
    "--exclude-dir=.git",
];

async fn grep(sandbox: &Sandbox, pattern: &str, path: &str) -> Result<String, AgentError> {
    let target = sandbox.resolve(path)?;
    let target_arg = target.to_string_lossy();
    // `-e` guards a pattern starting with `-`; `--` ends option parsing. Args are
    // passed directly (no shell), so the pattern can't inject commands. `-r`
    // does not follow symlinks below the (canonical) target; `-D skip` never
    // reads devices, FIFOs or sockets.
    let mut args = vec!["-rnIHZ", "-D", "skip"];
    args.extend_from_slice(GREP_SECRET_EXCLUDES);
    args.extend_from_slice(&["-e", pattern, "--", &target_arg]);
    let out = run_cmd(sandbox.root(), "grep", &args).await?;
    // `-HZ` prints every file name, ending it with NUL. Keep only names under
    // the target and re-check each with `is_secret`, so the read filter stays
    // the single source of truth; a line without a name is dropped (fail
    // closed).
    Ok(out
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .filter(|(file, _)| Path::new(file).starts_with(&target) && !is_secret(Path::new(file)))
        .map(|(file, rest)| format!("{file}:{rest}\n"))
        .collect())
}

async fn git(sandbox: &Sandbox, sub: GitRead) -> Result<String, AgentError> {
    let args: &[&str] = match sub {
        GitRead::Status => &["status", "--short"],
        GitRead::Diff => &["diff", "--stat"],
        GitRead::Log => &["log", "--oneline", "-30"],
    };
    run_cmd(sandbox.root(), "git", args).await
}

/// Run a subprocess with args passed directly (no shell), inside the sandbox,
/// with a timeout. Returns stdout; falls back to stderr when stdout is empty.
async fn run_cmd(root: &Path, program: &str, args: &[&str]) -> Result<String, AgentError> {
    let mut cmd = Command::new(program);
    // Never hand Core's environment (database password, provider keys) to a
    // workspace subprocess: git reads repository config that can run commands.
    cmd.env_clear();
    for key in CHILD_ENV {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.args(args).current_dir(root).kill_on_drop(true);
    let out = tokio::time::timeout(EXEC_TIMEOUT, cmd.output())
        .await
        .map_err(|_| AgentError::Timeout)?
        .map_err(|e| AgentError::Exec(format!("{program}: {e}")))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        // grep exits 1 with empty stdout on "no matches" — not an error.
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.trim().is_empty() && program != "grep" {
            s = err.into_owned();
        }
    }
    Ok(s)
}

/// Cap output size so a huge file/log can't blow up memory or the audit trail.
fn cap(mut s: String) -> (String, bool) {
    if s.len() <= MAX_OUTPUT {
        return (s, false);
    }
    // `truncate` panics inside a multi-byte character: cut at a boundary.
    let mut end = MAX_OUTPUT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_sandbox(name: &str) -> (Sandbox, PathBuf) {
        let dir = std::env::temp_dir().join(format!("jarvis_agent_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        (Sandbox::new(&dir).unwrap(), dir)
    }

    #[test]
    fn rejects_absolute_and_escape() {
        let (sb, _dir) = temp_sandbox("escape");
        assert!(matches!(
            sb.resolve("/etc/passwd"),
            Err(AgentError::OutsideSandbox)
        ));
        // ".." exists (the parent) but is outside the root.
        assert!(matches!(sb.resolve(".."), Err(AgentError::OutsideSandbox)));
    }

    #[test]
    fn denies_secret_paths() {
        let (sb, dir) = temp_sandbox("secret");
        std::fs::write(dir.join(".env"), "JARVIS_LLM_API_KEY=sk-secret").unwrap();
        assert!(matches!(sb.resolve(".env"), Err(AgentError::Denied(_))));
        assert!(is_secret(Path::new("/x/id_rsa")));
        assert!(is_secret(Path::new("/home/.ssh/known_hosts")));
        assert!(!is_secret(Path::new("/x/main.rs")));
        for secret in [
            "/x/.ENV",
            "/x/.Env.Local",
            "/x/prod.env",
            "/x/Cert.PEM",
            "/x/tls.Key",
            "/x/store.p12",
            "/x/store.PFX",
            "/x/ID_ECDSA",
            "/x/.netrc",
            "/x/.git-credentials",
            "/r/.git/config",
            "/r/.GIT/HEAD",
            "/r/.SSH/config",
        ] {
            assert!(is_secret(Path::new(secret)), "{secret}");
        }
        for public in ["/x/.gitignore", "/x/.github/ci.yml", "/x/keyboard.rs"] {
            assert!(!is_secret(Path::new(public)), "{public}");
        }
        // A non-UTF-8 name cannot bypass the filter.
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/x/\xffcert.PEM");
        assert!(is_secret(Path::new(raw)));
    }

    #[tokio::test]
    async fn reads_a_file_and_lists_a_dir() {
        let (sb, dir) = temp_sandbox("read");
        std::fs::write(dir.join("hello.txt"), "hoi Jarvis").unwrap();
        let out = execute(
            &sb,
            &Action::ReadFile {
                path: "hello.txt".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(out.output, "hoi Jarvis");
        assert!(!out.truncated);

        let listed = execute(&sb, &Action::ListDir { path: ".".into() })
            .await
            .unwrap();
        assert!(listed.output.contains("hello.txt"));
    }

    #[tokio::test]
    async fn reading_a_secret_is_denied_end_to_end() {
        let (sb, dir) = temp_sandbox("read_secret");
        std::fs::write(dir.join(".env"), "SECRET=1").unwrap();
        let err = execute(
            &sb,
            &Action::ReadFile {
                path: ".env".into(),
            },
        )
        .await;
        assert!(matches!(err, Err(AgentError::Denied(_))));
    }

    #[test]
    fn read_only_is_auto_mutating_needs_approval() {
        assert_eq!(
            classify(&Action::ListDir { path: ".".into() }),
            RiskClass::Auto
        );
        assert_eq!(
            action_type(&Action::Git {
                sub: GitRead::Status
            }),
            "git_status"
        );
        assert_eq!(
            classify(&Action::WriteFile {
                path: "a.txt".into(),
                content: "x".into()
            }),
            RiskClass::NeedsApproval
        );
        assert!(is_mutating(&Action::GitCommit {
            message: "m".into()
        }));
    }

    /// The auto-vs-approval decision comes from `jarvis-policy` (review P2): every
    /// mutating capability resolves to `RequireApproval`, reads to `Allow`, and
    /// the agent's `RiskClass` adapter must agree with a direct policy call.
    #[test]
    fn classify_is_consistent_with_jarvis_policy() {
        use jarvis_policy::{
            decide, Capability, PolicyContext, PolicyDecision, RiskClass as PRisk,
        };
        let ctx = |cap, risk| PolicyContext {
            capability: cap,
            risk,
            trusted_device: true,
            approved: false,
            reversible: false,
        };
        // Reads → Allow → Auto.
        assert_eq!(
            decide(&ctx(Capability::ReadData, PRisk::ReadOnly)),
            PolicyDecision::Allow
        );
        assert_eq!(
            classify(&Action::ReadFile { path: "a".into() }),
            RiskClass::Auto
        );
        // Code execution → RequireApproval → NeedsApproval.
        assert_eq!(
            decide(&ctx(Capability::ExecuteCode, PRisk::Mutating)),
            PolicyDecision::RequireApproval
        );
        assert_eq!(
            classify(&Action::ClaudeCode { prompt: "x".into() }),
            RiskClass::NeedsApproval
        );
        // File writes → RequireApproval → NeedsApproval.
        assert_eq!(
            decide(&ctx(Capability::ManageFiles, PRisk::Mutating)),
            PolicyDecision::RequireApproval
        );
    }

    #[tokio::test]
    async fn writes_a_file_but_never_the_core_or_secrets() {
        let (sb, dir) = temp_sandbox("write");
        std::fs::create_dir_all(dir.join("jarvis-core")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        // Happy path: write into the sandbox.
        let ok = execute(
            &sb,
            &Action::WriteFile {
                path: "note.txt".into(),
                content: "hoi".into(),
            },
        )
        .await;
        assert!(ok.is_ok());
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "hoi"
        );

        // The Core is never writable — not even with a valid path.
        let core = execute(
            &sb,
            &Action::WriteFile {
                path: "jarvis-core/Jarvis.md".into(),
                content: "hack".into(),
            },
        )
        .await;
        assert!(
            matches!(core, Err(AgentError::Denied(_))),
            "core write must be denied"
        );

        // .git internals, secrets, and escapes are denied too.
        assert!(matches!(
            sb.resolve_write("jarvis-core/anything.md"),
            Err(AgentError::Denied(_))
        ));
        assert!(matches!(
            sb.resolve_write(".git/config"),
            Err(AgentError::Denied(_))
        ));
        assert!(matches!(
            sb.resolve_write(".env"),
            Err(AgentError::Denied(_))
        ));
        assert!(matches!(
            sb.resolve_write("/etc/passwd"),
            Err(AgentError::OutsideSandbox)
        ));
        assert!(matches!(
            sb.resolve_write("/etc/jarvis/Jarvis.md"),
            Err(AgentError::OutsideSandbox)
        ));

        // And the Core stays untouched on disk.
        assert!(!dir.join("jarvis-core/Jarvis.md").exists());
    }

    #[tokio::test]
    async fn write_never_follows_a_leaf_link_or_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(workspace.path()).unwrap();
        let dir = sb.root().to_path_buf();
        std::fs::create_dir_all(dir.join(".git/hooks")).unwrap();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        std::fs::write(outside.path().join("existing"), "keep").unwrap();
        std::fs::write(outside.path().join("victim"), "keep").unwrap();
        let link = std::os::unix::fs::symlink;
        link(outside.path().join("pwn"), dir.join("out.txt")).unwrap();
        link(dir.join(".git/hooks/pre-commit"), dir.join("hook.txt")).unwrap();
        link(outside.path().join("existing"), dir.join("existing.txt")).unwrap();
        std::fs::hard_link(outside.path().join("victim"), dir.join("hard.txt")).unwrap();

        for name in ["out.txt", "hook.txt", "existing.txt", "hard.txt", "subdir"] {
            let action = Action::WriteFile {
                path: name.into(),
                content: "payload".into(),
            };
            assert!(
                matches!(preview(&sb, &action).await, Err(AgentError::Denied(_))),
                "preview must refuse {name}"
            );
            assert!(
                matches!(execute(&sb, &action).await, Err(AgentError::Denied(_))),
                "write must refuse {name}"
            );
        }
        assert!(!outside.path().join("pwn").exists());
        assert!(!dir.join(".git/hooks/pre-commit").exists());
        for kept in ["existing", "victim"] {
            assert_eq!(
                std::fs::read_to_string(outside.path().join(kept)).unwrap(),
                "keep"
            );
        }
    }

    #[tokio::test]
    async fn write_overwrites_an_existing_regular_file() {
        let workspace = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(workspace.path()).unwrap();
        let dir = sb.root().to_path_buf();
        std::fs::write(dir.join("note.txt"), "old content").unwrap();
        let action = Action::WriteFile {
            path: "note.txt".into(),
            content: "new".into(),
        };
        assert!(preview(&sb, &action).await.unwrap().contains("OVERWRITES"));
        execute(&sb, &action).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("note.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn subprocesses_get_only_the_allowlisted_environment() {
        let dir = tempfile::tempdir().unwrap();
        let out = run_cmd(dir.path(), "env", &[]).await.unwrap();
        for line in out.lines() {
            let key = line.split('=').next().unwrap_or_default();
            assert!(CHILD_ENV.contains(&key), "unexpected variable {key}");
        }
    }

    #[tokio::test]
    async fn preview_of_a_write_shows_the_content_and_denies_the_core() {
        let (sb, _dir) = temp_sandbox("preview");
        let p = preview(
            &sb,
            &Action::WriteFile {
                path: "x.txt".into(),
                content: "content".into(),
            },
        )
        .await
        .unwrap();
        assert!(p.contains("content"));
        let denied = preview(
            &sb,
            &Action::WriteFile {
                path: "jarvis-core/x.md".into(),
                content: "y".into(),
            },
        )
        .await;
        assert!(matches!(denied, Err(AgentError::Denied(_))));
    }

    #[test]
    fn cap_truncates_large_output() {
        let (s, truncated) = cap("x".repeat(MAX_OUTPUT + 100));
        assert!(truncated);
        assert_eq!(s.len(), MAX_OUTPUT);
    }

    #[test]
    fn cap_cuts_on_a_char_boundary() {
        // A 3-byte character straddles the cap.
        let s = format!("{}€tail", "x".repeat(MAX_OUTPUT - 1));
        let (s, truncated) = cap(s);
        assert!(truncated);
        assert_eq!(s.len(), MAX_OUTPUT - 1);
        assert!(s.bytes().all(|b| b == b'x'));
    }

    #[tokio::test]
    async fn grep_never_returns_secret_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sb = Sandbox::new(dir).unwrap();
        std::fs::create_dir_all(dir.join("sub/.ssh")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        for secret in [
            ".env",
            ".env.local",
            "prod.env",
            "sub/cert.pem",
            "sub/Mixed.Key",
            "sub/store.p12",
            "id_rsa",
            "sub/id_ed25519.pub",
            "sub/.ssh/config",
            ".netrc",
            ".git-credentials",
            ".git/config",
        ] {
            std::fs::write(dir.join(secret), "TOKEN=leak\n").unwrap();
        }
        std::fs::write(dir.join("sub/notes.txt"), "TOKEN=public\n").unwrap();
        let out = execute(
            &sb,
            &Action::Grep {
                pattern: "TOKEN".into(),
                path: ".".into(),
            },
        )
        .await
        .unwrap()
        .output;
        assert!(!out.contains("leak"), "secret content leaked: {out}");
        assert!(out.contains("notes.txt:1:TOKEN=public"), "{out}");

        // A single-file target still reports its matches.
        let one = execute(
            &sb,
            &Action::Grep {
                pattern: "TOKEN".into(),
                path: "sub/notes.txt".into(),
            },
        )
        .await
        .unwrap()
        .output;
        assert!(one.contains("notes.txt:1:TOKEN=public"), "{one}");
    }

    // ---- 4c: Claude Code executor -----------------------------------------

    #[test]
    fn claude_code_is_mutating_and_labeled() {
        let a = Action::ClaudeCode {
            prompt: "fix the bug".into(),
        };
        assert_eq!(classify(&a), RiskClass::NeedsApproval);
        assert!(is_mutating(&a));
        assert_eq!(action_type(&a), "claude_code");
    }

    #[tokio::test]
    async fn claude_code_denied_when_executor_disabled() {
        // A sandbox *without* `with_claude_code` must refuse the action before
        // ever spawning a process — the second opt-in is required.
        let (sb, _dir) = temp_sandbox("cc_disabled");
        let err = execute(
            &sb,
            &Action::ClaudeCode {
                prompt: "do it".into(),
            },
        )
        .await;
        assert!(matches!(err, Err(AgentError::Denied(_))));
    }

    #[tokio::test]
    async fn preview_claude_code_is_denied_even_with_legacy_config() {
        let (dir, _p) = temp_sandbox("cc_preview");
        let sb = dir.with_claude_code(ClaudeCodeCfg {
            bin: "claude".into(),
            model: String::new(),
        });
        let denied = preview(
            &sb,
            &Action::ClaudeCode {
                prompt: "refactor module X".into(),
            },
        )
        .await;
        assert!(matches!(denied, Err(AgentError::Denied(_))));

        // Disabled executor ⇒ even the preview is denied (nothing to sign).
        let (bare, _d) = temp_sandbox("cc_preview_off");
        let bare_denied = preview(&bare, &Action::ClaudeCode { prompt: "x".into() }).await;
        assert!(matches!(bare_denied, Err(AgentError::Denied(_))));
    }

    #[test]
    fn claude_code_settings_are_valid_json_and_deny_the_core_and_shell() {
        let v: serde_json::Value = serde_json::from_str(claude_code_settings()).unwrap();
        let deny = v["permissions"]["deny"].as_array().unwrap();
        let rules: Vec<&str> = deny.iter().filter_map(|r| r.as_str()).collect();
        assert!(rules.iter().any(|r| r.contains("jarvis-core/")));
        assert!(rules.contains(&"Bash"));
        assert!(rules.contains(&"WebFetch"));
        assert!(rules.iter().any(|r| r.contains(".env")));
    }

    #[test]
    fn parses_claude_code_success_and_error() {
        let ok = parse_claude_code_output(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done: edited 2 files"}"#,
        )
        .unwrap();
        assert_eq!(ok, "done: edited 2 files");

        let err = parse_claude_code_output(
            r#"{"is_error":true,"subtype":"error_max_turns","result":"limit reached"}"#,
        );
        assert!(matches!(err, Err(AgentError::Exec(m)) if m.contains("limit reached")));
    }

    #[test]
    fn changed_paths_and_protected_breaches() {
        let (sb, _dir) = temp_sandbox("cc_breach");
        let porcelain = " M src/main.rs\n?? note.txt\nR  old.rs -> jarvis-core/hack.rs\n M .env\n";
        let paths = changed_paths(porcelain);
        assert!(paths.contains(&"src/main.rs".to_string()));
        assert!(paths.contains(&"jarvis-core/hack.rs".to_string())); // rename target
        assert!(paths.contains(&".env".to_string()));

        let breaches = protected_breaches(&sb, &paths);
        assert!(breaches.contains(&"jarvis-core/hack.rs".to_string()));
        assert!(breaches.contains(&".env".to_string()));
        assert!(!breaches.contains(&"src/main.rs".to_string()));
    }
}
