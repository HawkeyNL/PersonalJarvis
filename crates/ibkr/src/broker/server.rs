//! The broker side of the socket: peer-uid gate, bounded framing, the fixed
//! request dispatch and one audit line per connection.

use std::{
    fs,
    future::Future,
    io::Write as _,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

use super::{BrokerError, BrokerRequest, Reply, MAX_REPLY_BYTES, MAX_REQUEST_BYTES};
use crate::{oauth::IbkrOAuthClient, IbkrError};

/// Audit log target: one line per connection, never a URL, credential or
/// full account id.
const AUDIT: &str = "jarvis_ibkr_broker::audit";
const MAX_CONNECTIONS: usize = 8;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(60);

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
            super::lookup_uid(entry).ok_or_else(|| format!("unknown peer user {entry}"))?
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
        if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::other(
                "refusing unsafe existing broker path",
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
pub async fn serve(
    listener: UnixListener,
    client: Arc<IbkrOAuthClient>,
    allowed_uids: Arc<[u32]>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    // The broker is the only holder of the IBKR session, so it keeps the
    // brokerage session open between reads.
    let keep_alive = tokio::spawn({
        let client = client.clone();
        async move {
            let mut interval = tokio::time::interval(KEEP_ALIVE_INTERVAL);
            loop {
                interval.tick().await;
                if let Err(error) = client.keep_alive().await {
                    tracing::warn!(target: AUDIT, %error, "IBKR keep-alive failed");
                }
            }
        }
    });
    tokio::pin!(shutdown);
    let result = loop {
        let stream = tokio::select! {
            () = &mut shutdown => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(error) => break Err(error),
            },
        };
        // The kernel-reported peer uid is the only authority; nothing in the
        // request can claim an identity.
        let peer_uid = stream.peer_cred().ok().map(|cred| cred.uid());
        let Some(peer_uid) = peer_uid.filter(|uid| allowed_uids.contains(uid)) else {
            tracing::warn!(target: AUDIT, peer_uid, outcome = "peer_refused", "IBKR broker request");
            continue;
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            // Best effort and non-blocking (the std socket keeps O_NONBLOCK;
            // tokio's try_write refuses before the first readiness event).
            if let Ok(mut stream) = stream.into_std() {
                let _ = stream.write(&error_line(BrokerError::Busy));
            }
            tracing::warn!(target: AUDIT, peer_uid, outcome = "busy", "IBKR broker request");
            continue;
        };
        let client = client.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle(stream, &client, peer_uid).await;
        });
    };
    keep_alive.abort();
    // New clients are refused at once instead of waiting in the backlog.
    drop(listener);
    let drained = tokio::time::timeout(DRAIN_TIMEOUT, slots.acquire_many(MAX_CONNECTIONS as u32));
    if drained.await.is_err() {
        tracing::warn!(target: AUDIT, "IBKR broker stopped with requests in flight");
    }
    result
}

async fn handle(stream: UnixStream, client: &IbkrOAuthClient, peer_uid: u32) {
    let started = Instant::now();
    let (read, mut write) = stream.into_split();
    let request = read_request(read).await;
    let (kind, account, reply) = match &request {
        Ok(request) => {
            let reply = tokio::time::timeout(super::UPSTREAM_DEADLINE, execute(client, request))
                .await
                .unwrap_or(Err(BrokerError::Timeout));
            (request.kind(), request.account_id().map(mask), reply)
        }
        Err(error) => ("invalid", None, Err(error.clone())),
    };
    let outcome = match &reply {
        Ok(_) => "ok".to_string(),
        Err(error) => format!("{error:?}"),
    };
    let line = reply.unwrap_or_else(error_line);
    let written = tokio::time::timeout(WRITE_TIMEOUT, write.write_all(&line)).await;
    tracing::info!(
        target: AUDIT,
        peer_uid,
        kind,
        account = account.as_deref(),
        outcome,
        reply_sent = matches!(written, Ok(Ok(()))),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "IBKR broker request"
    );
}

async fn read_request(read: impl AsyncRead + Unpin) -> Result<BrokerRequest, BrokerError> {
    let mut line = Vec::new();
    // Limit before allocation/read, not after an unbounded read_line.
    let mut reader = BufReader::new(read.take(MAX_REQUEST_BYTES as u64 + 1));
    tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut line))
        .await
        .map_err(|_| BrokerError::Timeout)?
        .map_err(|_| BrokerError::InvalidRequest)?;
    if line.len() > MAX_REQUEST_BYTES || line.last() != Some(&b'\n') {
        return Err(BrokerError::InvalidRequest);
    }
    serde_json::from_slice(&line).map_err(|_| BrokerError::InvalidRequest)
}

/// The only upstream calls the broker can make. Parameters are validated by
/// [`IbkrOAuthClient`] before anything is sent.
async fn execute(
    client: &IbkrOAuthClient,
    request: &BrokerRequest,
) -> Result<Vec<u8>, BrokerError> {
    match request {
        BrokerRequest::Accounts {} => encode(client.accounts().await),
        BrokerRequest::Positions { account_id } => encode(client.positions(account_id).await),
        BrokerRequest::Ledger { account_id } => encode(client.ledger(account_id).await),
        BrokerRequest::SecdefSearch { symbol } => encode(client.search(symbol).await),
        BrokerRequest::Snapshot { conids } => encode(client.snapshot(conids).await),
        BrokerRequest::History { conid, period, bar } => {
            encode(client.history(*conid, period, bar).await)
        }
    }
}

fn encode<T: Serialize>(result: Result<T, IbkrError>) -> Result<Vec<u8>, BrokerError> {
    let data = result.map_err(|error| {
        // IbkrError messages are fixed texts without URLs or credentials.
        tracing::warn!(target: AUDIT, %error, "IBKR upstream request failed");
        BrokerError::from(&error)
    })?;
    let mut line = serde_json::to_vec(&Reply::Ok(data)).map_err(|_| BrokerError::Upstream)?;
    line.push(b'\n');
    if line.len() > MAX_REPLY_BYTES {
        return Err(BrokerError::TooLarge);
    }
    Ok(line)
}

pub(super) fn error_line(error: BrokerError) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Reply::<()>::Error(error))
        .unwrap_or_else(|_| br#"{"error":{"code":"upstream"}}"#.to_vec());
    line.push(b'\n');
    line
}

/// Last three characters of a well-formed account id, for audit lines only.
pub(super) fn mask(account_id: &str) -> String {
    if !(1..=32).contains(&account_id.len())
        || !account_id.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        "invalid".into()
    } else if account_id.len() > 3 {
        format!("***{}", &account_id[account_id.len() - 3..])
    } else {
        "***".into()
    }
}
