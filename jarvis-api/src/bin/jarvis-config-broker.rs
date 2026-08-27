//! Root-only, local Unix-socket broker for a deliberately tiny set of Home
//! Node configuration mutations.  It is intentionally separate from Core: a
//! compromised bearer session or Core process cannot turn into root merely by
//! reaching this socket, because every request is independently device-signed,
//! bound to the exact file version and consumed once here.

use std::{
    fs,
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

use jarvis_config::AppConfig;
use jarvis_identity as identity;
use jarvis_llm::ModelAccessPolicy;
use jarvis_privileged::{Operation, SignedRequest};

const SOCKET: &str = "/run/jarvis-config-broker/broker.sock";
const REPLAY_DIR: &str = "/var/lib/jarvis/config-broker/replays";

#[derive(Serialize)]
struct Reply {
    status: &'static str,
}

#[derive(Deserialize)]
struct RequestEnvelope {
    request: SignedRequest,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("jarvis-config-broker must run as root");
    }
    let socket = std::env::args().nth(1).unwrap_or_else(|| SOCKET.into());
    let config = AppConfig::load().context("load protected broker configuration")?;
    let db = jarvis_store::connect(
        &config.surreal_endpoint,
        &config.surreal_namespace,
        &config.surreal_database,
        &config.surreal_username,
        &config.surreal_password,
    )
    .await?;
    // The broker never applies schema changes: it must fail closed when Core
    // has not established the trusted schema first.
    prepare_socket(&socket)?;
    fs::create_dir_all(REPLAY_DIR)?;
    fs::set_permissions(REPLAY_DIR, fs::Permissions::from_mode(0o700))?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o660))?;
    tracing::info!(socket = %socket, "root configuration broker ready");
    loop {
        let (stream, _) = listener.accept().await?;
        let db = db.clone();
        let policy = PathBuf::from(&config.llm_model_policy_path);
        tokio::spawn(async move {
            if let Err(error) = handle(stream, &db, &policy).await {
                tracing::warn!(%error, "privileged broker request denied");
            }
        });
    }
}

fn prepare_socket(socket: &str) -> anyhow::Result<()> {
    let path = Path::new(socket);
    let parent = path.parent().context("broker socket has no parent")?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            bail!("refusing non-socket broker path");
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

async fn handle(
    stream: UnixStream,
    db: &jarvis_store::Database,
    policy_path: &Path,
) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let read_len = BufReader::new(read).read_line(&mut line).await?;
    if read_len == 0 || read_len > 16 * 1024 {
        bail!("invalid broker request size");
    }
    let envelope: RequestEnvelope =
        serde_json::from_str(&line).context("invalid broker request")?;
    let request = envelope.request;
    let result = async {
        request
            .reject_if_expired(time::OffsetDateTime::now_utc())
            .map_err(|_| anyhow::anyhow!("expired approval"))?;
        let message = request
            .message()
            .map_err(|_| anyhow::anyhow!("invalid approval payload"))?;
        let signature = hex::decode(&request.signature_hex)
            .map_err(|_| anyhow::anyhow!("invalid signature"))?;
        identity::verify_device_signature(
            db,
            request.user_id,
            request.device_id,
            &message,
            &signature,
        )
        .await
        .map_err(|_| anyhow::anyhow!("untrusted owner device signature"))?;
        consume_once(request.request_id)?;
        apply(&request.operation, policy_path)
    }
    .await;
    let outcome = match result.as_ref() {
        Ok(()) => "applied",
        Err(error) if error.to_string().contains("expired") => "expired",
        Err(error) if error.to_string().contains("replay") => "replay_rejected",
        Err(error)
            if error.to_string().contains("signature") || error.to_string().contains("device") =>
        {
            "signature_denied"
        }
        Err(_) => "denied",
    };
    audit(db, &request, outcome).await;
    result?;
    write
        .write_all(serde_json::to_string(&Reply { status: "applied" })?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    Ok(())
}

fn consume_once(request_id: uuid::Uuid) -> anyhow::Result<()> {
    let path = Path::new(REPLAY_DIR).join(request_id.to_string());
    let result = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path);
    result
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("approval replay rejected"))
}

fn apply(operation: &Operation, policy_path: &Path) -> anyhow::Result<()> {
    match operation {
        Operation::ModelSetEnabled {
            provider,
            model,
            enabled,
            expected_policy_sha256,
        } => {
            let raw = read_protected(policy_path)?;
            let hash = hex::encode(Sha256::digest(&raw));
            if &hash != expected_policy_sha256 {
                bail!("policy version changed");
            }
            let mut policy: ModelAccessPolicy =
                serde_json::from_slice(&raw).context("malformed model policy")?;
            policy.validate().map_err(anyhow::Error::msg)?;
            let Some(entry) = policy
                .models
                .iter_mut()
                .find(|entry| entry.provider == *provider && entry.model == *model)
            else {
                bail!("model is not discovered");
            };
            entry.enabled = *enabled;
            let replacement = serde_json::to_vec_pretty(&policy)?;
            atomic_root_write(policy_path, &replacement, 0o640)?;
        }
    }
    Ok(())
}

fn read_protected(path: &Path) -> anyhow::Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file()
        || meta.file_type().is_symlink()
        || meta.permissions().mode() & 0o022 != 0
    {
        bail!("unsafe protected configuration");
    }
    Ok(fs::read(path)?)
}

fn atomic_root_write(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    let dir = path.parent().context("protected path has no parent")?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("config"),
        uuid::Uuid::now_v7()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)?;
    use std::io::Write;
    file.write_all(content)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    fs::rename(&temp, path)?;
    Ok(())
}

async fn audit(db: &jarvis_store::Database, request: &SignedRequest, outcome: &str) {
    let _ = db.query("CREATE security_audit SET id = $id, ts = time::now(), device_id = $device_id, event = 'privileged_config', outcome = $outcome, detail = $detail RETURN NONE")
        .bind(json!({"id": uuid::Uuid::now_v7().to_string(), "device_id": request.device_id.to_string(), "outcome": outcome, "detail": request.operation.action()})).await;
}
