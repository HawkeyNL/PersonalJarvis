//! Finite, text-only Codex chat worker for subscription models (for example
//! GPT-6 Luna through a ChatGPT plan). The official CLI runs as jarvis-codex
//! with every tool off, in an empty private directory, never inside Core and
//! never with an API key. It does not touch the Codex coding broker.

mod subscription_worker;

use std::{path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use jarvis_llm::{
    claude_worker_protocol::{
        ClaudeWorkerReply, ClaudeWorkerRequest, ClaudeWorkerState, MAX_REQUEST_BYTES,
    },
    codex_chat_protocol::{
        classify_codex_failure, codex_subscription_status, reviewed_codex_version,
        REVIEWED_CODEX_VERSION,
    },
};
use subscription_worker::{
    authorized_peer, inherited_listener, named_uid, send, validate_private_dir,
    validate_root_binary,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
    sync::Semaphore,
};

const CODEX: &str = "/usr/local/bin/codex";
/// Login home written by `jarvis accounts connect codex`
/// (`codex login --device-auth` as jarvis-codex).
const HOME: &str = "/var/lib/jarvis-codex";
const RUNTIME: &str = "/run/jarvis-codex-chat";
const MAX_PARALLEL_RUNS: usize = 2;
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ANSWER_BYTES: u64 = 128 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    let worker_uid = named_uid("jarvis-codex")?;
    let core_uid = named_uid("jarvis")?;
    if unsafe { libc::geteuid() } != worker_uid {
        bail!("Codex chat worker must run as the jarvis-codex identity");
    }
    // A paid API key must never be reachable, not even by accident.
    if std::env::vars_os().any(|(key, _)| key.to_string_lossy().ends_with("_API_KEY")) {
        bail!("Codex chat worker refuses to start with an API key in its environment");
    }
    validate_root_binary(CODEX)?;
    validate_private_dir(HOME, worker_uid)?;
    validate_private_dir(RUNTIME, worker_uid)?;
    let listener = inherited_listener()?;
    let permits = std::sync::Arc::new(Semaphore::new(MAX_PARALLEL_RUNS));
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
        tokio::spawn(async move {
            let _permit = permit;
            let reply = match tokio::time::timeout(
                RUN_TIMEOUT + Duration::from_secs(5),
                handle(&mut stream),
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

async fn handle(stream: &mut UnixStream) -> Result<ClaudeWorkerReply> {
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
    let version = bounded_stdout(&["--version"], Duration::from_secs(3)).await;
    if !version.is_some_and(|output| reviewed_codex_version(&output, REVIEWED_CODEX_VERSION)) {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::IncompatibleRuntime,
        ));
    }
    let login = bounded_stdout(&["login", "status"], Duration::from_secs(5)).await;
    if !login.is_some_and(|output| codex_subscription_status(&output) == "connected") {
        return Ok(ClaudeWorkerReply::failure(
            ClaudeWorkerState::SubscriptionUnavailable,
        ));
    }
    run_official_client(request).await
}

/// Non-generative status probe with bounded output and time.
async fn bounded_stdout(args: &[&str], limit: Duration) -> Option<Vec<u8>> {
    let mut command = clean_command(CODEX, Path::new(RUNTIME));
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut bytes = Vec::new();
    let status = tokio::time::timeout(limit, async {
        stdout.take(16 * 1024 + 1).read_to_end(&mut bytes).await?;
        child.wait().await
    })
    .await
    .ok()?
    .ok()?;
    (status.success() && bytes.len() <= 16 * 1024).then_some(bytes)
}

/// The reviewed `codex exec` invocation: ephemeral, no user config or rules,
/// read-only sandbox and every tool, app, memory, hook, history and analytics
/// feature off. The prompt arrives on stdin (`-`), never as an argument.
fn exec_args(model: &str, answer: &Path, instructions: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = [
        "exec",
        "--ephemeral",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "--sandbox",
        "read-only",
        "-m",
        model,
        "-o",
    ]
    .map(String::from)
    .to_vec();
    args.push(answer.display().to_string());
    for setting in [
        "features.shell_tool=false",
        "features.unified_exec=false",
        "web_search=disabled",
        "tools.view_image=false",
        "features.apps=false",
        "features.multi_agent=false",
        "features.memories=false",
        "features.hooks=false",
        "history.persistence=none",
        "analytics.enabled=false",
        "approval_policy=never",
        "shell_environment_policy.inherit=none",
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

async fn run_official_client(request: ClaudeWorkerRequest) -> Result<ClaudeWorkerReply> {
    // Per-run private (0700) directory: an empty neutral workdir, the answer
    // file and the optional instructions file. Removed on drop.
    let run = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(RUNTIME)?;
    let workdir = run.path().join("work");
    tokio::fs::DirBuilder::new()
        .mode(0o700)
        .create(&workdir)
        .await?;
    let answer = run.path().join("answer.txt");
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
    command.args(exec_args(&request.model, &answer, instructions.as_deref()));
    let outcome = run_cli(command, &request.prompt, RUN_TIMEOUT).await?;
    Ok(finish(outcome, &answer).await)
}

#[derive(Debug)]
enum Outcome {
    Exited { success: bool, diagnostics: Vec<u8> },
    TimedOut,
}

async fn run_cli(mut command: Command, prompt: &str, limit: Duration) -> Result<Outcome> {
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().context("Codex stdin unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("Codex diagnostics unavailable")?;
    let run = async {
        // Feed the prompt while draining diagnostics, so neither pipe can
        // stall the CLI. A CLI that exits early decides via its status.
        let feed = async move {
            let _ = stdin.write_all(prompt.as_bytes()).await;
        };
        let ((), diagnostics) = tokio::join!(feed, read_tail(stderr, MAX_DIAGNOSTIC_BYTES));
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>(Outcome::Exited {
            success: status.success(),
            diagnostics: diagnostics?,
        })
    };
    match tokio::time::timeout(limit, run).await {
        Ok(outcome) => outcome,
        // Dropping the child kills it (kill_on_drop).
        Err(_) => Ok(Outcome::TimedOut),
    }
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

async fn finish(outcome: Outcome, answer: &Path) -> ClaudeWorkerReply {
    match outcome {
        Outcome::TimedOut => ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
        Outcome::Exited {
            success: false,
            diagnostics,
        } => ClaudeWorkerReply::failure(classify_codex_failure(&diagnostics)),
        Outcome::Exited { success: true, .. } => match read_answer(answer).await {
            Some(text) if !text.trim().is_empty() => ClaudeWorkerReply {
                protocol: 1,
                state: ClaudeWorkerState::Completed,
                text: Some(text),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            _ => ClaudeWorkerReply::failure(ClaudeWorkerState::RuntimeFailure),
        },
    }
}

/// The final message, only from a regular file within the size bound.
async fn read_answer(path: &Path) -> Option<String> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;
    if !meta.is_file() || meta.len() > MAX_ANSWER_BYTES {
        return None;
    }
    let mut text = String::new();
    tokio::fs::File::open(path)
        .await
        .ok()?
        .take(MAX_ANSWER_BYTES + 1)
        .read_to_string(&mut text)
        .await
        .ok()?;
    (text.len() as u64 <= MAX_ANSWER_BYTES).then_some(text)
}

/// Clean environment: no inherited variable (so no API key) reaches the CLI;
/// HOME is the dedicated login home, exactly as during account linking.
fn clean_command(program: &str, workdir: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", HOME)
        .env("USER", "jarvis-codex")
        .env("LOGNAME", "jarvis-codex")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .current_dir(workdir);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = clean_command("/bin/sh", Path::new("/"));
        command.args(["-c", script]);
        command
    }

    #[test]
    fn exec_argv_is_exactly_the_reviewed_invocation() {
        let argv = exec_args(
            "gpt-6-luna",
            Path::new("/run/jarvis-codex-chat/run-x/answer.txt"),
            Some(Path::new("/run/jarvis-codex-chat/run-x/instructions.md")),
        );
        assert_eq!(
            argv.join(" "),
            "exec --ephemeral --skip-git-repo-check --ignore-user-config --ignore-rules \
             --sandbox read-only -m gpt-6-luna -o /run/jarvis-codex-chat/run-x/answer.txt \
             -c features.shell_tool=false -c features.unified_exec=false \
             -c web_search=disabled -c tools.view_image=false -c features.apps=false \
             -c features.multi_agent=false -c features.memories=false -c features.hooks=false \
             -c history.persistence=none -c analytics.enabled=false -c approval_policy=never \
             -c shell_environment_policy.inherit=none \
             -c model_instructions_file=\"/run/jarvis-codex-chat/run-x/instructions.md\" -"
        );
        let plain = exec_args("gpt-6-luna", Path::new("/a"), None);
        assert_eq!(plain.last().map(String::as_str), Some("-"));
        assert!(!plain.iter().any(|arg| arg.contains("instructions")));
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
    async fn a_hanging_cli_is_stopped_at_the_deadline() {
        let started = std::time::Instant::now();
        let outcome = run_cli(shell("sleep 30"), "hi", Duration::from_millis(200))
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
        let reply = finish(outcome, Path::new("/nonexistent")).await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
    }

    #[tokio::test]
    async fn prompt_arrives_on_stdin_and_diagnostics_are_bounded() {
        let outcome = run_cli(
            shell("p=$(cat); [ \"$p\" = 'private prompt' ] && head -c 100000 /dev/zero >&2"),
            "private prompt",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let Outcome::Exited {
            success,
            diagnostics,
        } = outcome
        else {
            panic!("unexpected timeout");
        };
        assert!(success, "the prompt did not arrive on stdin");
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTIC_BYTES);
    }

    #[tokio::test]
    async fn answer_output_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let answer = dir.path().join("answer.txt");
        std::fs::write(&answer, "x".repeat(MAX_ANSWER_BYTES as usize)).unwrap();
        assert!(read_answer(&answer).await.is_some());
        std::fs::write(&answer, "x".repeat(MAX_ANSWER_BYTES as usize + 1)).unwrap();
        assert!(read_answer(&answer).await.is_none());
        let reply = finish(
            Outcome::Exited {
                success: true,
                diagnostics: Vec::new(),
            },
            &answer,
        )
        .await;
        assert!(matches!(reply.state, ClaudeWorkerState::RuntimeFailure));
        assert!(reply.text.is_none());
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&answer, &link).unwrap();
        assert!(read_answer(&link).await.is_none());
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
        )
        .await
        .unwrap();
        let reply = finish(outcome, Path::new("/nonexistent")).await;
        assert!(matches!(reply.state, ClaudeWorkerState::ModelUnavailable));
        assert!(reply.text.is_none());
    }
}
