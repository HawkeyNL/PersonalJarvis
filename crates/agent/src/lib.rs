//! Agentic execution — phase 4a: read-only, policy-gated, sandboxed (ADR-029).
//!
//! Jarvis' first "hands", deliberately minimal and safe:
//! - **Typed allowlist, no free shell.** Only the [`Action`] variants exist; a
//!   request that doesn't map to one can't be represented, let alone run.
//! - **Read-only only.** List/read/grep/git-status — nothing mutates. cargo/tests
//!   (which compile + run code) are intentionally NOT here yet.
//! - **Sandboxed.** Every path is resolved inside a single workspace root; no
//!   escape (`..`/symlinks), no secrets (`.env`, keys, `.ssh`) — even to read.
//! - **Off by default + audited.** The kill switch (`JARVIS_AGENT_ENABLED`) and
//!   the audit log live in the API; this crate is the pure policy + executor.
//!
//! The Core (`core/**`, policy, secrets) is never even representable here — there
//! is no write action at all in 4a, and secret paths are denied on read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

const MAX_OUTPUT: usize = 64 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Classify an action (ADR-029 laag 2). Read-only ⇒ `Auto`; mutating ⇒
/// `NeedsApproval` (4b, behind a device-signed approval). Path-level denials
/// (escape/secret/Core) are enforced separately at resolution.
pub fn classify(action: &Action) -> RiskClass {
    match action {
        Action::WriteFile { .. } | Action::GitCommit { .. } => RiskClass::NeedsApproval,
        _ => RiskClass::Auto,
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

/// A confined workspace: all file access resolves inside `root` (canonicalized).
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Build a sandbox from a workspace root. Fails if the root doesn't exist.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, AgentError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| AgentError::NoWorkspace)?;
        Ok(Self { root })
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
    /// secret, or inside `.git`. This is the hard guarantee that Jarvis can't
    /// rewrite its own rules (ADR-029 / Jarvis.md §30).
    fn resolve_write(&self, rel: &str) -> Result<PathBuf, AgentError> {
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
        let parent_canon = parent
            .canonicalize()
            .map_err(|_| AgentError::NotFound)?;
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
        Ok(target)
    }

    /// The Core and version-control internals are never agent-writable. `core/**`
    /// is Jarvis' constitution (Jarvis.md §30); `.git/**` is not for direct writes.
    fn is_protected(&self, path: &Path) -> bool {
        path.starts_with(self.root.join("core"))
            || path.components().any(|c| c.as_os_str() == ".git")
    }
}

/// Secret paths are off-limits even to read, even inside the sandbox (ADR-029).
fn is_secret(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || path.components().any(|c| c.as_os_str() == ".ssh")
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
            let target = sandbox.resolve_write(path)?; // enforces Core/secret denial
            let verb = if target.exists() { "OVERSCHRIJFT" } else { "nieuw bestand" };
            let head: String = content.chars().take(2000).collect();
            Ok(format!(
                "WriteFile {path} ({verb}, {} bytes)\n--- inhoud ---\n{head}",
                content.len()
            ))
        }
        Action::GitCommit { message } => {
            let status = git(sandbox, GitRead::Status).await?;
            Ok(format!(
                "GitCommit \"{message}\"\n--- staat (git status) ---\n{}",
                if status.trim().is_empty() {
                    "(niets te committen)".into()
                } else {
                    status
                }
            ))
        }
        // Read-only actions don't need a preview; describe them plainly.
        other => Ok(format!("{} (alleen-lezen)", action_type(other))),
    }
}

fn write_file(sandbox: &Sandbox, path: &str, content: &str) -> Result<String, AgentError> {
    let target = sandbox.resolve_write(path)?;
    std::fs::write(&target, content).map_err(|e| AgentError::Exec(e.to_string()))?;
    Ok(format!("geschreven: {path} ({} bytes)", content.len()))
}

async fn git_commit(sandbox: &Sandbox, message: &str) -> Result<String, AgentError> {
    run_cmd(sandbox.root(), "git", &["add", "-A"]).await?;
    let out = run_cmd(sandbox.root(), "git", &["commit", "-m", message]).await?;
    Ok(out)
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
    if meta.is_dir() {
        return Err(AgentError::Exec("path is a directory".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(AgentError::Denied(format!(
            "file too large ({} bytes)",
            meta.len()
        )));
    }
    std::fs::read_to_string(&file).map_err(|e| AgentError::Exec(e.to_string()))
}

async fn grep(sandbox: &Sandbox, pattern: &str, path: &str) -> Result<String, AgentError> {
    let target = sandbox.resolve(path)?;
    // `-e` guards a pattern starting with `-`; `--` ends option parsing. Args are
    // passed directly (no shell), so the pattern can't inject commands.
    run_cmd(
        sandbox.root(),
        "grep",
        &[
            "-rnI",
            "-e",
            pattern,
            "--",
            &target.to_string_lossy(),
        ],
    )
    .await
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
    s.truncate(MAX_OUTPUT);
    // Trim to a char boundary.
    while !s.is_char_boundary(s.len()) {
        s.pop();
    }
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
        assert!(matches!(sb.resolve("/etc/passwd"), Err(AgentError::OutsideSandbox)));
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
    }

    #[tokio::test]
    async fn reads_a_file_and_lists_a_dir() {
        let (sb, dir) = temp_sandbox("read");
        std::fs::write(dir.join("hello.txt"), "hoi Jarvis").unwrap();
        let out = execute(&sb, &Action::ReadFile { path: "hello.txt".into() })
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
        let err = execute(&sb, &Action::ReadFile { path: ".env".into() }).await;
        assert!(matches!(err, Err(AgentError::Denied(_))));
    }

    #[test]
    fn read_only_is_auto_mutating_needs_approval() {
        assert_eq!(classify(&Action::ListDir { path: ".".into() }), RiskClass::Auto);
        assert_eq!(action_type(&Action::Git { sub: GitRead::Status }), "git_status");
        assert_eq!(
            classify(&Action::WriteFile { path: "a.txt".into(), content: "x".into() }),
            RiskClass::NeedsApproval
        );
        assert!(is_mutating(&Action::GitCommit { message: "m".into() }));
    }

    #[tokio::test]
    async fn writes_a_file_but_never_the_core_or_secrets() {
        let (sb, dir) = temp_sandbox("write");
        std::fs::create_dir_all(dir.join("core")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        // Happy path: write into the sandbox.
        let ok = execute(&sb, &Action::WriteFile { path: "note.txt".into(), content: "hoi".into() }).await;
        assert!(ok.is_ok());
        assert_eq!(std::fs::read_to_string(dir.join("note.txt")).unwrap(), "hoi");

        // The Core is never writable — not even with a valid path.
        let core = execute(&sb, &Action::WriteFile { path: "core/Jarvis.md".into(), content: "hack".into() }).await;
        assert!(matches!(core, Err(AgentError::Denied(_))), "core write must be denied");

        // .git internals, secrets, and escapes are denied too.
        assert!(matches!(sb.resolve_write("core/anything.md"), Err(AgentError::Denied(_))));
        assert!(matches!(sb.resolve_write(".git/config"), Err(AgentError::Denied(_))));
        assert!(matches!(sb.resolve_write(".env"), Err(AgentError::Denied(_))));
        assert!(matches!(sb.resolve_write("/etc/passwd"), Err(AgentError::OutsideSandbox)));

        // And the Core stays untouched on disk.
        assert!(!dir.join("core/Jarvis.md").exists());
    }

    #[tokio::test]
    async fn preview_of_a_write_shows_the_content_and_denies_the_core() {
        let (sb, _dir) = temp_sandbox("preview");
        let p = preview(&sb, &Action::WriteFile { path: "x.txt".into(), content: "inhoud".into() })
            .await
            .unwrap();
        assert!(p.contains("inhoud"));
        let denied = preview(&sb, &Action::WriteFile { path: "core/x.md".into(), content: "y".into() }).await;
        assert!(matches!(denied, Err(AgentError::Denied(_))));
    }

    #[test]
    fn cap_truncates_large_output() {
        let (s, truncated) = cap("x".repeat(MAX_OUTPUT + 100));
        assert!(truncated);
        assert_eq!(s.len(), MAX_OUTPUT);
    }
}
