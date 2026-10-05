//! Host checks shared by the subscription worker binaries (Claude, Codex
//! chat). Each worker includes this module; it is not a binary of its own.

use std::{
    ffi::{CStr, CString},
    fs,
    os::{
        fd::FromRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use jarvis_llm::claude_worker_protocol::{
    ClaudeWorkerReply, ClaudeWorkerRequest, MAX_REPLY_BYTES, MAX_REQUEST_BYTES,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// Parallel runs per worker. At most one of them may be a research run, so a
/// long web search never takes every slot from ordinary chat.
pub const MAX_PARALLEL_RUNS: usize = 2;
pub const MAX_PARALLEL_RESEARCH_RUNS: usize = 1;
const _: () = assert!(MAX_PARALLEL_RESEARCH_RUNS < MAX_PARALLEL_RUNS);
/// Core writes its whole request at once; a stalled peer loses its slot.
pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// An owner-enabled research run searches the web first.
const RESEARCH_RUN_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the official CLI may run.
pub fn run_timeout(research: bool) -> Duration {
    if research {
        RESEARCH_RUN_TIMEOUT
    } else {
        RUN_TIMEOUT
    }
}

/// The reply deadline after the request was read: the run's own deadline
/// (which stops the CLI) always fires first.
pub fn reply_deadline(research: bool) -> Duration {
    run_timeout(research) + Duration::from_secs(5)
}

/// Ordinary chat needs no extra slot (`Some(None)`). A research run needs the
/// research slot; `None` means it is taken and the request is refused.
pub fn research_slot(
    research: bool,
    slots: &Arc<Semaphore>,
) -> Option<Option<OwnedSemaphorePermit>> {
    if !research {
        return Some(None);
    }
    slots.clone().try_acquire_owned().ok().map(Some)
}

/// One bounded, newline-terminated, valid request from Core.
pub async fn read_request(stream: &mut UnixStream) -> Result<ClaudeWorkerRequest> {
    let mut bytes = Vec::new();
    stream
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_REQUEST_BYTES || !bytes.ends_with(b"\n") {
        bail!("invalid bounded subscription worker request");
    }
    let request: ClaudeWorkerRequest = serde_json::from_slice(&bytes)?;
    if !request.valid() {
        bail!("invalid subscription worker request shape");
    }
    Ok(request)
}

pub fn inherited_listener() -> Result<UnixListener> {
    let pid = std::env::var("LISTEN_PID").context("socket activation PID missing")?;
    let count = std::env::var("LISTEN_FDS").context("socket activation fd missing")?;
    if pid.parse::<u32>()? != std::process::id() || count != "1" {
        bail!("subscription worker requires exactly one systemd Unix socket");
    }
    // systemd passes the first listening fd as 3 for Accept=no socket units.
    let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
    std_listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(std_listener)?)
}

/// Only the unprivileged Core identity may use a worker. The socket mode is
/// the first gate; this kernel-reported peer UID is the second.
pub fn authorized_peer(stream: &UnixStream, core_uid: u32) -> bool {
    stream
        .peer_cred()
        .is_ok_and(|credentials| credentials.uid() == core_uid)
}

pub async fn send(stream: &mut UnixStream, reply: ClaudeWorkerReply) -> Result<()> {
    let bytes = serde_json::to_vec(&reply)?;
    if bytes.len() > MAX_REPLY_BYTES {
        bail!("subscription worker reply exceeded bound");
    }
    stream.write_all(&bytes).await?;
    stream.shutdown().await?;
    Ok(())
}

pub fn named_uid(name: &str) -> Result<u32> {
    let name = CString::new(name)?;
    let record = unsafe { libc::getpwnam(name.as_ptr()) };
    if record.is_null() {
        bail!("required service identity missing");
    }
    let record = unsafe { &*record };
    let shell = unsafe { CStr::from_ptr(record.pw_shell) }.to_str()?;
    if shell != "/usr/sbin/nologin" {
        bail!("service identity has unsafe shell");
    }
    Ok(record.pw_uid)
}

pub fn validate_private_dir(path: &str, uid: u32) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != uid
        || meta.permissions().mode() & 0o777 != 0o700
    {
        bail!("unsafe subscription worker private directory");
    }
    Ok(())
}

pub fn validate_root_binary(path: &str) -> Result<()> {
    let path = Path::new(path);
    for parent in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != 0
            || meta.permissions().mode() & 0o022 != 0
        {
            bail!("unsafe official runtime parent directory");
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.uid() != 0
        || meta.permissions().mode() & 0o022 != 0
        || meta.permissions().mode() & 0o111 == 0
    {
        bail!("unsafe official runtime executable");
    }
    Ok(())
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    #[test]
    fn research_keeps_the_chat_deadline_short_and_its_own_bounded() {
        assert_eq!(reply_deadline(false), Duration::from_secs(125));
        assert_eq!(reply_deadline(true), Duration::from_secs(305));
        assert!(run_timeout(false) < reply_deadline(false));
        assert!(run_timeout(true) < reply_deadline(true));
    }

    #[test]
    fn only_one_research_run_at_a_time_and_chat_never_waits_for_it() {
        let slots = Arc::new(Semaphore::new(MAX_PARALLEL_RESEARCH_RUNS));
        let first = research_slot(true, &slots).expect("first research run");
        assert!(first.is_some());
        assert!(research_slot(true, &slots).is_none());
        // Chat is unaffected while research holds its slot.
        assert!(matches!(research_slot(false, &slots), Some(None)));
        drop(first);
        assert!(research_slot(true, &slots).is_some());
    }
}
