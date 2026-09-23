//! Root-only, local Unix-socket broker for a deliberately tiny set of Home
//! Node configuration mutations.  It is intentionally separate from Core: a
//! compromised bearer session or Core process cannot turn into root merely by
//! reaching this socket, because every request is independently device-signed,
//! bound to the exact file version and consumed once here.

use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

use jarvis_config::AppConfig;
use jarvis_identity as identity;
use jarvis_llm::ModelAccessPolicy;
use jarvis_privileged::{Operation, SignedRequest};

const SOCKET: &str = "/run/jarvis-config-broker/broker.sock";
const REPLAY_DIR: &str = "/var/lib/jarvis/config-broker/replays";
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Serialize)]
struct Reply {
    status: &'static str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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
    let connections = std::sync::Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            // Do not queue unbounded tasks while slow peers hold connections.
            drop(stream);
            continue;
        };
        let db = db.clone();
        let policy = PathBuf::from(&config.llm_model_policy_path);
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle(stream, &db, &policy).await {
                tracing::warn!(%error, "privileged broker request denied");
            }
        });
    }
}

async fn read_request_frame(
    reader: impl AsyncRead + Unpin,
    timeout: std::time::Duration,
) -> anyhow::Result<Vec<u8>> {
    let mut line = Vec::new();
    // Limit before allocation/read, not after an unbounded read_line.
    let mut reader = BufReader::new(reader.take((MAX_REQUEST_BYTES + 1) as u64));
    tokio::time::timeout(timeout, reader.read_until(b'\n', &mut line))
        .await
        .map_err(|_| anyhow::anyhow!("broker request timed out"))??;
    if line.len() > MAX_REQUEST_BYTES || line.last() != Some(&b'\n') {
        bail!("invalid broker request frame");
    }
    Ok(line)
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
    let line = read_request_frame(read, std::time::Duration::from_secs(5)).await?;
    let envelope: RequestEnvelope =
        serde_json::from_slice(&line).map_err(|_| anyhow::anyhow!("invalid broker request"))?;
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
    // Lock the stable protected directory, not the atomically replaced file.
    // The canonical CLI helper takes the same lock for its read/modify/write.
    use std::os::fd::AsRawFd;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(policy_path.parent().context("policy has no parent")?)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        bail!("unsafe model policy directory");
    }
    if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("another model policy change is in progress");
    }
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
    use std::io::Read;
    // Validate and read the same inode. NONBLOCK avoids hanging on a FIFO.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != 0 || meta.permissions().mode() & 0o022 != 0 {
        bail!("unsafe protected configuration");
    }
    const MAX_POLICY_BYTES: u64 = 8 * 1024 * 1024;
    if meta.len() > MAX_POLICY_BYTES {
        bail!("protected configuration exceeds size limit");
    }
    let mut content = Vec::new();
    file.take(MAX_POLICY_BYTES + 1).read_to_end(&mut content)?;
    if content.len() as u64 > MAX_POLICY_BYTES {
        bail!("protected configuration exceeds size limit");
    }
    Ok(content)
}

fn atomic_root_write(path: &Path, content: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    let previous = fs::symlink_metadata(path)?;
    if !previous.file_type().is_file()
        || previous.uid() != 0
        || previous.permissions().mode() & 0o022 != 0
    {
        bail!("unsafe protected configuration");
    }
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
    let result = (|| -> anyhow::Result<()> {
        use std::io::Write;
        // Preserve the protected file's reader group. A root:root replacement
        // would make mode 0640 unreadable to the unprivileged Core service.
        if unsafe { libc::fchown(file.as_raw_fd(), previous.uid(), previous.gid()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(content)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        // Remove only this call's uniquely created staging file, if present.
        let _ = fs::remove_file(&temp);
    }
    result
}

async fn audit(db: &jarvis_store::Database, request: &SignedRequest, outcome: &str) {
    let _ = db.query("CREATE security_audit SET id = $id, ts = time::now(), device_id = $device_id, event = 'privileged_config', outcome = $outcome, detail = $detail RETURN NONE")
        .bind(json!({"id": uuid::Uuid::now_v7().to_string(), "device_id": request.device_id.to_string(), "outcome": outcome, "detail": request.operation.action()})).await;
}

#[cfg(test)]
mod request_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore = "requires root and mount namespaces in isolated CI; fixture paths only"]
    fn root_policy_namespace_requires_directory_not_file_write_access() {
        use std::process::Command;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        const FIXTURE: &str = "JARVIS_POLICY_NAMESPACE_FIXTURE";
        const TEST: &str =
            "request_tests::root_policy_namespace_requires_directory_not_file_write_access";
        if let Some(path) = std::env::var_os(FIXTURE) {
            let root = std::path::PathBuf::from(path);
            assert_eq!(root.parent(), Some(Path::new("/tmp")));
            assert!(root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("jarvis-policy-namespace-"));
            assert!(!fs::symlink_metadata(&root)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::metadata(&root).unwrap().uid(), 0);
            let mount = |args: &[&std::ffi::OsStr]| {
                assert!(Command::new("mount").args(args).status().unwrap().success());
            };
            let managed = root.join("model-policy");
            let legacy = root.join("model-policy.json");
            // Reproduce ProtectSystem=strict plus a file-only ReadWritePaths.
            mount(&["--bind".as_ref(), root.as_os_str(), root.as_os_str()]);
            mount(&["--bind".as_ref(), legacy.as_os_str(), legacy.as_os_str()]);
            mount(&["--bind".as_ref(), managed.as_os_str(), managed.as_os_str()]);
            mount(&["-o".as_ref(), "remount,bind,ro".as_ref(), root.as_os_str()]);
            assert!(atomic_root_write(&legacy, b"new", 0o640).is_err());
            assert_eq!(fs::read(&legacy).unwrap(), b"old");
            // The dedicated directory mount permits sibling staging+rename,
            // while unrelated protected configuration stays read-only.
            atomic_root_write(&managed.join("policy.json"), b"new", 0o640).unwrap();
            assert_eq!(fs::read(managed.join("policy.json")).unwrap(), b"new\n");
            assert!(fs::write(root.join("core.env"), b"must not be writable").is_err());
            return;
        }
        let root = tempfile::Builder::new()
            .prefix("jarvis-policy-namespace-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::create_dir(root.path().join("model-policy")).unwrap();
        for path in [
            root.path().join("model-policy.json"),
            root.path().join("model-policy/policy.json"),
        ] {
            fs::write(&path, b"old").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        // A subprocess owns the mount namespace: no mounts leak into the
        // parent test runner, even when an assertion fails in the child.
        assert!(Command::new("unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", TEST])
            .env(FIXTURE, root.path())
            .status()
            .unwrap()
            .success());
    }

    #[test]
    #[ignore = "requires root in an isolated CI runner; fixture paths only"]
    fn root_model_toggle_fixture_is_atomic_and_rejects_stale_or_concurrent_writes() {
        use std::os::fd::AsRawFd;
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let directory =
            std::env::temp_dir().join(format!("jarvis-model-toggle-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("policy.json");
        let initial = br#"{"version":1,"models":[{"provider":"ollama-cloud","model":"fixture","enabled":false,"source":"discovered"}]}"#;
        fs::write(&path, initial).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let operation = Operation::ModelSetEnabled {
            provider: "ollama-cloud".into(),
            model: "fixture".into(),
            enabled: true,
            expected_policy_sha256: hex::encode(Sha256::digest(initial)),
        };
        // The CLI and broker lock the same stable directory, not the replaced
        // JSON inode. A competing writer cannot consume an old snapshot.
        let competing = fs::File::open(&directory).unwrap();
        assert_eq!(
            unsafe { libc::flock(competing.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(apply(&operation, &path).is_err());
        assert_eq!(fs::read(&path).unwrap(), initial);
        drop(competing);
        apply(&operation, &path).unwrap();
        let activated = read_protected(&path).unwrap();
        let policy: ModelAccessPolicy = serde_json::from_slice(&activated).unwrap();
        assert!(policy.allows("ollama-cloud", "fixture"));
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert!(apply(&operation, &path).is_err());
        assert_eq!(read_protected(&path).unwrap(), activated);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn atomic_policy_write_preserves_reader_group_and_rejects_links() {
        let directory =
            std::env::temp_dir().join(format!("jarvis-policy-write-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let policy = directory.join("policy.json");
        let link = directory.join("link.json");
        fs::write(&policy, b"original").unwrap();
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&policy, &link).unwrap();
        assert!(atomic_root_write(&link, b"denied", 0o640).is_err());
        assert_eq!(fs::read(&policy).unwrap(), b"original");
        if unsafe { libc::geteuid() } == 0 {
            use std::os::fd::AsRawFd;
            let file = fs::File::open(&policy).unwrap();
            assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 0, 42424) }, 0);
            atomic_root_write(&policy, b"replacement", 0o640).unwrap();
            let metadata = fs::metadata(&policy).unwrap();
            assert_eq!(metadata.uid(), 0);
            assert_eq!(metadata.gid(), 42424);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
            assert_eq!(fs::read(&policy).unwrap(), b"replacement\n");
        } else {
            assert!(atomic_root_write(&policy, b"denied", 0o640).is_err());
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_file(link).unwrap();
        fs::remove_file(policy).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn protected_policy_rejects_links_nonfiles_and_unsafe_permissions() {
        let directory =
            std::env::temp_dir().join(format!("jarvis-policy-read-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let policy = directory.join("policy.json");
        let link = directory.join("link.json");
        fs::write(&policy, b"{}").unwrap();
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&policy, &link).unwrap();
        assert!(read_protected(&link).is_err());
        assert!(read_protected(&directory).is_err());
        // The normal non-root CI runner must not be able to supply a policy.
        if unsafe { libc::geteuid() } != 0 {
            assert!(read_protected(&policy).is_err());
        } else {
            assert_eq!(read_protected(&policy).unwrap(), b"{}");
        }
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_protected(&policy).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_file(policy).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn accepts_one_bounded_frame_only() {
        let frame = read_request_frame(&b"{}\nignored\n"[..], Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(frame, b"{}\n");
        let maximum = [vec![b' '; MAX_REQUEST_BYTES - 1], vec![b'\n']].concat();
        assert_eq!(
            read_request_frame(maximum.as_slice(), Duration::from_secs(1))
                .await
                .unwrap()
                .len(),
            MAX_REQUEST_BYTES
        );
    }

    #[tokio::test]
    async fn rejects_oversize_and_unterminated_frames() {
        for frame in [
            vec![b'x'; MAX_REQUEST_BYTES + 1],
            b"{}".to_vec(),
            Vec::new(),
        ] {
            assert!(read_request_frame(frame.as_slice(), Duration::from_secs(1))
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn slow_peer_is_timed_out_without_waiting_for_eof() {
        let (_peer, reader) = tokio::io::duplex(64);
        let error = read_request_frame(reader, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "broker request timed out");
    }

    #[test]
    fn invalid_envelope_does_not_accept_extra_authority_fields() {
        let request = SignedRequest {
            request_id: uuid::Uuid::nil(),
            nonce_hex: "00".repeat(32),
            user_id: uuid::Uuid::nil(),
            device_id: uuid::Uuid::nil(),
            issued_at: time::OffsetDateTime::UNIX_EPOCH,
            expires_at: time::OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(1),
            operation: Operation::ModelSetEnabled {
                provider: "ollama-cloud".into(),
                model: "fixture-model".into(),
                enabled: false,
                expected_policy_sha256: "00".repeat(32),
            },
            signature_hex: "00".repeat(64),
        };
        let mut envelope = json!({"request": request});
        assert!(serde_json::from_value::<RequestEnvelope>(envelope.clone()).is_ok());
        envelope["approved"] = json!(true);
        assert!(serde_json::from_value::<RequestEnvelope>(envelope).is_err());
    }
}
