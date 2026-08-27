//! Local-only broker for the finite Codex/OpenSandbox protocol.
//!
//! This process deliberately has no HTTP listener, shell endpoint, host-path
//! argument, environment mutation or generic process-spawn API.  It validates
//! a device-signed request independently of Core.  Activation remains gated on
//! a task-scoped Codex credential provider; until then valid requests are
//! audited and denied rather than falling back to a host Codex process.

use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::Path,
};

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

use jarvis_config::AppConfig;
use jarvis_identity as identity;

const DEFAULT_SOCKET: &str = "/run/jarvis-codex-broker/broker.sock";
const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
struct Reply {
    status: &'static str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Root is intentionally not required: this dedicated account owns neither
    // protected persona/configuration nor Docker. systemd supplies its socket
    // directory and a narrowly readable database principal.
    if unsafe { libc::geteuid() } == 0 {
        bail!("jarvis-codex-broker must not run as root");
    }
    let config = AppConfig::load().context("load Codex broker configuration")?;
    let socket = if config.codex_broker_socket.trim().is_empty() {
        DEFAULT_SOCKET
    } else {
        config.codex_broker_socket.trim()
    };
    let db = jarvis_store::connect(
        &config.surreal_endpoint,
        &config.surreal_namespace,
        &config.surreal_database,
        &config.surreal_username,
        &config.surreal_password,
    )
    .await?;
    prepare_socket(socket)?;
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
    tracing::info!(
        socket,
        "Codex broker ready; scoped credential gate remains enforced"
    );
    loop {
        let (stream, _) = listener.accept().await?;
        let db = db.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &db).await {
                tracing::warn!(%error, "Codex broker request denied");
            }
        });
    }
}

fn prepare_socket(socket: &str) -> anyhow::Result<()> {
    let path = Path::new(socket);
    let parent = path.parent().context("Codex socket has no parent")?;
    let meta = fs::symlink_metadata(parent).context("Codex socket directory missing")?;
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        bail!("unsafe Codex socket directory");
    }
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            bail!("refusing non-socket Codex broker path");
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

async fn handle(stream: UnixStream, db: &jarvis_store::Database) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let length = BufReader::new(read).read_line(&mut line).await?;
    if length == 0 || length > MAX_REQUEST_BYTES {
        bail!("invalid Codex broker request size");
    }
    let request: jarvis_codex::BrokerRequest =
        serde_json::from_str(&line).context("invalid Codex broker request")?;
    request
        .validate_shape()
        .map_err(|_| anyhow::anyhow!("invalid Codex operation"))?;
    let Some(signed) = request.signed_request() else {
        // Cancel and status are intentionally not callable through this raw
        // socket. Their authenticated session-bound control path belongs to
        // the future run registry, not an untrusted local caller.
        audit(db, None, "denied", "unsupported raw operation").await;
        bail!("unsupported raw Codex broker operation");
    };
    let outcome = validate_signature(db, signed).await;
    if let Err(error) = outcome {
        audit(db, Some(signed.device_id), "denied", "signature or expiry").await;
        return Err(error);
    }
    // The only supported OpenSandbox adapter deliberately refuses to inject a
    // secret. Do not consume an approval or create a sandbox until an
    // authenticated, task-scoped credential vault is proven end-to-end.
    audit(
        db,
        Some(signed.device_id),
        "denied",
        "scoped Codex credential unavailable",
    )
    .await;
    write
        .write_all(serde_json::to_string(&Reply { status: "denied" })?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    Ok(())
}

async fn validate_signature(
    db: &jarvis_store::Database,
    request: &jarvis_codex::SignedCodingRequest,
) -> anyhow::Result<()> {
    request
        .reject_if_expired(time::OffsetDateTime::now_utc())
        .map_err(|_| anyhow::anyhow!("expired Codex approval"))?;
    let message = request
        .message()
        .map_err(|_| anyhow::anyhow!("invalid Codex approval"))?;
    let signature = hex::decode(&request.signature_hex)
        .map_err(|_| anyhow::anyhow!("invalid Codex signature"))?;
    identity::verify_device_signature(db, request.user_id, request.device_id, &message, &signature)
        .await
        .map_err(|_| anyhow::anyhow!("untrusted Codex owner device"))?;
    if jarvis_codex::request_policy(true) != jarvis_policy::PolicyDecision::RequireApproval {
        bail!("Codex policy denied");
    }
    Ok(())
}

async fn audit(
    db: &jarvis_store::Database,
    device_id: Option<uuid::Uuid>,
    outcome: &str,
    detail: &str,
) {
    let _ = db.query("CREATE security_audit SET id=$id,ts=time::now(),device_id=$device_id,event='codex_broker',outcome=$outcome,detail=$detail RETURN NONE")
        .bind(json!({"id":uuid::Uuid::now_v7().to_string(),"device_id":device_id.map(|id| id.to_string()),"outcome":outcome,"detail":detail})).await;
}
