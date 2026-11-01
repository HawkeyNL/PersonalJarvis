//! Read-only IBKR broker: the only process that holds the IBKR OAuth
//! credentials. systemd delivers them with `LoadCredential=` under fixed names;
//! Core reaches the broker only through its peer-uid checked Unix socket.
//!
//! Environment:
//! - `CREDENTIALS_DIRECTORY` (set by systemd): `consumer_key`, `access_token`,
//!   `access_token_secret` (decrypted, hex), `signature_key` (PEM),
//!   `dh_param` (PEM).
//! - `JARVIS_IBKR_BROKER_ALLOWED_PEERS`: comma-separated uids or user names.
//! - `JARVIS_IBKR_BROKER_SOCKET` (optional): socket path.
//! - `JARVIS_IBKR_OAUTH_REALM` (optional): defaults to `limited_poa`.

use std::{path::PathBuf, process::ExitCode, sync::Arc};

use jarvis_ibkr::{
    broker::{server, DEFAULT_SOCKET},
    oauth::{IbkrOAuthClient, OAuthCredentialFiles, OAuthCredentials},
};
use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> ExitCode {
    jarvis_observability::init(true);
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "IBKR broker stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    // No core dumps or same-uid ptrace of a process holding credentials.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err("cannot mark the broker non-dumpable".into());
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err("jarvis-ibkr-broker must not run as root".into());
    }
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let allowed = server::resolve_peers(
        &env("JARVIS_IBKR_BROKER_ALLOWED_PEERS")
            .ok_or("JARVIS_IBKR_BROKER_ALLOWED_PEERS missing")?,
    )?;
    let socket = PathBuf::from(env("JARVIS_IBKR_BROKER_SOCKET").unwrap_or(DEFAULT_SOCKET.into()));
    let realm = env("JARVIS_IBKR_OAUTH_REALM").unwrap_or("limited_poa".into());
    let dir = PathBuf::from(env("CREDENTIALS_DIRECTORY").ok_or("CREDENTIALS_DIRECTORY missing")?);
    if !dir.is_absolute() || !socket.is_absolute() {
        return Err("credential directory and socket must be absolute paths".into());
    }
    let files = OAuthCredentialFiles {
        consumer_key: dir.join("consumer_key"),
        access_token: dir.join("access_token"),
        access_token_secret_prepend_hex: dir.join("access_token_secret"),
        signature_key: dir.join("signature_key"),
        dh_param: dir.join("dh_param"),
    };
    let credentials = OAuthCredentials::load(&files, &realm).map_err(|e| e.to_string())?;
    let client = IbkrOAuthClient::new(credentials).map_err(|e| e.to_string())?;
    let listener = server::bind(&socket).map_err(|e| format!("bind broker socket: {e}"))?;
    let mut term = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
    let mut int = signal(SignalKind::interrupt()).map_err(|e| e.to_string())?;
    tracing::info!(peers = ?allowed, "IBKR read broker ready");
    let shutdown = async {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    };
    let served = server::serve(listener, Arc::new(client), allowed.into(), shutdown).await;
    let _ = std::fs::remove_file(&socket);
    served.map_err(|e| format!("broker socket failed: {e}"))
}
