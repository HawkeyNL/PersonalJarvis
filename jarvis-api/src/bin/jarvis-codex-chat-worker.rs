//! Finite, text-only Codex chat worker for subscription models (for example
//! GPT-6 Luna through a ChatGPT plan). The official CLI runs as the jarvis-codex
//! identity with its one ChatGPT login, with every tool off, in an empty private
//! directory, never inside Core and never with an API key. The unit keeps the
//! Codex coding broker and App Server state out of its reach.

mod subscription_worker;

use std::{path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use jarvis_llm::{
    claude_worker_protocol::{
        ClaudeWorkerReply, ClaudeWorkerRequest, ClaudeWorkerState, MAX_REQUEST_BYTES,
    },
    codex_chat_protocol::{
        classify_codex_failure, codex_subscription_status, parse_codex_event,
        reviewed_codex_version, CodexEvent, REVIEWED_VERSION_ENV,
    },
};
use subscription_worker::{
    authorized_peer, inherited_listener, named_uid, send, validate_private_dir,
    validate_root_binary,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    sync::Semaphore,
};

const CODEX: &str = "/usr/local/bin/codex";
/// Login home written by `jarvis accounts connect codex`
/// (`codex login --device-auth` as jarvis-codex). The same login serves the
/// coding path, which stays off until its shared-token design is re-reviewed.
const IDENTITY: &str = "jarvis-codex";
const HOME: &str = "/var/lib/jarvis-codex";
const RUNTIME: &str = "/run/jarvis-codex-chat";
const MAX_PARALLEL_RUNS: usize = 2;
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// An owner-enabled research run searches the web first.
const RESEARCH_RUN_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_ANSWER_BYTES: usize = 128 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
/// Bound on everything one status probe prints.
const PROBE_LIMIT: usize = 16 * 1024;
/// One JSONL event (an escaped answer can be about twice its size).
const MAX_EVENT_LINE_BYTES: usize = 512 * 1024;
const MAX_EVENT_STREAM_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_EVENTS: usize = 8;

#[tokio::main]
async fn main() -> Result<()> {
    // Prompts and answers pass through this process: no core dumps and no
    // ptrace or /proc memory access by other processes of the same UID.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        bail!("Codex chat worker could not disable core dumps");
    }
    let worker_uid = named_uid(IDENTITY)?;
    let core_uid = named_uid("jarvis")?;
    if unsafe { libc::geteuid() } != worker_uid {
        bail!("Codex chat worker must run as the jarvis-codex identity");
    }
    // A paid API key must never be reachable, not even by accident.
    if std::env::vars_os().any(|(key, _)| key.to_string_lossy().ends_with("_API_KEY")) {
        bail!("Codex chat worker refuses to start with an API key in its environment");
    }
    validate_root_binary(CODEX)?;
    // Owner-reviewed CLI version from the root-owned unit configuration.
    // Unset or non-UTF-8 becomes empty, which the gate never accepts.
    let reviewed: &'static str = std::env::var(REVIEWED_VERSION_ENV)
        .unwrap_or_default()
        .leak();
    validate_private_dir(HOME, worker_uid)?;
    validate_private_dir(RUNTIME, worker_uid)?;
    let listener = inherited_listener()?;
    let permits = std::sync::Arc::new(Semaphore::new(MAX_PARALLEL_RUNS));
    // Under PrivatePIDs this process is PID 1 of its namespace, which ignores
    // SIGTERM without a handler; stop promptly when systemd asks.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let (mut stream, _) = tokio::select! {
            accepted = listener.accept() => accepted?,
            _ = terminate.recv() => return Ok(()),
        };
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
        tokio::spawn(async move {
            let _permit = permit;
            let reply = match tokio::time::timeout(
                RESEARCH_RUN_TIMEOUT + Duration::from_secs(5),
                handle(&mut stream, reviewed),
            )
            .await
            {
                Ok(Ok(reply)) => reply,
                _ => ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
            };
            let _ = send(&mut stream, reply).await;
        });
    }
}

async fn handle(stream: &mut UnixStream, reviewed: &str) -> Result<ClaudeWorkerReply> {
    // The run gets what is left of its run timeout after the request and the
    // probes, so its own deadline (which stops the whole process group)
    // always fires before the outer reply deadline.
    let started = std::time::Instant::now();
    let mut bytes = Vec::new();
    stream
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_REQUEST_BYTES || !bytes.ends_with(b"\n") {
        bail!("invalid bounded Codex chat request");
    }
    let request: ClaudeWorkerRequest = serde_json::from_slice(&bytes)?;
    if !request.valid() {
        bail!("invalid Codex chat request shape");
    }
    let version = bounded_probe(&["--version"], Duration::from_secs(3), false).await;
    if !version.is_some_and(|output| reviewed_codex_version(&output, reviewed)) {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::IncompatibleRuntime,
        ));
    }
    // `codex login status` prints its status on stderr.
    let login = bounded_probe(&["login", "status"], Duration::from_secs(5), true).await;
    if !login.is_some_and(|output| codex_subscription_status(&output) == "connected") {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::SubscriptionUnavailable,
        ));
    }
    let limit = if request.research {
        RESEARCH_RUN_TIMEOUT
    } else {
        RUN_TIMEOUT
    };
    run_official_client(request, limit.saturating_sub(started.elapsed())).await
}

/// Non-generative status probe of the official CLI.
async fn bounded_probe(args: &[&str], limit: Duration, with_stderr: bool) -> Option<Vec<u8>> {
    let mut command = clean_command(CODEX, Path::new(RUNTIME));
    command.args(args);
    bounded_output(command, limit, with_stderr).await
}

/// Output of a probe that exited successfully within one time bound and one
/// size bound over everything it printed. `with_stderr` also captures stderr,
/// after stdout. The output is for parsing only, never for logs or replies.
async fn bounded_output(
    mut command: Command,
    limit: Duration,
    with_stderr: bool,
) -> Option<Vec<u8>> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(if with_stderr {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take();
    let mut bytes = Vec::new();
    let mut errors = Vec::new();
    let status = tokio::time::timeout(limit, async {
        let read_errors = async {
            match stderr {
                Some(stderr) => stderr
                    .take(PROBE_LIMIT as u64 + 1)
                    .read_to_end(&mut errors)
                    .await
                    .map(drop),
                None => Ok(()),
            }
        };
        let mut stdout = stdout.take(PROBE_LIMIT as u64 + 1);
        let (out, err) = tokio::join!(stdout.read_to_end(&mut bytes), read_errors);
        out?;
        err?;
        child.wait().await
    })
    .await
    .ok()?
    .ok()?;
    if !errors.is_empty() {
        bytes.push(b'\n');
        bytes.extend(errors);
    }
    (status.success() && bytes.len() <= PROBE_LIMIT).then_some(bytes)
}

/// The reviewed `codex exec` invocation: ephemeral, no user config or rules,
/// ChatGPT login only, read-only sandbox and every tool, app, memory, hook,
/// history and analytics feature off. `--json` streams every event, so the
/// worker can refuse a run that uses a tool even if a `-c` key stopped
/// working. The prompt arrives on stdin (`-`), never as an argument. Only an
/// owner-enabled research run turns on the provider-hosted web search
/// (`web_search=live`); everything else stays off.
fn exec_args(model: &str, instructions: Option<&Path>, research: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "--sandbox",
        "read-only",
        "-m",
        model,
    ]
    .map(String::from)
    .to_vec();
    for setting in [
        "features.shell_tool=false",
        "features.unified_exec=false",
        if research {
            "web_search=live"
        } else {
            "web_search=disabled"
        },
        "tools.view_image=false",
        "features.apps=false",
        "features.multi_agent=false",
        "features.memories=false",
        "features.hooks=false",
        "history.persistence=none",
        "analytics.enabled=false",
        "approval_policy=never",
        "shell_environment_policy.inherit=none",
        "forced_login_method=\"chatgpt\"",
    ] {
        args.extend(["-c".into(), setting.into()]);
    }
    if let Some(path) = instructions {
        // A private 0600 file, so the system prompt is never in the argv.
        args.extend([
            "-c".into(),
            format!("model_instructions_file=\"{}\"", path.display()),
        ]);
    }
    args.push("-".into());
    args
}

async fn run_official_client(
    request: ClaudeWorkerRequest,
    limit: Duration,
) -> Result<ClaudeWorkerReply> {
    // Per-run private (0700) directory: an empty neutral workdir and the
    // optional instructions file. Removed on drop.
    let run = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(RUNTIME)?;
    let workdir = run.path().join("work");
    tokio::fs::DirBuilder::new()
        .mode(0o700)
        .create(&workdir)
        .await?;
    let mut instructions = None;
    if let Some(system) = request
        .system
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let path = run.path().join("instructions.md");
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .await?;
        file.write_all(system.as_bytes()).await?;
        file.flush().await?;
        instructions = Some(path);
    }
    let mut command = clean_command(CODEX, &workdir);
    command.args(exec_args(
        &request.model,
        instructions.as_deref(),
        request.research,
    ));
    let outcome = run_cli(command, &request.prompt, limit, request.research).await?;
    Ok(finish(outcome))
}

/// What the event stream reported. The answer comes only from a completed
/// agent message event, never from diagnostics.
#[derive(Debug, Default)]
struct Events {
    answer: Option<String>,
    completed: bool,
    errors: Vec<String>,
}

#[derive(Debug)]
enum Outcome {
    Exited {
        success: bool,
        events: Events,
        diagnostics: Vec<u8>,
    },
    /// A tool, unknown or malformed event: the run was stopped.
    Refused,
    TimedOut,
}

async fn run_cli(
    mut command: Command,
    prompt: &str,
    limit: Duration,
    research: bool,
) -> Result<Outcome> {
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        // Its own process group, so a refusal or timeout stops everything
        // the CLI started, not only the CLI itself.
        .process_group(0);
    let mut child = command.spawn()?;
    let group = child
        .id()
        .and_then(|pid| libc::pid_t::try_from(pid).ok())
        .context("Codex process id unavailable")?;
    let mut stdin = child.stdin.take().context("Codex stdin unavailable")?;
    let stdout = child.stdout.take().context("Codex events unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex diagnostics unavailable")?;
    // Feed the prompt and drain diagnostics in the background, so no pipe can
    // stall the CLI and a refusal does not wait for either. A CLI that exits
    // early decides via its status.
    let prompt = prompt.to_owned();
    tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });
    let diagnostics = tokio::spawn(read_tail(stderr, MAX_DIAGNOSTIC_BYTES));
    let run = async {
        let Some(events) = read_events(stdout, research).await? else {
            return Ok(Outcome::Refused);
        };
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>(Outcome::Exited {
            success: status.success(),
            events,
            diagnostics: diagnostics.await??,
        })
    };
    match tokio::time::timeout(limit, run).await {
        Ok(Ok(exited @ Outcome::Exited { .. })) => Ok(exited),
        other => {
            stop_group(&mut child, group).await;
            match other {
                Ok(result) => result,
                Err(_) => Ok(Outcome::TimedOut),
            }
        }
    }
}

/// Read the `--json` event stream until it ends. `Ok(None)` as soon as one
/// event is not plainly text (or, in a research run, a provider-hosted web
/// search); the caller then stops the whole run.
async fn read_events(
    stdout: impl AsyncRead + Unpin,
    research: bool,
) -> std::io::Result<Option<Events>> {
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut events = Events::default();
    let mut total = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_EVENT_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if read == 0 {
            return Ok(Some(events));
        }
        total += read;
        if read > MAX_EVENT_LINE_BYTES || total > MAX_EVENT_STREAM_BYTES {
            return Err(std::io::Error::other(
                "Codex event stream exceeded its bound",
            ));
        }
        if line.ends_with(b"\n") {
            line.pop();
        }
        match parse_codex_event(&line, research) {
            CodexEvent::Benign => {}
            CodexEvent::AgentMessage(text) => events.answer = Some(text),
            CodexEvent::TurnCompleted => events.completed = true,
            CodexEvent::Error(message) if events.errors.len() < MAX_ERROR_EVENTS => {
                events.errors.push(message)
            }
            CodexEvent::Error(_) => {}
            CodexEvent::Refused => return Ok(None),
        }
    }
}

/// Kill the run's whole process group, then reap the CLI and whatever of its
/// group was re-parented to this worker (it is PID 1 under `PrivatePIDs`).
/// Either the CLI is not reaped yet, or a group member still holds its output
/// open (the only wait after the CLI is reaped), so the group id is still in
/// use and cannot belong to another run.
async fn stop_group(child: &mut Child, group: libc::pid_t) {
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.start_kill();
    let _ = child.wait().await;
    let reap = tokio::task::spawn_blocking(move || {
        while unsafe { libc::waitpid(-group, std::ptr::null_mut(), 0) } > 0 {}
    });
    let _ = tokio::time::timeout(Duration::from_secs(5), reap).await;
}

/// Drain a stream completely, keeping only its last `cap` bytes.
async fn read_tail(mut reader: impl AsyncRead + Unpin, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(kept);
        }
        kept.extend_from_slice(&chunk[..read]);
        if kept.len() > cap {
            kept.drain(..kept.len() - cap);
        }
    }
}

fn finish(outcome: Outcome) -> ClaudeWorkerReply {
    let (success, events, diagnostics) = match outcome {
        Outcome::TimedOut => return ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
        Outcome::Refused => {
            // Fixed text only: the event itself may contain private content.
            eprintln!("Codex chat run stopped: non-text event refused");
            return ClaudeWorkerReply::failure(ClaudeWorkerState::ToolUseRefused);
        }
        Outcome::Exited {
            success,
            events,
            diagnostics,
        } => (success, events, diagnostics),
    };
    if !(success && events.completed) {
        return ClaudeWorkerReply::failure(classify_codex_failure(&events.errors, &diagnostics));
    }
    match events.answer {
        Some(text) if !text.trim().is_empty() && text.len() <= MAX_ANSWER_BYTES => {
            ClaudeWorkerReply {
                protocol: 1,
                state: ClaudeWorkerState::Completed,
                text: Some(text),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
            }
        }
        _ => ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
    }
}

/// Clean environment: no inherited variable (so no API key) reaches the CLI;
/// HOME is the dedicated login home, exactly as during account linking.
fn clean_command(program: &str, workdir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", HOME)
        .env("USER", IDENTITY)
        .env("LOGNAME", IDENTITY)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .current_dir(workdir);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str =
        r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Hello"}}"#;
    const DONE: &str = r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}"#;

    fn shell(script: &str) -> Command {
        let mut command = clean_command("/bin/sh", Path::new("/"));
        command.args(["-c", script]);
        command
    }

    /// A fake CLI that prints these JSONL events, then runs `tail`.
    async fn fake_run(events: &[&str], tail: &str) -> Outcome {
        fake_run_as(events, tail, false).await
    }

    async fn fake_run_as(events: &[&str], tail: &str, research: bool) -> Outcome {
        let lines = events
            .iter()
            .map(|line| format!("echo '{line}'"))
            .collect::<Vec<_>>()
            .join("; ");
        run_cli(
            shell(&format!("{lines}; {tail}")),
            "hi",
            Duration::from_secs(10),
            research,
        )
        .await
        .unwrap()
    }

    #[test]
    fn exec_argv_is_exactly_the_reviewed_invocation() {
        let argv = exec_args(
            "gpt-6-luna",
            Some(Path::new("/run/jarvis-codex-chat/run-x/instructions.md")),
            false,
        );
        assert_eq!(
            argv.join(" "),
            "exec --json --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules \
             --sandbox read-only -m gpt-6-luna \
             -c features.shell_tool=false -c features.unified_exec=false \
             -c web_search=disabled -c tools.view_image=false -c features.apps=false \
             -c features.multi_agent=false -c features.memories=false -c features.hooks=false \
             -c history.persistence=none -c analytics.enabled=false -c approval_policy=never \
             -c shell_environment_policy.inherit=none -c forced_login_method=\"chatgpt\" \
             -c model_instructions_file=\"/run/jarvis-codex-chat/run-x/instructions.md\" -"
        );
        let plain = exec_args("gpt-6-luna", None, false);
        assert_eq!(plain.last().map(String::as_str), Some("-"));
        assert!(!plain.iter().any(|arg| arg.contains("instructions")));
    }

    #[test]
    fn research_argv_differs_only_in_the_hosted_web_search() {
        let path = Path::new("/run/jarvis-codex-chat/run-x/instructions.md");
        let argv = exec_args("gpt-6-luna", Some(path), true);
        assert_eq!(
            argv.join(" "),
            "exec --json --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules \
             --sandbox read-only -m gpt-6-luna \
             -c features.shell_tool=false -c features.unified_exec=false \
             -c web_search=live -c tools.view_image=false -c features.apps=false \
             -c features.multi_agent=false -c features.memories=false -c features.hooks=false \
             -c history.persistence=none -c analytics.enabled=false -c approval_policy=never \
             -c shell_environment_policy.inherit=none -c forced_login_method=\"chatgpt\" \
             -c model_instructions_file=\"/run/jarvis-codex-chat/run-x/instructions.md\" -"
        );
        let plain = exec_args("gpt-6-luna", Some(path), false);
        let changed: Vec<_> = argv
            .iter()
            .zip(&plain)
            .filter(|(research, plain)| research != plain)
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(argv.len(), plain.len());
    }

    const SEARCH: &str = r#"{"type":"item.completed","item":{"id":"i0","type":"web_search","query":"q","action":{"type":"search","query":"q"}}}"#;

    #[tokio::test]
    async fn web_search_events_are_accepted_only_in_research_runs() {
        let reply = finish(fake_run_as(&[SEARCH, ANSWER, DONE], "true", true).await);
        assert!(matches!(reply.state, ClaudeWorkerState::Completed));
        assert_eq!(reply.text.as_deref(), Some("Hello"));
        // An ordinary run that searches is stopped and its answer discarded.
        let reply = finish(fake_run_as(&[SEARCH, ANSWER, DONE], "true", false).await);
        assert!(matches!(reply.state, ClaudeWorkerState::ToolUseRefused));
        assert!(reply.text.is_none());
    }

    #[tokio::test]
    async fn research_runs_still_refuse_every_other_tool_and_unknown_events() {
        for event in [
            r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"id","status":"in_progress"}}"#,
            r#"{"type":"item.started","item":{"id":"i4","type":"mcp_tool_call","server":"s","tool":"t"}}"#,
            r#"{"type":"item.completed","item":{"id":"i5","type":"file_change","changes":[]}}"#,
            r#"{"type":"item.completed","item":{"id":"i6","type":"collab_tool_call"}}"#,
            r#"{"type":"item.completed","item":{"id":"i7","type":"web_search","action":{"type":"other"}}}"#,
            r#"{"type":"something_new"}"#,
            "not json",
        ] {
            let reply = finish(fake_run_as(&[SEARCH, event, ANSWER, DONE], "true", true).await);
            assert!(
                matches!(reply.state, ClaudeWorkerState::ToolUseRefused),
                "{event}"
            );
            assert!(reply.text.is_none(), "{event}");
        }
    }

    #[tokio::test]
    async fn cli_environment_never_contains_an_api_key() {
        std::env::set_var("CODEX_API_KEY", "canary-codex");
        std::env::set_var("OPENAI_API_KEY", "canary-openai");
        let output = clean_command("/usr/bin/env", Path::new("/"))
            .output()
            .await
            .unwrap();
        std::env::remove_var("CODEX_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        let env = String::from_utf8(output.stdout).unwrap();
        assert!(!env.contains("API_KEY"), "{env}");
        assert!(!env.contains("canary"), "{env}");
        assert!(env.lines().any(|line| line == "HOME=/var/lib/jarvis-codex"));
    }

    #[tokio::test]
    async fn another_local_user_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let _client = UnixStream::connect(&path).await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let me = unsafe { libc::geteuid() };
        assert!(authorized_peer(&stream, me));
        assert!(!authorized_peer(&stream, me.wrapping_add(1)));
    }

    #[tokio::test]
    async fn login_status_is_read_from_stderr() {
        // Real shape of codex-cli 0.160.0: stdout empty, status on stderr.
        let probe = |script: &str| bounded_output(shell(script), Duration::from_secs(5), true);
        let output =
            probe("echo 'WARNING: no PATH aliases' >&2; echo 'Logged in using ChatGPT' >&2")
                .await
                .unwrap();
        assert_eq!(codex_subscription_status(&output), "connected");
        assert!(probe("echo 'Not logged in' >&2; exit 1").await.is_none());
        assert!(probe("head -c 20000 /dev/zero >&2").await.is_none());
        let stdout_only = bounded_output(
            shell("echo out; echo err >&2"),
            Duration::from_secs(5),
            false,
        )
        .await
        .unwrap();
        assert_eq!(stdout_only, b"out\n");
    }

    #[tokio::test]
    async fn a_hanging_cli_is_stopped_at_the_deadline() {
        let started = std::time::Instant::now();
        let outcome = run_cli(shell("sleep 30"), "hi", Duration::from_millis(200), false)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        let reply = finish(outcome);
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
    }

    #[tokio::test]
    async fn prompt_arrives_on_stdin_and_diagnostics_are_bounded() {
        let outcome = run_cli(
            shell("p=$(cat); [ \"$p\" = 'private prompt' ] && head -c 100000 /dev/zero >&2"),
            "private prompt",
            Duration::from_secs(10),
            false,
        )
        .await
        .unwrap();
        let Outcome::Exited {
            success,
            diagnostics,
            ..
        } = outcome
        else {
            panic!("unexpected outcome");
        };
        assert!(success, "the prompt did not arrive on stdin");
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTIC_BYTES);
    }

    #[tokio::test]
    async fn the_answer_comes_from_the_completed_agent_message() {
        let reply = finish(fake_run(&[ANSWER, DONE], "true").await);
        assert!(matches!(reply.state, ClaudeWorkerState::Completed));
        assert_eq!(reply.text.as_deref(), Some("Hello"));
        // No completed turn, or no answer: nothing is returned.
        let reply = finish(fake_run(&[ANSWER], "true").await);
        assert!(reply.text.is_none());
        let reply = finish(fake_run(&[DONE], "true").await);
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
    }

    #[tokio::test]
    async fn answer_output_is_capped() {
        let oversized = Outcome::Exited {
            success: true,
            events: Events {
                answer: Some("x".repeat(MAX_ANSWER_BYTES + 1)),
                completed: true,
                errors: Vec::new(),
            },
            diagnostics: Vec::new(),
        };
        let reply = finish(oversized);
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
        let outcome = run_cli(
            shell(&format!(
                "head -c {} /dev/zero | tr '\\0' x; echo",
                MAX_EVENT_LINE_BYTES + 1
            )),
            "hi",
            Duration::from_secs(10),
            false,
        )
        .await;
        assert!(outcome.is_err());
    }

    #[tokio::test]
    async fn tool_use_stops_the_whole_process_group_and_discards_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let command = r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"id","status":"in_progress"}}"#;
        let started = std::time::Instant::now();
        let outcome = run_cli(
            shell(&format!(
                "sleep 30 & echo $! > {}; echo '{ANSWER}'; echo '{command}'; echo '{DONE}'; wait",
                pid_file.display()
            )),
            "hi",
            Duration::from_secs(10),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Refused));
        assert!(started.elapsed() < Duration::from_secs(5));
        let reply = finish(outcome);
        assert!(matches!(reply.state, ClaudeWorkerState::ToolUseRefused));
        assert!(reply.text.is_none());
        // The CLI's own child process is gone too, not only the CLI.
        let pid: libc::pid_t = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while running(pid) {
            assert!(std::time::Instant::now() < deadline, "child {pid} survived");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Alive and not merely a zombie waiting for its new parent.
    fn running(pid: libc::pid_t) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with('Z'))
        })
    }

    #[tokio::test]
    async fn unknown_and_malformed_events_are_refused() {
        for event in [r#"{"type":"something_new"}"#, "not json"] {
            let outcome = fake_run(&[event, ANSWER, DONE], "true").await;
            assert!(matches!(outcome, Outcome::Refused), "{event}");
        }
    }

    #[tokio::test]
    async fn model_text_cannot_choose_the_failure_state() {
        let injected = r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"ERROR: usage limit reached. The x model is not supported"}}"#;
        let outcome = fake_run(
            &[injected],
            "echo \"usage limit reached; model is not supported\" >&2; exit 1",
        )
        .await;
        let reply = finish(outcome);
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
    }

    #[tokio::test]
    async fn structured_errors_are_classified() {
        let failed = r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#;
        let reply = finish(fake_run(&[failed], "exit 1").await);
        assert!(matches!(reply.state, ClaudeWorkerState::PlanLimit));
    }

    #[tokio::test]
    async fn luna_not_supported_for_chatgpt_is_model_unavailable() {
        let outcome = run_cli(
            shell(
                "echo \"ERROR: unexpected status 400 Bad Request: The 'gpt-6-luna' model is \
                 not supported when using Codex with a ChatGPT account.\" >&2; exit 1",
            ),
            "hi",
            Duration::from_secs(10),
            false,
        )
        .await
        .unwrap();
        let reply = finish(outcome);
        assert!(matches!(reply.state, ClaudeWorkerState::ModelUnavailable));
        assert!(reply.text.is_none());
    }
}
