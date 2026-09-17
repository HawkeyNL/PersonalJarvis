//! Host-local device administration. No TCP listener or HTTP route exists.
use crate::{audit::record_security_event, AppState};
use anyhow::{bail, Context};
use jarvis_identity::surreal::local_devices::{self, LocalRootPeer};
use jarvis_privileged::local_devices::{DeviceRequest, REQUEST_LIMIT, RESPONSE_LIMIT, SOCKET_PATH};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
};

pub fn start(state: AppState) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let parent = fs::symlink_metadata("/run/jarvis-core-admin")
        .context("systemd-managed local administration runtime directory unavailable")?;
    if !parent.is_dir() || parent.uid() != unsafe { libc::geteuid() } || parent.mode() & 0o077 != 0
    {
        bail!("unsafe local administration runtime directory");
    }
    match fs::symlink_metadata(SOCKET_PATH) {
        Ok(info) if info.file_type().is_socket() && info.uid() == parent.uid() => {
            fs::remove_file(SOCKET_PATH)?
        }
        Ok(_) => bail!("unsafe local administration socket"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(SOCKET_PATH)?;
    fs::set_permissions(SOCKET_PATH, fs::Permissions::from_mode(0o600))?;
    Ok(tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(4));
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let _permit = permit;
                // Logs deliberately exclude the request, peer PID, names, and
                // database errors; operation outcomes have a separate audit.
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(10), handle(stream, &state)).await,
                    Ok(Ok(()))
                ) {
                    tracing::warn!("local device administration connection denied or unavailable");
                }
            });
        }
    }))
}

async fn handle(mut stream: UnixStream, state: &AppState) -> anyhow::Result<()> {
    // Check kernel identity before allocating/parsing even a bounded request.
    LocalRootPeer::authenticate(&stream).map_err(|_| anyhow::anyhow!("root peer required"))?;
    let request = read_request(&mut stream).await?;
    let peer = LocalRootPeer::authenticate(&stream)?;
    let result = execute(state, &peer, request).await;
    let response = match result {
        Ok(value) => json!({"ok":true,"data":value}),
        Err(_) => {
            json!({"ok":false,"error":"device operation refused; refresh state and verify the selected request"})
        }
    };
    let mut encoded = serde_json::to_vec(&response)?;
    if encoded.len() >= RESPONSE_LIMIT {
        bail!("local device response too large");
    }
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    Ok(())
}

async fn read_request(reader: impl tokio::io::AsyncRead + Unpin) -> anyhow::Result<DeviceRequest> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take((REQUEST_LIMIT + 1) as u64))
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.last() != Some(&b'\n') {
        bail!("incomplete or oversized local device request");
    }
    DeviceRequest::parse(&bytes).map_err(anyhow::Error::msg)
}

async fn execute(
    state: &AppState,
    peer: &LocalRootPeer<'_>,
    request: DeviceRequest,
) -> anyhow::Result<Value> {
    let owner = jarvis_identity::first_user(&state.db).await?;
    let result = match request {
        DeviceRequest::List {} => {
            let devices = match owner {
                Some(owner) => jarvis_identity::list_active_devices(&state.db, owner.id).await?,
                None => vec![],
            };
            return Ok(json!({"devices":devices}));
        }
        DeviceRequest::Pending {} => {
            let pending = match owner {
                Some(owner) => {
                    jarvis_identity::pending_pairing_requests(&state.db, owner.id).await?
                }
                None => vec![],
            };
            // Never return candidate nonce/key bytes or account credentials.
            return Ok(
                json!({"requests": pending.into_iter().map(|r| json!({"id":r.id,"name":r.candidate_name,"platform":r.candidate_platform,"fingerprint":r.candidate_fingerprint,"expires_at":r.expires_at.unix_timestamp()})).collect::<Vec<_>>()}),
            );
        }
        DeviceRequest::Approve {
            request_id,
            fingerprint,
        } => local_devices::approve(&state.db, peer, request_id, &fingerprint)
            .await
            .map(|d| ("local.device.approve", json!({"device_id":d.id}))),
        DeviceRequest::Deny { request_id } => local_devices::deny(&state.db, peer, request_id)
            .await
            .map(|()| ("local.device.deny", json!({"status":"denied"}))),
        DeviceRequest::Revoke { device_id } => local_devices::revoke(&state.db, peer, device_id)
            .await
            .map(|owner| {
                state.realtime.disconnect_owner(owner.id, Some(device_id));
                ("local.device.revoke", json!({"status":"revoked"}))
            }),
    };
    match result {
        Ok((action, value)) => {
            record_security_event(state, None, action, "ok", None).await;
            Ok(value)
        }
        Err(error) => {
            record_security_event(state, None, "local.device.mutation", "denied", None).await;
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framing_is_bounded_complete_and_strict() {
        assert_eq!(
            read_request(&b"{\"operation\":\"list\"}\n"[..])
                .await
                .unwrap(),
            DeviceRequest::List {}
        );
        for input in [
            b"{\"operation\":\"list\"}".to_vec(),
            b"{\"operation\":\"list\",\"uid\":0}\n".to_vec(),
            vec![b' '; REQUEST_LIMIT + 100],
        ] {
            assert!(read_request(input.as_slice()).await.is_err());
        }
    }
}
