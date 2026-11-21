//! The reader side of the socket: peer-uid gate, bounded framing, one fetch
//! per connection and one audit line with the host and outcome only.

use std::{
    fs,
    future::Future,
    io::Write as _,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

use crate::{
    check_url,
    fetch::{Fetcher, Resolve},
    Refusal, Reply, Request, MAX_DENY_HOSTS, MAX_REPLY_BYTES, MAX_REQUEST_BYTES, PROTOCOL_VERSION,
};

/// Audit log target: one line per connection with the requested host and
/// the outcome, never the path, query, page text or an address.
const AUDIT: &str = "jarvis_page_reader::audit";
/// Each fetch holds up to 2 MiB of body plus its text; with
/// `MemoryMax=256M` this leaves ample room.
const MAX_CONNECTIONS: usize = 4;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
/// Pause after an accept error that passes (out of file descriptors, an
/// aborted connection).
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Resolve a comma-separated list of numeric uids or user names. Fails on an
/// empty list or an unknown user.
pub fn resolve_peers(spec: &str) -> Result<Vec<u32>, String> {
    let mut uids = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let uid = if entry.bytes().all(|b| b.is_ascii_digit()) {
            entry
                .parse()
                .map_err(|_| format!("invalid peer uid {entry}"))?
        } else {
            crate::lookup_uid(entry).ok_or_else(|| format!("unknown peer user {entry}"))?
        };
        uids.push(uid);
    }
    if uids.is_empty() {
        return Err("no allowed peer configured".into());
    }
    Ok(uids)
}

/// Bind the socket (0660; the parent directory comes from systemd), replacing
/// only a stale socket of this uid, never another file type or owner.
pub fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    if let Ok(meta) = fs::symlink_metadata(socket) {
        // SAFETY: geteuid has no preconditions.
        if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::other(
                "refusing unsafe existing reader path",
            ));
        }
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// Serve until `shutdown` resolves, then wait up to [`DRAIN_TIMEOUT`] for
/// requests in flight. An accept error ends the loop (systemd restarts).
pub async fn serve<R: Resolve + 'static>(
    listener: UnixListener,
    fetcher: Arc<Fetcher<R>>,
    allowed_uids: Arc<[u32]>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    let result = loop {
        let stream = tokio::select! {
            () = &mut shutdown => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) if is_transient(&error) => {
                    tracing::warn!(target: AUDIT, %error, "accept failed; retrying");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                    continue;
                }
                Err(error) => break Err(error),
            },
        };
        // The kernel-reported peer uid is the only authority.
        let peer_uid = stream.peer_cred().ok().map(|cred| cred.uid());
        let Some(peer_uid) = peer_uid.filter(|uid| allowed_uids.contains(uid)) else {
            tracing::warn!(target: AUDIT, peer_uid, outcome = "peer_refused", "page read");
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            // Best effort and non-blocking (the std socket keeps O_NONBLOCK).
            if let Ok(mut stream) = stream.into_std() {
                let _ = stream.write(&reply_line(&Reply::Refused {
                    code: Refusal::Busy,
                }));
            }
            tracing::warn!(target: AUDIT, peer_uid, outcome = "busy", "page read");
            continue;
        };
        let fetcher = fetcher.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle(stream, &fetcher, peer_uid).await;
        });
    };
    // New clients are refused at once instead of waiting in the backlog.
    drop(listener);
    let drained = tokio::time::timeout(DRAIN_TIMEOUT, slots.acquire_many(MAX_CONNECTIONS as u32));
    if drained.await.is_err() {
        tracing::warn!(target: AUDIT, "page reader stopped with requests in flight");
    }
    result
}

/// An accept error that passes: the loop waits and goes on instead of
/// ending the reader.
fn is_transient(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ECONNABORTED | libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

async fn handle<R: Resolve>(stream: UnixStream, fetcher: &Fetcher<R>, peer_uid: u32) {
    let started = Instant::now();
    let (read, mut write) = stream.into_split();
    let request = read_request(read).await;
    let host = request
        .as_ref()
        .ok()
        .and_then(|request| check_url(&request.url, &[]).ok())
        .and_then(|url| url.host_str().map(str::to_string));
    let reply = match request {
        Ok(request) => match fetcher.fetch(&request.url, &request.deny_hosts).await {
            Ok(page) => Reply::Page(page),
            Err(code) => Reply::Refused { code },
        },
        Err(code) => Reply::Refused { code },
    };
    let (outcome, truncated) = match &reply {
        Reply::Page(page) => ("page", page.truncated),
        Reply::Refused { code } => (code.code(), false),
    };
    let line = reply_line(&reply);
    let written = tokio::time::timeout(WRITE_TIMEOUT, write.write_all(&line)).await;
    tracing::info!(
        target: AUDIT,
        peer_uid,
        host = host.as_deref(),
        outcome,
        truncated,
        reply_sent = matches!(written, Ok(Ok(()))),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "page read"
    );
}

async fn read_request(read: impl AsyncRead + Unpin) -> Result<Request, Refusal> {
    let mut line = Vec::new();
    // Limit before allocation/read, not after an unbounded read_line.
    let mut reader = BufReader::new(read.take(MAX_REQUEST_BYTES as u64 + 1));
    tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut line))
        .await
        .map_err(|_| Refusal::Timeout)?
        .map_err(|_| Refusal::InvalidRequest)?;
    if line.len() > MAX_REQUEST_BYTES || line.last() != Some(&b'\n') {
        return Err(Refusal::InvalidRequest);
    }
    let request: Request = serde_json::from_slice(&line).map_err(|_| Refusal::InvalidRequest)?;
    if request.v != PROTOCOL_VERSION || request.deny_hosts.len() > MAX_DENY_HOSTS {
        return Err(Refusal::InvalidRequest);
    }
    Ok(request)
}

/// One reply line. A page reply above [`MAX_REPLY_BYTES`] cannot happen
/// (title and text are capped); it would be refused rather than sent.
fn reply_line(reply: &Reply) -> Vec<u8> {
    let fallback = || br#"{"refused":{"code":"upstream"}}"#.to_vec();
    let mut line = serde_json::to_vec(reply).unwrap_or_else(|_| fallback());
    if line.len() >= MAX_REPLY_BYTES {
        line = fallback();
    }
    line.push(b'\n');
    line
}

#[cfg(test)]
mod tests;
