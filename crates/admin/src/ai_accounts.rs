//! Owner-operated subscription login. This module deliberately has no Core API,
//! agent, MCP or sandbox entry point. Official clients own their auth stores.
use std::{
    ffi::{CStr, CString},
    fs,
    io::{self, IsTerminal, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;

const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum AccountProvider {
    Claude,
    Codex,
}

impl AccountProvider {
    fn all() -> [Self; 2] {
        [Self::Claude, Self::Codex]
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn user(self) -> &'static str {
        match self {
            Self::Claude => "jarvis-claude",
            Self::Codex => "jarvis-codex",
        }
    }

    fn home(self) -> &'static str {
        match self {
            Self::Claude => "/var/lib/jarvis-claude",
            Self::Codex => "/var/lib/jarvis-codex",
        }
    }

    fn binary(self) -> &'static str {
        match self {
            Self::Claude => "/usr/local/bin/claude",
            Self::Codex => "/usr/local/bin/codex",
        }
    }

    fn status_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "status"],
            Self::Codex => &["login", "status"],
        }
    }

    fn connect_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "login"],
            Self::Codex => &["login", "--device-auth"],
        }
    }

    fn disconnect_args(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["auth", "logout"],
            Self::Codex => &["logout"],
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
        AccountsCommand::List | AccountsCommand::Status { .. } => None,
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
                        println!("{:<8} {:<20} {}", item.provider, item.worker, item.state);
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
                if provider == AccountProvider::Claude && !claude_runtime_compatible() {
                    bail!("Claude runtime version is incompatible with the reviewed worker flags");
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
    validate_root_executable("/usr/bin/logger")?;
    let record = format!(
        "provider={} action={} outcome={}",
        provider.name(),
        action,
        outcome
    );
    let result = Command::new("/usr/bin/logger")
        .args([
            "--tag",
            "jarvis-accounts",
            "--priority",
            "authpriv.notice",
            "--",
            &record,
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
    validate_root_executable("/usr/bin/systemctl")?;
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
    }
}

fn ensure_service_identity(provider: AccountProvider) -> Result<()> {
    if service_identity(provider).is_err() {
        validate_root_executable("/usr/sbin/useradd")?;
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
        AccountProvider::Codex => parse_codex_status(&output),
    };
    let mut result = AccountStatus::new(provider, state);
    result.runtime = runtime_state(provider);
    result
}

fn claude_runtime_compatible() -> bool {
    worker_command(AccountProvider::Claude, &["--version"], false)
        .is_ok_and(claude_version_command_compatible)
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
    if validate_root_executable("/usr/bin/systemctl").is_err() {
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
    }
}

fn parse_claude_status(output: &str) -> &'static str {
    jarvis_llm::claude_worker_protocol::claude_subscription_status(output.as_bytes())
}

fn parse_codex_status(output: &str) -> &'static str {
    let line = output.trim().to_ascii_lowercase();
    if line == "logged in using chatgpt" {
        "connected"
    } else if line.contains("api key") || line.contains("api-key") {
        "wrong_auth_mode"
    } else if line.contains("not logged in") {
        "logged_out"
    } else {
        "unhealthy"
    }
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
    validate_root_executable(SYSTEMD_RUN)?;
    validate_root_executable("/usr/bin/env")?;
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
        .args(["--", "/usr/bin/env", "-i"])
        .args(provider_process_environment(provider));
    command.arg(provider.binary()).args(args).env_clear();
    Ok(command)
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
    }
    environment
}

fn validate_root_executable(path: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("official provider runtime path is not absolute");
    }
    for parent in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(parent).context("inspect runtime path parent")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.permissions().mode() & 0o022 != 0
        {
            bail!("official provider runtime parent is unsafe");
        }
    }
    let metadata = fs::symlink_metadata(path).context("official provider runtime is missing")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o111 == 0
    {
        bail!("official provider runtime has unsafe ownership or permissions");
    }
    Ok(())
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
                ]
                .iter()
                .any(|prefix| entry.starts_with(prefix))
            }));
            assert!(environment.iter().all(|entry| {
                !entry.contains("API_KEY") && !entry.contains("TOKEN") && !entry.contains("PROXY")
            }));
        }
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
}
