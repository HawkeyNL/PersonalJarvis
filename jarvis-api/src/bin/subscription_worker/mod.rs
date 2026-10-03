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
};

use anyhow::{bail, Context, Result};
use jarvis_llm::claude_worker_protocol::{ClaudeWorkerReply, MAX_REPLY_BYTES};
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
};

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
