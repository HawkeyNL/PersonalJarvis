//! Finite subscription-only Claude brain worker. The official CLI runs as
//! jarvis-claude, never inside Core and never with Core's provider API keys.

mod subscription_worker;

use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use jarvis_llm::{
    claude_worker_protocol::{
        claude_subscription_status, reviewed_claude_version, ClaudeWorkerReply,
        ClaudeWorkerRequest, ClaudeWorkerState, MAX_REPLY_BYTES,
    },
    privacy::leaks_identifier,
};
use serde::Deserialize;
use subscription_worker::{
    authorized_peer, background_slot, inherited_listener, named_uid, read_request, reply_deadline,
    run_timeout, send, stop_group, validate_private_dir, validate_root_binary,
    MAX_PARALLEL_BACKGROUND_RUNS, MAX_PARALLEL_RUNS, REQUEST_READ_TIMEOUT,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

const CLAUDE: &str = "/usr/local/bin/claude";
const HOME: &str = "/var/lib/jarvis-claude";
const RUNTIME: &str = "/run/jarvis-claude";
/// Search, read results, answer: enough turns for a few searches, bounded.
const RESEARCH_MAX_TURNS: &str = "8";
/// A research run's whole event stream. Each line, the final result among
/// them, stays within [`MAX_REPLY_BYTES`] like the single JSON reply.
const MAX_RESEARCH_STREAM_BYTES: usize = 4 * 1024 * 1024;

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
    let background_permits = std::sync::Arc::new(Semaphore::new(MAX_PARALLEL_BACKGROUND_RUNS));
    loop {
        let (mut stream, _) = listener.accept().await?;
        if !authorized_peer(&stream, core_uid) {
            continue;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            let _ = send(
                &mut stream,
                failed(ClaudeWorkerState::RuntimeFailure, "all run slots busy"),
            )
            .await;
            continue;
        };
        let background_permits = background_permits.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let failure = |stage| failed(ClaudeWorkerState::RuntimeFailure, stage);
            let reply =
                match tokio::time::timeout(REQUEST_READ_TIMEOUT, read_request(&mut stream)).await {
                    Ok(Ok(request)) => match background_slot(&request, &background_permits) {
                        Some(_background_permit) => {
                            let deadline = reply_deadline(&request);
                            match tokio::time::timeout(deadline, handle(request)).await {
                                Ok(Ok(reply)) => reply,
                                Ok(Err(_)) => failure("run I/O error"),
                                Err(_) => failure("reply deadline"),
                            }
                        }
                        // The one background slot is busy: refuse, never queue.
                        None => failure("background slot busy"),
                    },
                    _ => failure("invalid request"),
                };
            let _ = send(&mut stream, reply).await;
        });
    }
}

async fn handle(request: ClaudeWorkerRequest) -> Result<ClaudeWorkerReply> {
    if !supported_runtime().await {
        return Ok(failed(
            ClaudeWorkerState::IncompatibleRuntime,
            "version gate",
        ));
    }
    if !subscription_auth().await {
        return Ok(failed(
            ClaudeWorkerState::SubscriptionUnavailable,
            "subscription auth check",
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
    let mut command = official_command(&request, system_file.as_ref().map(|file| file.path()));
    if request.research {
        return Ok(run_research(command, &request.prompt, run_timeout(&request)).await);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return Ok(failed(ClaudeWorkerState::RuntimeFailure, "spawn"));
    };
    let mut stdin = child.stdin.take().context("Claude stdin unavailable")?;
    if stdin.write_all(request.prompt.as_bytes()).await.is_err() {
        return Ok(failed(ClaudeWorkerState::RuntimeFailure, "stdin write"));
    }
    drop(stdin);
    let stdout = child.stdout.take().context("Claude stdout unavailable")?;
    let mut bytes = Vec::new();
    let Ok(output) = tokio::time::timeout(run_timeout(&request), async {
        stdout
            .take((MAX_REPLY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await?;
        child.wait().await
    })
    .await
    else {
        return Ok(failed(ClaudeWorkerState::RuntimeFailure, "run timeout"));
    };
    if bytes.len() > MAX_REPLY_BYTES {
        return Ok(failed(
            ClaudeWorkerState::RuntimeFailure,
            "oversized output",
        ));
    }
    if !output?.success() {
        return Ok(failed(ClaudeWorkerState::RuntimeFailure, "non-zero exit"));
    }
    Ok(parse_cli_reply(&bytes))
}

/// The reviewed `claude -p` invocation: restricted, no session, no MCP and no
/// built-in tool. Never `--bare`: it reads only `ANTHROPIC_API_KEY` or an
/// `apiKeyHelper`, never the subscription OAuth login this worker exists for.
/// What `--bare` skipped is covered by `--restricted` (user, project and local
/// settings files, so their hooks and plugins), `--strict-mcp-config`, the
/// environment of `clean_command` and the private working directory.
///
/// Only an owner-enabled research run gets exactly one tool: `WebSearch`,
/// which runs on Anthropic's side and returns titles and URLs. `WebFetch` (a
/// host-side fetch) stays unavailable: `--restricted` removes it unless
/// `--tools` names it, and `--tools` names only WebSearch. A typed task never
/// searches (its request cannot be a research run), so it runs with no tool.
/// A research run streams its events (`stream-json`, which `-p` allows only
/// with `--verbose`), so the worker sees every tool call and search query.
fn cli_args(request: &ClaudeWorkerRequest, system_prompt: Option<&Path>) -> Vec<OsString> {
    let research = request.research;
    let tools = if research { "WebSearch" } else { "" };
    let format = if research { "stream-json" } else { "json" };
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        format,
        "--model",
        &request.model,
        "--restricted",
        "--strict-mcp-config",
        "--no-session-persistence",
        "--tools",
        tools,
        "--disallowedTools",
        "mcp__*",
        "--disable-slash-commands",
    ]
    .map(OsString::from)
    .to_vec();
    if research {
        // `-p` cannot ask for permission; allow exactly the one tool.
        args.extend(
            [
                "--verbose",
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

/// A research run: the reviewed invocation with its event stream inspected
/// line by line. A tool call other than `WebSearch`, a search query the
/// identifier check flags, or a line the worker cannot read stops the whole
/// process group and discards the run. The event arrives after the CLI
/// produced it, so this detects and discards; it cannot prevent.
async fn run_research(mut command: Command, prompt: &str, limit: Duration) -> ClaudeWorkerReply {
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        // Its own process group, so a refusal or timeout stops everything
        // the CLI started, not only the CLI itself.
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        return failed(ClaudeWorkerState::RuntimeFailure, "spawn");
    };
    let (Some(group), Some(mut stdin), Some(stdout)) = (
        child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()),
        child.stdin.take(),
        child.stdout.take(),
    ) else {
        return failed(ClaudeWorkerState::RuntimeFailure, "spawn");
    };
    let run = async {
        stdin.write_all(prompt.as_bytes()).await?;
        drop(stdin);
        let Some(result) = read_research_stream(stdout).await? else {
            return Ok(None);
        };
        Ok::<_, std::io::Error>(Some((child.wait().await?, result)))
    };
    let outcome = tokio::time::timeout(limit, run).await;
    let (status, result) = match outcome {
        Ok(Ok(Some(finished))) => finished,
        other => {
            stop_group(&mut child, group).await;
            return match other {
                Ok(Ok(_)) => failed(ClaudeWorkerState::ToolUseRefused, "research event refused"),
                Ok(Err(_)) => failed(ClaudeWorkerState::RuntimeFailure, "event stream"),
                Err(_) => failed(ClaudeWorkerState::RuntimeFailure, "run timeout"),
            };
        }
    };
    if !status.success() {
        return failed(ClaudeWorkerState::RuntimeFailure, "non-zero exit");
    }
    result.unwrap_or_else(|| failed(ClaudeWorkerState::RuntimeFailure, "missing result"))
}

/// Read the event stream to its end: `Ok(None)` as soon as one line is
/// refused, otherwise the reply from the last `result` event, if any.
async fn read_research_stream(
    stdout: impl AsyncRead + Unpin,
) -> std::io::Result<Option<Option<ClaudeWorkerReply>>> {
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = Vec::new();
    let mut total = 0;
    let mut result = None;
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_REPLY_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if read == 0 {
            return Ok(Some(result));
        }
        total += read;
        if read > MAX_REPLY_BYTES || total > MAX_RESEARCH_STREAM_BYTES {
            return Err(std::io::Error::other(
                "Claude event stream exceeded its bound",
            ));
        }
        match stream_event(line.strip_suffix(b"\n").unwrap_or(&line)) {
            StreamEvent::Benign => {}
            StreamEvent::Result(reply) => result = Some(reply),
            StreamEvent::Refused => return Ok(None),
        }
    }
}

#[derive(Debug)]
enum StreamEvent {
    Benign,
    Result(ClaudeWorkerReply),
    Refused,
}

/// One line of `claude -p --output-format stream-json --verbose`, checked
/// against Claude Code 2.1.311 (the installed runtime) and its documented
/// SDK message shapes: `{"type":"system",...}`, `{"type":"assistant",
/// "message":{"content":[...]}}` with `text`, `thinking` and `tool_use`
/// blocks (`{"type":"tool_use","name":"WebSearch","input":{"query":...}}`),
/// `{"type":"user",...}` carrying tool results, and a final
/// `{"type":"result",...}` with the same fields as `--output-format json`.
/// Any tool-use block (`tool_use`, `server_tool_use`, `mcp_tool_use`) other
/// than a `WebSearch` whose query passes the identifier check is refused, as
/// is a line that is not a JSON object with a type or an assistant message
/// without a content list. Other event types carry no tool call and pass.
fn stream_event(line: &[u8]) -> StreamEvent {
    use serde_json::Value;
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return if line.trim_ascii().is_empty() {
            StreamEvent::Benign
        } else {
            StreamEvent::Refused
        };
    };
    match event.get("type").and_then(Value::as_str) {
        Some("result") => StreamEvent::Result(parse_cli_reply(line)),
        Some("assistant") => match event["message"]["content"].as_array() {
            Some(blocks) if blocks.iter().all(allowed_block) => StreamEvent::Benign,
            _ => StreamEvent::Refused,
        },
        Some(_) => StreamEvent::Benign,
        None => StreamEvent::Refused,
    }
}

/// A content block without tool use, or the one allowed search: `WebSearch`
/// with a string query, where no string anywhere in its input (the query,
/// and domain filters such as `allowed_domains`) is flagged by
/// [`leaks_identifier`]: no email address, key, possessive account word or
/// long number (a plain date is fine). The whole input reaches the inner
/// search call.
fn allowed_block(block: &serde_json::Value) -> bool {
    let Some(kind) = block.get("type").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !kind.ends_with("tool_use") {
        return true;
    }
    kind == "tool_use"
        && block["name"] == "WebSearch"
        && block["input"]["query"].is_string()
        && strings_pass(&block["input"])
}

/// Every string and number in `value`, at any depth, passes
/// [`leaks_identifier`].
fn strings_pass(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::String(text) => !leaks_identifier(text),
        Value::Number(number) => !leaks_identifier(&number.to_string()),
        Value::Array(items) => items.iter().all(strings_pass),
        Value::Object(fields) => fields
            .iter()
            .all(|(key, field)| !leaks_identifier(key) && strings_pass(field)),
        _ => true,
    }
}

fn parse_cli_reply(bytes: &[u8]) -> ClaudeWorkerReply {
    let Ok(parsed) = serde_json::from_slice::<CliResult>(bytes) else {
        return failed(ClaudeWorkerState::RuntimeFailure, "unparseable output");
    };
    if parsed.is_error {
        let category = format!("{} {}", parsed.subtype, parsed.result).to_ascii_lowercase();
        let state = if category.contains("limit") || category.contains("rate") {
            ClaudeWorkerState::PlanLimit
        } else {
            ClaudeWorkerState::RuntimeFailure
        };
        // Only a short machine subtype; never the result text.
        let subtype = Some(parsed.subtype.as_str())
            .filter(|s| s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
            .unwrap_or("other");
        return failed(state, &format!("is_error subtype {subtype}"));
    }
    if parsed.result.trim().is_empty() {
        return failed(ClaudeWorkerState::RuntimeFailure, "empty result");
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

/// A failure reply, logged to the journal by state and fixed stage only:
/// never prompt, system prompt, reply text, account data or CLI stderr.
fn failed(state: ClaudeWorkerState, stage: &str) -> ClaudeWorkerReply {
    eprintln!("Claude run failed: {state:?} at {stage}");
    ClaudeWorkerReply::failure(state)
}

/// The reviewed invocation in the clean environment. Only a research run
/// adds `TZ=UTC`: the CLI may tell the model the local time zone, and that
/// could reach a search query. Chat and tasks keep their environment.
fn official_command(request: &ClaudeWorkerRequest, system_prompt: Option<&Path>) -> Command {
    let mut command = clean_command();
    command.args(cli_args(request, system_prompt));
    if request.research {
        command.env("TZ", "UTC");
    }
    command
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
        // What `--bare` used to skip: no CLAUDE.md (user or project) and no
        // auto-memory. Not CLAUDE_CODE_SIMPLE: that is `--bare` itself.
        .env("CLAUDE_CODE_DISABLE_CLAUDE_MDS", "1")
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        // Belt and braces: WebFetch is never in `--tools` either. A research
        // run searches; it never fetches a page a result points at.
        .env("CLAUDE_CODE_DISABLE_WEB_FETCH", "1")
        // The private runtime directory holds no CLAUDE.md or `.claude/`,
        // nor do its root-owned parents; HOME is the CLI's own config home.
        .current_dir(RUNTIME);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_llm::claude_worker_protocol::WorkerTask;

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

    fn environment(command: &Command) -> std::collections::BTreeMap<&str, Option<&str>> {
        command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_str().unwrap(),
                    value.and_then(|value| value.to_str()),
                )
            })
            .collect()
    }

    #[test]
    fn only_research_runs_get_a_utc_time_zone() {
        let clean = clean_command();
        let clean = environment(&clean);
        assert!(!clean.contains_key("TZ"));
        for (research, task) in [
            (false, None),
            (false, Some(WorkerTask::MemeDecision)),
            (false, Some(WorkerTask::MemeStrategyReview)),
        ] {
            let command = official_command(&request(research, task), None);
            assert_eq!(environment(&command), clean);
        }
        let command = official_command(&request(true, None), None);
        let mut research = environment(&command);
        assert_eq!(research.remove("TZ"), Some(Some("UTC")));
        assert_eq!(research, clean);
    }

    #[test]
    fn cli_environment_disables_self_updates() {
        let command = clean_command();
        let environment = environment(&command);
        assert_eq!(environment.get("DISABLE_UPDATES"), Some(&Some("1")));
        assert_eq!(environment.get("DISABLE_AUTOUPDATER"), Some(&Some("1")));
        assert_eq!(
            environment.get("CLAUDE_CODE_DISABLE_CLAUDE_MDS"),
            Some(&Some("1"))
        );
        assert_eq!(
            environment.get("CLAUDE_CODE_DISABLE_AUTO_MEMORY"),
            Some(&Some("1"))
        );
        assert_eq!(
            environment.get("CLAUDE_CODE_DISABLE_WEB_FETCH"),
            Some(&Some("1"))
        );
        assert!(!environment.contains_key("ANTHROPIC_API_KEY"));
        // CLAUDE_CODE_SIMPLE is `--bare`, which never reads the OAuth login.
        assert!(!environment.contains_key("CLAUDE_CODE_SIMPLE"));
        assert_eq!(command.as_std().get_current_dir(), Some(Path::new(RUNTIME)));
    }

    fn request(research: bool, task: Option<WorkerTask>) -> ClaudeWorkerRequest {
        ClaudeWorkerRequest {
            protocol: 1,
            model: "claude-opus-5".into(),
            system: None,
            prompt: "{}".into(),
            research,
            task,
        }
    }

    fn argv(research: bool, system_prompt: Option<&Path>) -> String {
        task_argv(&request(research, None), system_prompt)
    }

    fn task_argv(request: &ClaudeWorkerRequest, system_prompt: Option<&Path>) -> String {
        cli_args(request, system_prompt)
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Flags and tools that would widen what a run may touch, as substrings
    /// of the whole argv (`--dangerously*` included). CI guard.
    fn assert_no_widening_tokens(args: &str) {
        for forbidden in [
            "--settings",
            "--add-dir",
            "--permission-mode",
            "--dangerously",
            "--agents",
            "Agent",
            "Task",
        ] {
            assert!(!args.contains(forbidden), "{forbidden} in {args}");
        }
        // Exact tokens: `--strict-mcp-config` stays allowed.
        for forbidden in [
            "--mcp-config",
            "--plugin-dir",
            "--chrome",
            "--resume",
            "-r",
            "--continue",
            "-c",
            "--include-partial-messages",
            "--setting-sources",
            "--session-id",
            "--fork-session",
            "--debug-file",
        ] {
            assert!(
                !args
                    .split(' ')
                    .any(|arg| arg == forbidden || arg.starts_with(&format!("{forbidden}="))),
                "{forbidden} in {args}"
            );
        }
    }

    #[test]
    fn argv_is_exactly_the_reviewed_invocation() {
        let prompt = Path::new("/run/jarvis-claude/.tmpX");
        assert_eq!(
            argv(false, Some(prompt)),
            "-p --output-format json --model claude-opus-5 --restricted --strict-mcp-config \
             --no-session-persistence --tools  --disallowedTools mcp__* \
             --disable-slash-commands --system-prompt-file /run/jarvis-claude/.tmpX"
        );
        assert_eq!(cli_args(&request(false, None), None)[9], OsString::from(""));
        assert_eq!(
            argv(true, Some(prompt)),
            "-p --output-format stream-json --model claude-opus-5 --restricted \
             --strict-mcp-config --no-session-persistence --tools WebSearch \
             --disallowedTools mcp__* --disable-slash-commands --verbose \
             --allowedTools WebSearch --max-turns 8 --system-prompt-file /run/jarvis-claude/.tmpX"
        );
        // Never the host-side fetch, never an MCP or command tool.
        for research in [false, true] {
            let args = argv(research, None);
            assert_no_widening_tokens(&args);
            assert_no_widening_tokens(&argv(research, Some(prompt)));
            for forbidden in [
                "--bare",
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
    fn task_runs_use_the_reviewed_tool_free_invocation_and_the_requested_model() {
        let prompt = Path::new("/run/jarvis-claude/.tmpX");
        for task in [WorkerTask::MemeDecision, WorkerTask::MemeStrategyReview] {
            let mut request = request(false, Some(task));
            request.model = "claude-sonnet-5".into();
            let args = cli_args(&request, Some(prompt));
            assert_eq!(args[9], OsString::from(""));
            assert_eq!(
                task_argv(&request, Some(prompt)),
                "-p --output-format json --model claude-sonnet-5 --restricted \
                 --strict-mcp-config --no-session-persistence --tools  \
                 --disallowedTools mcp__* --disable-slash-commands \
                 --system-prompt-file /run/jarvis-claude/.tmpX"
            );
            let joined = task_argv(&request, None);
            assert_no_widening_tokens(&joined);
            assert_no_widening_tokens(&task_argv(&request, Some(prompt)));
            for forbidden in [
                "WebSearch",
                "WebFetch",
                "--allowedTools",
                "--max-turns",
                "stream-json",
                "--verbose",
            ] {
                assert!(!joined.contains(forbidden), "{forbidden}");
            }
        }
    }

    const SEARCH: &str = r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"WebSearch","input":{"query":"rust 1.97 release notes"}}]},"parent_tool_use_id":null,"session_id":"s"}"#;
    const RESULT: &str = r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"num_turns":2,"result":"Answer\nSources:\nhttps://example.org/a","session_id":"s","usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":5,"cache_creation_input_tokens":6},"permission_denials":[]}"#;

    #[test]
    fn stream_lines_allow_only_a_private_free_web_search() {
        for benign in [
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["WebSearch"],"mcp_servers":[]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"Searching."}]}}"#,
            SEARCH,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"Links: ..."}]}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t4","name":"WebSearch","input":{"query":"rust release","allowed_domains":["rust-lang.org","blog.rust-lang.org"],"blocked_domains":[]}}]}}"#,
            // Prices, dates and symbols are not identifiers.
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"WebSearch","input":{"query":"bitcoin price usd 2026 op 10/11/2026 ASML"}}]}}"#,
            "",
        ] {
            assert!(
                matches!(stream_event(benign.as_bytes()), StreamEvent::Benign),
                "{benign}"
            );
        }
        let tool = |name: &str, input: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"x"}},{{"type":"tool_use","id":"t2","name":"{name}","input":{input}}}]}}}}"#
            )
        };
        for refused in [
            tool("Bash", r#"{"command":"id"}"#),
            tool("WebFetch", r#"{"url":"https://example.org","prompt":"x"}"#),
            tool("Agent", r#"{"prompt":"x"}"#),
            tool("EndConversation", "{}"),
            tool("mcp__srv__post", r#"{"query":"x"}"#),
            tool("websearch", r#"{"query":"rust"}"#),
            // A search with personal details, or without a string query.
            tool("WebSearch", r#"{"query":"mijn portfolio rendement"}"#),
            tool("WebSearch", r#"{"query":"bel 0612345678"}"#),
            tool("WebSearch", r#"{"query":"jan.devries@example.nl"}"#),
            tool("WebSearch", r#"{"query":"NL91ABNA0417164300 rente"}"#),
            tool("WebSearch", r#"{"query":"2026-11-10 06 1234"}"#),
            // The domain filters reach the inner search call too.
            tool(
                "WebSearch",
                r#"{"query":"rust","allowed_domains":["jan.devries@example.nl"]}"#,
            ),
            tool(
                "WebSearch",
                r#"{"query":"rust","blocked_domains":["x.org"],"note":{"n":"0612345678"}}"#,
            ),
            tool("WebSearch", r#"{"query":"rust","n":612345678}"#),
            tool("WebSearch", r#"{"query":["rust"]}"#),
            tool("WebSearch", "{}"),
            r#"{"type":"assistant","message":{"content":[{"type":"server_tool_use","id":"s1","name":"web_search","input":{"query":"rust"}}]}}"#.into(),
            r#"{"type":"assistant","message":{"content":[{"type":"mcp_tool_use","id":"s1","name":"x","server_name":"y","input":{}}]}}"#.into(),
            r#"{"type":"assistant","message":{"content":[{"text":"no type"}]}}"#.into(),
            r#"{"type":"assistant","message":{"content":"plain"}}"#.into(),
            r#"{"type":"assistant"}"#.into(),
            r#"{"subtype":"init"}"#.into(),
            r#"["type","result"]"#.into(),
            r#"{"type":"assistant","message":{"content":[]"#.into(),
            "not json".into(),
        ] {
            assert!(
                matches!(stream_event(refused.as_bytes()), StreamEvent::Refused),
                "{refused}"
            );
        }
        let StreamEvent::Result(reply) = stream_event(RESULT.as_bytes()) else {
            panic!("the result event is the reply");
        };
        assert!(matches!(reply.state, ClaudeWorkerState::Completed));
        assert_eq!(
            reply.text.as_deref(),
            Some("Answer\nSources:\nhttps://example.org/a")
        );
        assert_eq!(reply.output_tokens, Some(4));
        assert_eq!(reply.cache_write_tokens, Some(6));
    }

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    /// A fake CLI that prints these stream lines, then runs `tail`.
    async fn fake_research(lines: &[&str], tail: &str) -> ClaudeWorkerReply {
        let echoes = lines
            .iter()
            .map(|line| format!("printf '%s\\n' '{line}'"))
            .collect::<Vec<_>>()
            .join("; ");
        run_research(
            shell(&format!("cat >/dev/null; {echoes}; {tail}")),
            "question",
            Duration::from_secs(10),
        )
        .await
    }

    #[tokio::test]
    async fn research_answer_comes_from_the_final_result_event() {
        let reply = fake_research(&[SEARCH, RESULT], "true").await;
        assert!(matches!(reply.state, ClaudeWorkerState::Completed));
        assert_eq!(
            reply.text.as_deref(),
            Some("Answer\nSources:\nhttps://example.org/a")
        );
        // No result event, or a failed exit: nothing is returned.
        let reply = fake_research(&[SEARCH], "true").await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
        let reply = fake_research(&[SEARCH, RESULT], "exit 1").await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
    }

    #[tokio::test]
    async fn refused_and_malformed_lines_discard_the_run() {
        let bash = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"id"}}]}}"#;
        let private = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"WebSearch","input":{"query":"my portfolio today"}}]}}"#;
        for line in [bash, private, "not json"] {
            for lines in [[SEARCH, line, RESULT], [SEARCH, RESULT, line]] {
                let reply = fake_research(&lines, "true").await;
                assert!(
                    matches!(reply.state, ClaudeWorkerState::ToolUseRefused),
                    "{line}"
                );
                assert!(reply.text.is_none(), "{line}");
            }
        }
    }

    #[tokio::test]
    async fn oversized_stream_lines_fail_the_run() {
        let reply = fake_research(
            &[SEARCH],
            &format!(
                "head -c {} /dev/zero | tr '\\0' x; echo; printf '%s\\n' '{RESULT}'",
                MAX_REPLY_BYTES + 1
            ),
        )
        .await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
        // Benign lines within the line bound: within the stream bound they
        // pass, beyond it the run fails.
        let system_lines = |count: usize| {
            format!(
                "for i in $(seq {count}); do printf '{{\"type\":\"system\",\"p\":\"'; \
                 head -c {} /dev/zero | tr '\\0' x; printf '\"}}\\n'; done; \
                 printf '%s\\n' '{RESULT}'",
                MAX_REPLY_BYTES / 2
            )
        };
        let reply = fake_research(&[SEARCH], &system_lines(2)).await;
        assert!(matches!(reply.state, ClaudeWorkerState::Completed));
        let over = MAX_RESEARCH_STREAM_BYTES / (MAX_REPLY_BYTES / 2) + 1;
        let reply = fake_research(&[SEARCH], &system_lines(over)).await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
    }

    #[tokio::test]
    async fn a_refused_tool_stops_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let bash = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"id"}}]}}"#;
        let started = std::time::Instant::now();
        let reply = run_research(
            shell(&format!(
                "cat >/dev/null; sleep 30 & echo $! > {}; printf '%s\\n' '{bash}' '{RESULT}'; wait",
                pid_file.display()
            )),
            "question",
            Duration::from_secs(10),
        )
        .await;
        assert!(matches!(reply.state, ClaudeWorkerState::ToolUseRefused));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z'))
        }) {
            assert!(std::time::Instant::now() < deadline, "child {pid} survived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_hanging_research_run_is_stopped_at_the_deadline() {
        let started = std::time::Instant::now();
        let reply = run_research(
            shell("cat >/dev/null; sleep 30"),
            "question",
            Duration::from_millis(200),
        )
        .await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn plan_limit_is_non_secret_status() {
        let reply = parse_cli_reply(br#"{"is_error":true,"subtype":"error_usage_limit","result":"private provider message"}"#);
        assert!(matches!(reply.state, ClaudeWorkerState::PlanLimit));
        assert!(reply.text.is_none());
    }
}
