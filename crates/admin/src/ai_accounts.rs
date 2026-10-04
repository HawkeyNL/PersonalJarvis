//! Owner-operated subscription login. This module deliberately has no Core API,
//! agent, MCP or sandbox entry point. Official clients own their auth stores.
use std::{
    ffi::{CStr, CString, OsString},
    fs,
    io::{self, IsTerminal, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;

mod runtime;

const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
const ENV: &str = "/usr/bin/env";
/// Bound on symlink hops while resolving a fixed system executable.
const SYSTEM_LINK_HOPS: usize = 8;
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum AccountProvider {
    Claude,
    Codex,
    /// The text-only Codex chat worker's own ChatGPT login, separate from the
    /// coding broker's `codex` login.
    CodexChat,
}

impl AccountProvider {
    fn all() -> [Self; 3] {
        [Self::Claude, Self::Codex, Self::CodexChat]
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::CodexChat => "codex-chat",
        }
    }

    fn user(self) -> &'static str {
        match self {
            Self::Claude => "jarvis-claude",
            Self::Codex => "jarvis-codex",
            Self::CodexChat => "jarvis-codex-chat",
        }
    }

    fn home(self) -> &'static str {
        match self {
            Self::Claude => "/var/lib/jarvis-claude",
            Self::Codex => "/var/lib/jarvis-codex",
            Self::CodexChat => "/var/lib/jarvis-codex-chat",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Claude => "/usr/local/bin/claude",
            Self::Codex | Self::CodexChat => "/usr/local/bin/codex",
        }
    }

    fn status_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "status"],
            Self::Codex | Self::CodexChat => &["login", "status"],
        }
    }

    fn connect_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "login"],
            Self::Codex | Self::CodexChat => &["login", "--device-auth"],
        }
    }

    fn disconnect_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "logout"],
            Self::Codex | Self::CodexChat => &["logout"],
        }
    }
}

#[derive(Debug, Subcommand)]
pub(super) enum AccountsCommand {
    /// Show only sanitized subscription connection states.
    List,
    Status {
        provider: AccountProvider,
    },
    /// Open the provider's official login flow in this controlling terminal.
    Connect {
        provider: AccountProvider,
    },
    /// Non-generative official-client authentication check.
    Test {
        provider: AccountProvider,
    },
    /// Explicitly start the official login flow again; never automatic.
    Reconnect {
        provider: AccountProvider,
    },
    /// Log out the dedicated service identity after explicit confirmation.
    Disconnect {
        provider: AccountProvider,
    },
    /// Inspect, install or roll back the official provider CLI runtime.
    Runtime {
        #[command(subcommand)]
        command: runtime::RuntimeCommand,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AccountStatus {
    provider: &'static str,
    worker: &'static str,
    state: &'static str,
    billing: &'static str,
    runtime: &'static str,
}

impl AccountStatus {
    fn new(provider: AccountProvider, state: &'static str) -> Self {
        Self {
            provider: provider.name(),
            worker: provider.user(),
            state,
            // The CLI confirms the login type, not the account-level extra
            // usage setting. Never represent that as verified zero overage.
            billing: if state == "connected" && provider == AccountProvider::Claude {
                "overage_unverified"
            } else if state == "connected" {
                "subscription"
            } else {
                "unverified"
            },
            runtime: "inactive",
        }
    }
}

pub(super) fn run(command: AccountsCommand, json: bool) -> Result<()> {
    let audit = match &command {
        AccountsCommand::Connect { provider } => Some((*provider, "connect")),
        AccountsCommand::Reconnect { provider } => Some((*provider, "reconnect")),
        AccountsCommand::Disconnect { provider } => Some((*provider, "disconnect")),
        AccountsCommand::Test { provider } => Some((*provider, "test")),
        // Runtime commands record their own version-bearing audit events.
        AccountsCommand::List
        | AccountsCommand::Status { .. }
        | AccountsCommand::Runtime { .. } => None,
    };
    if let Some((provider, action)) = audit {
        audit_account_event(provider, action, "initiated")?;
    }
    let result = (|| -> Result<()> {
        match command {
            AccountsCommand::List => {
                let statuses = AccountProvider::all().map(status);
                if json {
                    println!("{}", serde_json::to_string(&statuses)?);
                } else {
                    for item in statuses {
                        println!("{:<11} {:<20} {}", item.provider, item.worker, item.state);
                    }
                }
                Ok(())
            }
            AccountsCommand::Status { provider } => {
                let result = status(provider);
                if json {
                    println!("{}", serde_json::to_string(&result)?);
                } else {
                    println!("{}: {} ({})", result.provider, result.state, result.billing);
                }
                Ok(())
            }
            AccountsCommand::Test { provider } => {
                let result = status(provider);
                if json {
                    println!("{}", serde_json::to_string(&result)?);
                } else {
                    println!("{}: {} ({})", result.provider, result.state, result.billing);
                }
                if result.state != "connected" {
                    bail!("{} subscription connection is not usable", provider.name());
                }
                Ok(())
            }
            AccountsCommand::Connect { provider } | AccountsCommand::Reconnect { provider } => {
                if json {
                    bail!("account linking requires a trusted interactive terminal");
                }
                require_tty()?;
                validate_root_executable(provider.binary())?;
                ensure_service_identity(provider)?;
                if provider == AccountProvider::Claude {
                    ensure_claude_runtime_compatible()?;
                }
                let mut child = worker_command(provider, provider.connect_args(), true)?;
                child
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit());
                let result = child.status().context("start official provider login")?;
                if !result.success() {
                    bail!("{} account linking did not complete", provider.name());
                }
                let result = status(provider);
                if result.state != "connected" {
                    bail!(
                        "{} login finished but subscription authentication was not verified",
                        provider.name()
                    );
                }
                println!("{} subscription connection verified", provider.name());
                Ok(())
            }
            AccountsCommand::Runtime { command } => runtime::run(command, json),
            AccountsCommand::Disconnect { provider } => {
                if json {
                    bail!("account disconnection requires a trusted interactive terminal");
                }
                require_tty()?;
                println!(
                    "Disconnect {} subscription for {}? Type YES to continue:",
                    provider.name(),
                    provider.user()
                );
                let mut confirmation = String::new();
                io::stdin().read_line(&mut confirmation)?;
                if confirmation.trim() != "YES" {
                    bail!("account left unchanged");
                }
                // Block new runs and stop any resident provider process before
                // removing credentials. An explicit future Connect is still
                // required; logout never triggers a silent reconnect.
                stop_subscription_runtime(provider)?;
                let mut child = worker_command(provider, provider.disconnect_args(), false)?;
                child
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if !child
                    .status()
                    .context("start official provider logout")?
                    .success()
                {
                    bail!("{} logout did not complete", provider.name());
                }
                if status(provider).state == "connected" {
                    bail!("{} still reports connected after logout", provider.name());
                }
                println!("{} subscription disconnected", provider.name());
                Ok(())
            }
        }
    })();
    if let Some((provider, action)) = audit {
        audit_account_event(
            provider,
            action,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            },
        )
        .context("account action may have completed but its audit event failed")?;
    }
    result
}

fn audit_account_event(provider: AccountProvider, action: &str, outcome: &str) -> Result<()> {
    audit_record(&format!(
        "provider={} action={} outcome={}",
        provider.name(),
        action,
        outcome
    ))
}

/// Records only fixed, caller-validated `key=value` fields.
fn audit_record(record: &str) -> Result<()> {
    validate_system_executable("/usr/bin/logger")?;
    let result = Command::new("/usr/bin/logger")
        .args([
            "--tag",
            "jarvis-accounts",
            "--priority",
            "authpriv.notice",
            "--",
            record,
        ])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !result.success() {
        bail!("account audit event could not be recorded");
    }
    Ok(())
}

fn require_tty() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        bail!("account mutation requires a controlling terminal");
    }
    Ok(())
}

fn stop_subscription_runtime(provider: AccountProvider) -> Result<()> {
    validate_system_executable("/usr/bin/systemctl")?;
    for args in runtime_stop_commands(provider) {
        let result = Command::new("/usr/bin/systemctl")
            .args(*args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !result.success() {
            bail!(
                "{} runtime could not be stopped before logout",
                provider.name()
            );
        }
    }
    Ok(())
}

fn runtime_stop_commands(provider: AccountProvider) -> &'static [&'static [&'static str]] {
    match provider {
        AccountProvider::Claude => &[
            &["disable", "--now", "jarvis-claude.socket"],
            &["stop", "jarvis-claude.service"],
        ],
        AccountProvider::Codex => &[
            &["disable", "--now", "jarvis-codex-broker.service"],
            &["disable", "--now", "jarvis-codex.service"],
        ],
        AccountProvider::CodexChat => &[
            &["disable", "--now", "jarvis-codex-chat.socket"],
            &["stop", "jarvis-codex-chat.service"],
        ],
    }
}

fn ensure_service_identity(provider: AccountProvider) -> Result<()> {
    if service_identity(provider).is_err() {
        validate_system_executable("/usr/sbin/useradd")?;
        let result = Command::new("/usr/sbin/useradd")
            .args([
                "--system",
                "--user-group",
                "--home-dir",
                provider.home(),
                "--shell",
                "/usr/sbin/nologin",
                provider.user(),
            ])
            .env_clear()
            .status()?;
        if !result.success() {
            bail!("dedicated subscription identity could not be created");
        }
    }
    let (uid, gid) = service_identity(provider)?;
    let state = Path::new(provider.home());
    if !state.exists() && !state.is_symlink() {
        let parent = fs::symlink_metadata("/var/lib")?;
        if !parent.is_dir()
            || parent.file_type().is_symlink()
            || parent.uid() != 0
            || parent.permissions().mode() & 0o022 != 0
        {
            bail!("unsafe subscription state parent");
        }
        fs::create_dir(state)?;
        fs::set_permissions(state, fs::Permissions::from_mode(0o700))?;
        let path = CString::new(provider.home())?;
        if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
            bail!("dedicated subscription state ownership could not be set");
        }
    }
    validate_state_directory(provider.home(), uid, gid)
}

fn status(provider: AccountProvider) -> AccountStatus {
    // A rejected host tool is not a missing provider runtime.
    if validate_worker_host().is_err() {
        return AccountStatus::new(provider, "host_unsupported");
    }
    if provider == AccountProvider::Claude {
        let Ok(command) = worker_command(provider, &["--version"], false) else {
            return AccountStatus::new(provider, "runtime_missing");
        };
        if !claude_version_command_compatible(command) {
            return AccountStatus::new(provider, "incompatible_runtime");
        }
    }
    let Ok(mut command) = worker_command(provider, provider.status_args(), false) else {
        return AccountStatus::new(provider, "runtime_missing");
    };
    let Ok(Some(output)) = bounded_status_output(&mut command) else {
        return AccountStatus::new(provider, "unhealthy");
    };
    let state = match provider {
        AccountProvider::Claude => parse_claude_status(&output),
        AccountProvider::Codex | AccountProvider::CodexChat => parse_codex_status(&output),
    };
    let mut result = AccountStatus::new(provider, state);
    result.runtime = runtime_state(provider);
    result
}

/// Reports why connect cannot proceed: a worker that cannot be prepared or
/// run is not a CLI outside the reviewed version contract.
fn ensure_claude_runtime_compatible() -> Result<()> {
    let mut command = worker_command(AccountProvider::Claude, &["--version"], false)
        .context("Claude worker could not be prepared")?;
    let output = bounded_status_output(&mut command)
        .context("Claude version check could not run")?
        .context("Claude version check did not complete")?;
    if !jarvis_llm::claude_worker_protocol::reviewed_claude_version(output.as_bytes()) {
        bail!("Claude runtime version is incompatible with the reviewed worker flags");
    }
    Ok(())
}

fn claude_version_command_compatible(mut command: Command) -> bool {
    bounded_status_output(&mut command)
        .ok()
        .flatten()
        .is_some_and(|output| {
            jarvis_llm::claude_worker_protocol::reviewed_claude_version(output.as_bytes())
        })
}

fn runtime_state(provider: AccountProvider) -> &'static str {
    if validate_system_executable("/usr/bin/systemctl").is_err() {
        return "unavailable";
    }
    let active = |unit: &str| {
        Command::new("/usr/bin/systemctl")
            .args(["is-active", "--quiet", unit])
            .env_clear()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    match provider {
        AccountProvider::Claude => {
            if active("jarvis-claude.service") {
                "active"
            } else if active("jarvis-claude.socket") {
                "socket_ready"
            } else {
                "inactive"
            }
        }
        AccountProvider::Codex => {
            if active("jarvis-codex.service") {
                "active"
            } else {
                "inactive"
            }
        }
        AccountProvider::CodexChat => {
            if active("jarvis-codex-chat.service") {
                "active"
            } else if active("jarvis-codex-chat.socket") {
                "socket_ready"
            } else {
                "inactive"
            }
        }
    }
}

fn parse_claude_status(output: &str) -> &'static str {
    jarvis_llm::claude_worker_protocol::claude_subscription_status(output.as_bytes())
}

fn parse_codex_status(output: &str) -> &'static str {
    jarvis_llm::codex_chat_protocol::codex_subscription_status(output.as_bytes())
}

fn bounded_status_output(command: &mut Command) -> Result<Option<String>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().context("missing status output")?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(STATUS_LIMIT + 1).read_to_end(&mut bytes)?;
        Ok::<_, io::Error>(bytes)
    });
    let deadline = Instant::now() + STATUS_TIMEOUT;
    let success = loop {
        if let Some(result) = child.try_wait()? {
            break result.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("status reader failed"))??;
    if !success {
        return Ok(None);
    }
    if bytes.len() as u64 > STATUS_LIMIT {
        bail!("provider status exceeded safe bound");
    }
    Ok(Some(String::from_utf8(bytes)?))
}

fn worker_command(provider: AccountProvider, args: &[&str], interactive: bool) -> Result<Command> {
    validate_worker_host()?;
    validate_root_executable(provider.binary())?;
    let (uid, gid) = service_identity(provider)?;
    validate_state_directory(provider.home(), uid, gid)?;
    let runtime_limit = if interactive {
        "10min"
    } else if args == provider.status_args() {
        "8s"
    } else {
        "1min"
    };
    // The official client executes in a transient *system* service, not as a
    // setuid child in the administrator's host mount namespace. In particular,
    // ProtectHome and InaccessiblePaths prevent reading the owner's home and
    // Core provider secrets even if the CLI or a plugin behaves unexpectedly.
    let mut command = Command::new(SYSTEMD_RUN);
    command
        .arg("--quiet")
        .arg("--wait")
        .arg("--collect")
        .arg("--expand-environment=no")
        .arg(if interactive { "--pty" } else { "--pipe" })
        .arg(format!("--uid={}", provider.user()))
        .arg(format!("--gid={}", provider.user()))
        .arg(format!("--working-directory={}", provider.home()))
        .arg(format!("--property=StateDirectory={}", provider.user()))
        .arg("--property=StateDirectoryMode=0700")
        .arg("--property=NoNewPrivileges=yes")
        .arg("--property=CapabilityBoundingSet=")
        .arg("--property=RestrictSUIDSGID=yes")
        .arg("--property=PrivateTmp=yes")
        .arg("--property=ProtectHome=yes")
        .arg("--property=ProtectSystem=strict")
        .arg("--property=ProtectControlGroups=yes")
        .arg("--property=ProtectKernelModules=yes")
        .arg("--property=ProtectKernelTunables=yes")
        .arg("--property=ProtectProc=invisible")
        .arg("--property=InaccessiblePaths=/etc/jarvis /var/lib/jarvis")
        .arg("--property=RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6")
        .arg(format!("--property=RuntimeMaxSec={runtime_limit}"))
        .arg("--property=TasksMax=64")
        .arg("--property=MemoryMax=1G")
        .arg("--property=UnsetEnvironment=ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN OPENAI_API_KEY CODEX_ACCESS_TOKEN CLAUDE_CODE_OAUTH_TOKEN CLAUDE_CODE_OAUTH_REFRESH_TOKEN HF_TOKEN SSH_AUTH_SOCK");
    // A system manager may inject DefaultEnvironment even when this caller's
    // environment is empty. The provider process itself must start from -i.
    command
        .args(["--", ENV, "-i"])
        .args(provider_process_environment(provider));
    command.arg(provider.binary()).args(args).env_clear();
    Ok(command)
}

/// Fixed host tools every provider command runs through.
fn validate_worker_host() -> Result<()> {
    validate_system_executable(SYSTEMD_RUN)?;
    validate_system_executable(ENV)
}

fn provider_process_environment(provider: AccountProvider) -> Vec<String> {
    let mut environment = vec![
        format!("HOME={}", provider.home()),
        format!("USER={}", provider.user()),
        format!("LOGNAME={}", provider.user()),
        "PATH=/usr/local/bin:/usr/bin:/bin".to_owned(),
        "LANG=C.UTF-8".to_owned(),
    ];
    if provider == AccountProvider::Claude {
        environment.push(format!("CLAUDE_CONFIG_DIR={}/.claude", provider.home()));
        // The root-installed runtime is the only one Jarvis runs; the CLI must
        // never self-update into the worker's writable home.
        environment.push("DISABLE_UPDATES=1".to_owned());
        environment.push("DISABLE_AUTOUPDATER=1".to_owned());
    }
    environment
}

/// The official provider runtime: a root-owned regular file, never a link.
/// The installer writes a regular file, so a link here means tampering.
fn validate_root_executable(path: &str) -> Result<()> {
    resolve_trusted_executable(Path::new("/"), Path::new(path), 0, 0)
        .map(|_| ())
        .context("official provider runtime is not a root-owned regular executable")
}

/// Fixed host tools may be distribution symlinks (Ubuntu 26.04 ships
/// `/usr/bin/env -> ../lib/cargo/bin/coreutils/env`). Every link and every
/// directory on the chain must be root-controlled, so no non-root user can
/// redirect it between this check and exec. Callers execute the original path:
/// multi-call binaries dispatch on argv[0].
fn validate_system_executable(path: &str) -> Result<()> {
    resolve_trusted_executable(Path::new("/"), Path::new(path), 0, SYSTEM_LINK_HOPS)
        .map(|_| ())
        .with_context(|| format!("system executable {path} is not root-controlled"))
}

/// Resolves absolute `path` below `root` component by component, following at
/// most `max_hops` symlinks. Every directory walked (including `root`) and the
/// final regular file must be owned by `owner` without group/other write, the
/// file must be executable, and every symlink must be owned by `owner`. Only
/// tests pass a `root` other than `/` or an `owner` other than root.
fn resolve_trusted_executable(
    root: &Path,
    path: &Path,
    owner: u32,
    max_hops: usize,
) -> Result<PathBuf> {
    fn push_components(pending: &mut Vec<OsString>, path: &Path) {
        // Reversed: the stack pops the next component first.
        pending.extend(path.components().rev().filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some("..".into()),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        }));
    }
    let trusted = |metadata: &fs::Metadata| metadata.uid() == owner && metadata.mode() & 0o022 == 0;
    if !path.is_absolute() {
        bail!("path is not absolute");
    }
    let metadata = fs::symlink_metadata(root).context("inspect root directory")?;
    if !metadata.is_dir() || !trusted(&metadata) {
        bail!("unsafe directory {}", root.display());
    }
    let mut pending = Vec::new();
    push_components(&mut pending, path);
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(name) = pending.pop() {
        if name == ".." {
            // Every ancestor of `resolved` was already checked.
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&name);
        let full = root.join(&candidate);
        let metadata =
            fs::symlink_metadata(&full).with_context(|| format!("inspect {}", full.display()))?;
        if metadata.file_type().is_symlink() {
            hops += 1;
            if hops > max_hops {
                bail!("{} is a symlink beyond the allowed chain", full.display());
            }
            if metadata.uid() != owner {
                bail!("symlink {} has an unsafe owner", full.display());
            }
            let target =
                fs::read_link(&full).with_context(|| format!("read {}", full.display()))?;
            if target.is_absolute() {
                resolved.clear();
            }
            push_components(&mut pending, &target);
        } else if pending.is_empty() {
            if !metadata.is_file() || !trusted(&metadata) || metadata.mode() & 0o111 == 0 {
                bail!(
                    "{} has unsafe type, ownership or permissions",
                    full.display()
                );
            }
            return Ok(full);
        } else {
            if !metadata.is_dir() || !trusted(&metadata) {
                bail!("unsafe directory {}", full.display());
            }
            resolved = candidate;
        }
    }
    bail!("path does not name a file")
}

fn service_identity(provider: AccountProvider) -> Result<(u32, u32)> {
    let name = CString::new(provider.user())?;
    // Fixed compile-time names; NSS supplies identities but no caller controls them.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        bail!("dedicated subscription identity is missing");
    }
    let entry = unsafe { &*entry };
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) }.to_str()?;
    let home = unsafe { CStr::from_ptr(entry.pw_dir) }.to_str()?;
    if shell != "/usr/sbin/nologin" || home != provider.home() {
        bail!("dedicated subscription identity has unsafe account settings");
    }
    let group = unsafe { libc::getgrgid(entry.pw_gid) };
    if group.is_null() || unsafe { CStr::from_ptr((*group).gr_name) }.to_str()? != provider.user() {
        bail!("dedicated subscription identity has an unsafe primary group");
    }
    let mut groups = [0 as libc::gid_t; 16];
    let mut count = groups.len() as libc::c_int;
    let found =
        unsafe { libc::getgrouplist(name.as_ptr(), entry.pw_gid, groups.as_mut_ptr(), &mut count) };
    if found != 1 || count != 1 || groups[0] != entry.pw_gid {
        bail!("dedicated subscription identity has unexpected group access");
    }
    Ok((entry.pw_uid, entry.pw_gid))
}

fn validate_state_directory(path: &str, uid: u32, gid: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(Path::new(path)).context("subscription state is missing")?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        bail!("subscription state has unsafe ownership or permissions");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_fails_closed_on_api_billing_and_ambiguous_metadata() {
        assert_eq!(
            parse_claude_status(
                r#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}"#
            ),
            "wrong_auth_mode"
        );
        assert_eq!(
            parse_claude_status(
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
            ),
            "wrong_auth_mode"
        );
        assert_eq!(
            parse_claude_status(
                r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}"#
            ),
            "connected"
        );
        assert_eq!(
            parse_codex_status("Logged in using an API key"),
            "wrong_auth_mode"
        );
        assert_eq!(parse_codex_status("Logged in using ChatGPT"), "connected");
    }

    #[test]
    fn provider_commands_have_no_token_argument() {
        for provider in AccountProvider::all() {
            for args in [
                provider.status_args(),
                provider.connect_args(),
                provider.disconnect_args(),
            ] {
                assert!(!args
                    .iter()
                    .any(|arg| arg.contains("token") || arg.contains("key")));
            }
        }
    }

    #[test]
    fn official_client_environment_is_an_explicit_allowlist() {
        for provider in AccountProvider::all() {
            let environment = provider_process_environment(provider);
            assert!(environment.iter().all(|entry| {
                [
                    "HOME=",
                    "USER=",
                    "LOGNAME=",
                    "PATH=",
                    "LANG=",
                    "CLAUDE_CONFIG_DIR=",
                    "DISABLE_UPDATES=1",
                    "DISABLE_AUTOUPDATER=1",
                ]
                .iter()
                .any(|prefix| entry.starts_with(prefix))
            }));
            assert!(environment.iter().all(|entry| {
                !entry.contains("API_KEY") && !entry.contains("TOKEN") && !entry.contains("PROXY")
            }));
        }
        let claude = provider_process_environment(AccountProvider::Claude);
        assert!(claude.contains(&"DISABLE_UPDATES=1".to_owned()));
        assert!(claude.contains(&"DISABLE_AUTOUPDATER=1".to_owned()));
    }

    #[test]
    fn disconnect_blocks_new_runs_before_logging_out() {
        assert_eq!(
            runtime_stop_commands(AccountProvider::Claude)[0],
            ["disable", "--now", "jarvis-claude.socket"]
        );
        assert_eq!(
            runtime_stop_commands(AccountProvider::Codex)[0],
            ["disable", "--now", "jarvis-codex-broker.service"]
        );
        assert_eq!(
            runtime_stop_commands(AccountProvider::CodexChat)[0],
            ["disable", "--now", "jarvis-codex-chat.socket"]
        );
        // Each login stops only the runtime that uses it.
        assert!(!runtime_stop_commands(AccountProvider::Codex)
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("chat"))));
        assert!(!runtime_stop_commands(AccountProvider::CodexChat)
            .iter()
            .any(|args| args.iter().any(|arg| arg.contains("broker"))));
    }

    #[test]
    fn codex_chat_has_its_own_identity_and_login_home() {
        let chat = AccountProvider::from_str("codex-chat", false).unwrap();
        assert_eq!(chat, AccountProvider::CodexChat);
        assert_eq!(chat.user(), "jarvis-codex-chat");
        assert_eq!(chat.home(), "/var/lib/jarvis-codex-chat");
        assert_eq!(chat.connect_args(), ["login", "--device-auth"]);
        assert_ne!(chat.user(), AccountProvider::Codex.user());
        assert_ne!(chat.home(), AccountProvider::Codex.home());
        assert_eq!(
            AccountStatus::new(chat, "connected").billing,
            "subscription"
        );
    }

    #[test]
    fn status_output_never_contains_unrecognized_provider_fields() {
        let result = AccountStatus::new(AccountProvider::Claude, "connected");
        assert_eq!(result.billing, "overage_unverified");
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains("canary-secret"));
        assert!(!encoded.contains("email"));
        let incompatible = AccountStatus::new(AccountProvider::Claude, "incompatible_runtime");
        assert_eq!(incompatible.billing, "unverified");
    }

    /// A `/`-like tree owned by the test user: `usr/bin/env` is a relative
    /// link to the multi-call `usr/lib/coreutils/env`, as on Ubuntu 26.04.
    fn host_tree() -> (tempfile::TempDir, u32) {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        // The tree root stands in for `/`; it must not inherit a lax umask.
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        for dir in ["usr", "usr/bin", "usr/lib", "usr/lib/coreutils"] {
            fs::create_dir(root.path().join(dir)).unwrap();
            fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let binary = root.path().join("usr/lib/coreutils/env");
        fs::write(&binary, b"multi-call").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("../lib/coreutils/env", root.path().join("usr/bin/env")).unwrap();
        (root, unsafe { libc::geteuid() })
    }

    fn resolve(root: &Path, path: &str, owner: u32) -> Result<PathBuf> {
        resolve_trusted_executable(root, Path::new(path), owner, SYSTEM_LINK_HOPS)
    }

    #[test]
    fn system_executable_accepts_an_owned_symlink_chain() {
        let (root, uid) = host_tree();
        let binary = root.path().join("usr/lib/coreutils/env");
        assert_eq!(resolve(root.path(), "/usr/bin/env", uid).unwrap(), binary);
        // Absolute targets resolve from the same root; chains are followed.
        std::os::unix::fs::symlink("/usr/bin/env", root.path().join("usr/bin/printenv")).unwrap();
        assert_eq!(
            resolve(root.path(), "/usr/bin/printenv", uid).unwrap(),
            binary
        );
        assert_eq!(
            resolve(root.path(), "/usr/lib/coreutils/env", uid).unwrap(),
            binary
        );
        // Another owner is not trusted.
        assert!(resolve(root.path(), "/usr/bin/env", uid.wrapping_add(1)).is_err());
    }

    #[test]
    fn system_executable_rejects_writable_directories_and_targets() {
        let mode = |path: &str, mode| {
            fs::set_permissions(Path::new(path), fs::Permissions::from_mode(mode)).unwrap()
        };
        let (root, uid) = host_tree();
        let at = |rel: &str| root.path().join(rel).to_str().unwrap().to_owned();
        // The link's own directory.
        mode(&at("usr/bin"), 0o775);
        assert!(resolve(root.path(), "/usr/bin/env", uid).is_err());
        mode(&at("usr/bin"), 0o755);
        // A directory on the resolved target path.
        mode(&at("usr/lib"), 0o757);
        assert!(resolve(root.path(), "/usr/bin/env", uid).is_err());
        mode(&at("usr/lib"), 0o755);
        // The final target: writable, then not executable.
        mode(&at("usr/lib/coreutils/env"), 0o775);
        assert!(resolve(root.path(), "/usr/bin/env", uid).is_err());
        mode(&at("usr/lib/coreutils/env"), 0o644);
        assert!(resolve(root.path(), "/usr/bin/env", uid).is_err());
        mode(&at("usr/lib/coreutils/env"), 0o755);
        assert!(resolve(root.path(), "/usr/bin/env", uid).is_ok());
    }

    #[test]
    fn system_executable_rejects_loops_long_chains_and_dangling_links() {
        use std::os::unix::fs::symlink;
        let (root, uid) = host_tree();
        let bin = root.path().join("usr/bin");
        symlink("b", bin.join("a")).unwrap();
        symlink("a", bin.join("b")).unwrap();
        assert!(resolve(root.path(), "/usr/bin/a", uid).is_err());
        symlink("missing", bin.join("dangling")).unwrap();
        assert!(resolve(root.path(), "/usr/bin/dangling", uid).is_err());
        // SYSTEM_LINK_HOPS links are fine; one more is not.
        symlink("env", bin.join("link0")).unwrap();
        for hop in 1..=SYSTEM_LINK_HOPS {
            symlink(format!("link{}", hop - 1), bin.join(format!("link{hop}"))).unwrap();
        }
        assert!(resolve(
            root.path(),
            &format!("/usr/bin/link{}", SYSTEM_LINK_HOPS - 2),
            uid
        )
        .is_ok());
        assert!(resolve(
            root.path(),
            &format!("/usr/bin/link{}", SYSTEM_LINK_HOPS - 1),
            uid
        )
        .is_err());
        // A directory or relative path never qualifies.
        assert!(resolve(root.path(), "/usr/bin", uid).is_err());
        assert!(resolve(root.path(), "/usr/bin/..", uid).is_err());
        assert!(resolve(root.path(), "usr/bin/env", uid).is_err());
    }

    #[test]
    fn provider_runtime_still_rejects_any_symlink() {
        let (root, uid) = host_tree();
        let strict = |path: &str| resolve_trusted_executable(root.path(), Path::new(path), uid, 0);
        assert!(strict("/usr/lib/coreutils/env").is_ok());
        assert!(strict("/usr/bin/env").is_err());
    }
}
