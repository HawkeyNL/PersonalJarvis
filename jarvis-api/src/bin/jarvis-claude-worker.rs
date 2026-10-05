//! Finite subscription-only Claude brain worker. The official CLI runs as
//! jarvis-claude, never inside Core and never with Core's provider API keys.

mod subscription_worker;

use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use jarvis_llm::claude_worker_protocol::{
    claude_subscription_status, reviewed_claude_version, ClaudeWorkerReply, ClaudeWorkerRequest,
    ClaudeWorkerState, MAX_REPLY_BYTES,
};
use serde::Deserialize;
use subscription_worker::{
    authorized_peer, inherited_listener, named_uid, read_request, reply_deadline, research_slot,
    run_timeout, send, validate_private_dir, validate_root_binary, MAX_PARALLEL_RESEARCH_RUNS,
    MAX_PARALLEL_RUNS, REQUEST_READ_TIMEOUT,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

const CLAUDE: &str = "/usr/local/bin/claude";
const HOME: &str = "/var/lib/jarvis-claude";
const RUNTIME: &str = "/run/jarvis-claude";
/// Search, read results, answer: enough turns for a few searches, bounded.
const RESEARCH_MAX_TURNS: &str = "8";

#[derive(Deserialize)]
struct CliUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

#[derive(Deserialize)]
struct CliResult {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    subtype: String,
    #[serde(default)]
    result: String,
    usage: Option<CliUsage>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let worker_uid = named_uid("jarvis-claude")?;
    let core_uid = named_uid("jarvis")?;
    if unsafe { libc::geteuid() } != worker_uid {
        bail!("Claude worker must run as its dedicated identity");
    }
    validate_root_binary(CLAUDE)?;
    validate_private_dir(HOME, worker_uid)?;
    validate_private_dir(RUNTIME, worker_uid)?;
    let listener = inherited_listener()?;
    let permits = std::sync::Arc::new(Semaphore::new(MAX_PARALLEL_RUNS));
    let research_permits = std::sync::Arc::new(Semaphore::new(MAX_PARALLEL_RESEARCH_RUNS));
    loop {
        let (mut stream, _) = listener.accept().await?;
        if !authorized_peer(&stream, core_uid) {
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            let _ = send(
                &mut stream,
                ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
            )
            .await;
            continue;
        };
        let research_permits = research_permits.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let failure = || ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure);
            let reply =
                match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_request(&mut stream)).await {
                    Ok(Ok(request)) => match research_slot(request.research, &research_permits) {
                        Some(_research_permit) => {
                            let deadline = reply_deadline(request.research);
                            match tokio::time::timeout(deadline, handle(request)).await {
                                Ok(Ok(reply)) => reply,
                                _ => failure(),
                            }
                        }
                        // The one research slot is busy: refuse, never queue.
                        None => failure(),
                    },
                    _ => failure(),
                };
            let _ = send(&mut stream, reply).await;
        });
    }
}

async fn handle(request: ClaudeWorkerRequest) -> Result<ClaudeWorkerReply> {
    if !supported_runtime().await {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::IncompatibleRuntime,
        ));
    }
    if !subscription_auth().await {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::SubscriptionUnavailable,
        ));
    }
    run_official_client(request).await
}

/// The reviewed non-interactive invocation requires `--restricted`, introduced
/// in Claude Code 2.1.248. Do not probe by launching a paid prompt; `--help`
/// is also incomplete according to the official CLI reference. An unreviewed
/// minor/major line must be explicitly checked before this worker uses it.
async fn supported_runtime() -> bool {
    let mut command = clean_command();
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let Some(stdout) = child.stdout.take() else {
        return false;
    };
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        stdout.take(1025).read_to_end(&mut bytes).await?;
        child.wait().await
    })
    .await;
    matches!(result, Ok(Ok(status)) if status.success())
        && bytes.len() <= 1024
        && reviewed_claude_version(&bytes)
}

async fn subscription_auth() -> bool {
    let mut command = clean_command();
    command
        .args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let Some(stdout) = child.stdout.take() else {
        return false;
    };
    let mut bytes = Vec::new();
    let Ok(result) = tokio::time::timeout(Duration::from_secs(5), async {
        stdout.take(16 * 1024 + 1).read_to_end(&mut bytes).await?;
        child.wait().await
    })
    .await
    else {
        return false;
    };
    let Ok(status) = result else {
        return false;
    };
    if !status.success() || bytes.len() > 16 * 1024 {
        return false;
    }
    claude_subscription_status(&bytes) == "connected"
}

async fn run_official_client(request: ClaudeWorkerRequest) -> Result<ClaudeWorkerReply> {
    let mut system_file = None;
    if let Some(system) = request
        .system
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let mut file = tempfile::NamedTempFile::new_in(RUNTIME)?;
        use std::io::Write;
        file.write_all(system.as_bytes())?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        system_file = Some(file);
    }
    let mut command = clean_command();
    command.args(cli_args(
        &request.model,
        request.research,
        system_file.as_ref().map(|file| file.path()),
    ));
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().context("Claude stdin unavailable")?;
    stdin.write_all(request.prompt.as_bytes()).await?;
    drop(stdin);
    let stdout = child.stdout.take().context("Claude stdout unavailable")?;
    let mut bytes = Vec::new();
    let output = tokio::time::timeout(run_timeout(request.research), async {
        stdout
            .take((MAX_REPLY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        child.wait().await
    })
    .await??;
    if bytes.len() > MAX_REPLY_BYTES || !output.success() {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::RuntimeFailure,
        ));
    }
    Ok(parse_cli_reply(&bytes))
}

/// The reviewed `claude -p` invocation: restricted, bare, no session, no MCP
/// and no built-in tool. Only an owner-enabled research run gets exactly one
/// tool: `WebSearch`, which runs on Anthropic's side and returns titles and
/// URLs. `WebFetch` (a host-side fetch) stays unavailable: `--restricted`
/// removes it unless `--tools` names it, and `--tools` names only WebSearch.
fn cli_args(model: &str, research: bool, system_prompt: Option<&Path>) -> Vec<OsString> {
    let tools = if research { "WebSearch" } else { "" };
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "json",
        "--model",
        model,
        "--restricted",
        "--bare",
        "--no-session-persistence",
        "--tools",
        tools,
        "--disallowedTools",
        "mcp__*",
    ]
    .map(OsString::from)
    .to_vec();
    if research {
        // `-p` cannot ask for permission; allow exactly the one tool.
        args.extend(
            [
                "--allowedTools",
                "WebSearch",
                "--max-turns",
                RESEARCH_MAX_TURNS,
            ]
            .map(OsString::from),
        );
    }
    if let Some(path) = system_prompt {
        args.extend([OsString::from("--system-prompt-file"), path.into()]);
    }
    args
}

fn parse_cli_reply(bytes: &[u8]) -> ClaudeWorkerReply {
    let Ok(parsed) = serde_json::from_slice::<CliResult>(bytes) else {
        return ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure);
    };
    if parsed.is_error {
        let category = format!("{} {}", parsed.subtype, parsed.result).to_ascii_lowercase();
        return ClaudeWorkerReply::failure(
            if category.contains("limit") || category.contains("rate") {
                ClaudeWorkerState::PlanLimit
            } else {
                ClaudeWorkerState::RuntimeFailure
            },
        );
    }
    if parsed.result.trim().is_empty() {
        return ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure);
    }
    ClaudeWorkerReply {
        protocol: 1,
        state: ClaudeWorkerState::Completed,
        text: Some(parsed.result),
        input_tokens: parsed.usage.as_ref().map(|u| u.input_tokens),
        output_tokens: parsed.usage.as_ref().map(|u| u.output_tokens),
        cache_read_tokens: parsed.usage.as_ref().map(|u| u.cache_read_input_tokens),
        cache_write_tokens: parsed.usage.as_ref().map(|u| u.cache_creation_input_tokens),
    }
}

fn clean_command() -> Command {
    let mut command = Command::new(CLAUDE);
    command
        .env_clear()
        .env("HOME", HOME)
        .env("USER", "jarvis-claude")
        .env("LOGNAME", "jarvis-claude")
        .env("CLAUDE_CONFIG_DIR", format!("{HOME}/.claude"))
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        // Only the root-installed runtime may run; never self-update into HOME.
        .env("DISABLE_UPDATES", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .current_dir(HOME);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_auth_and_unknown_status_never_count_as_subscription() {
        assert_ne!(claude_subscription_status(br#"{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty","subscriptionType":"max"}"#), "connected");
        assert_ne!(
            claude_subscription_status(
                br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty"}"#
            ),
            "connected"
        );
        assert_eq!(claude_subscription_status(br#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}"#), "connected");
    }

    #[test]
    fn cli_environment_disables_self_updates() {
        let command = clean_command();
        let environment = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_str().unwrap(),
                    value.and_then(|value| value.to_str()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(environment.get("DISABLE_UPDATES"), Some(&Some("1")));
        assert_eq!(environment.get("DISABLE_AUTOUPDATER"), Some(&Some("1")));
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
    }

    fn argv(research: bool, system_prompt: Option<&Path>) -> String {
        cli_args("claude-opus-5", research, system_prompt)
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn argv_is_exactly_the_reviewed_invocation() {
        let prompt = Path::new("/run/jarvis-claude/.tmpX");
        assert_eq!(
            argv(false, Some(prompt)),
            "-p --output-format json --model claude-opus-5 --restricted --bare \
             --no-session-persistence --tools  --disallowedTools mcp__* \
             --system-prompt-file /run/jarvis-claude/.tmpX"
        );
        assert_eq!(cli_args("m", false, None)[9], OsString::from(""));
        assert_eq!(
            argv(true, Some(prompt)),
            "-p --output-format json --model claude-opus-5 --restricted --bare \
             --no-session-persistence --tools WebSearch --disallowedTools mcp__* \
             --allowedTools WebSearch --max-turns 8 \
             --system-prompt-file /run/jarvis-claude/.tmpX"
        );
        // Never the host-side fetch, never an MCP or command tool.
        for research in [false, true] {
            let args = argv(research, None);
            for forbidden in [
                "WebFetch",
                "Bash",
                "default",
                "--dangerously",
                "--mcp-config",
            ] {
                assert!(!args.contains(forbidden), "{forbidden}");
            }
        }
    }

    #[test]
    fn plan_limit_is_non_secret_status() {
        let reply = parse_cli_reply(br#"{"is_error":true,"subtype":"error_usage_limit","result":"private provider message"}"#);
        assert!(matches!(reply.state, ClaudeWorkerState::PlanLimit));
        assert!(reply.text.is_none());
    }
}
