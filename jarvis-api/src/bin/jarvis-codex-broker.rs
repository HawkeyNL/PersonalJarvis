//! Local-only broker for the finite Codex/OpenSandbox protocol.
//!
//! This process deliberately has no HTTP listener, shell endpoint, host-path
//! argument, environment mutation or generic process-spawn API.  It validates
//! a device-signed request independently of Core.  Activation remains gated on
//! a task-scoped Codex credential provider; until then valid requests are
//! audited and denied rather than falling back to a host Codex process.

use std::{
    collections::HashMap,
    ffi::CString,
    fs,
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{watch, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
};

use jarvis_codex::{
    relay::{CodexSubscriptionAdapter, UnavailableSubscriptionAdapter},
    snapshot::TrustedRepositoryRegistry,
    ApprovedSandboxRun, CodingOperation, CodingRunError, RunCapabilityAuthority,
    RunCapabilityClaims, SandboxOwnershipRecorder, SignedCodingRequest, TaskContextInput,
};
use jarvis_config::AppConfig;
use jarvis_identity as identity;
use jarvis_sandbox::{OpenSandboxProvider, SandboxAvailability, SandboxHandle, SandboxProvider};
use jarvis_usage::coding_reservations;

const DEFAULT_SOCKET: &str = "/run/jarvis-codex-broker/broker.sock";
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const ARTIFACTS_ROOT: &str = "/var/lib/jarvis-codex-broker/artifacts";
const MAX_STORED_RUNS: usize = 1_000;
const MAX_STORED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Serialize)]
struct Reply {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<uuid::Uuid>,
}

struct BrokerState {
    db: jarvis_store::Database,
    core_uid: u32,
    sandbox: Option<Arc<dyn SandboxProvider>>,
    adapter: Arc<dyn CodexSubscriptionAdapter>,
    artifacts_root: PathBuf,
    authority: Arc<RunCapabilityAuthority>,
    active: Arc<Mutex<HashMap<uuid::Uuid, watch::Sender<bool>>>>,
    run_slots: Arc<Semaphore>,
    cleanup_healthy: Arc<AtomicBool>,
    cleanup_epoch: Arc<AtomicU64>,
    reconcile_lock: Arc<AsyncMutex<()>>,
}

struct RunJob {
    signed: SignedCodingRequest,
    snapshot: jarvis_codex::RepositorySnapshot,
    context: TaskContextInput,
    run_id: uuid::Uuid,
    capability: jarvis_codex::RunCapabilityToken,
    receiver: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
}

/// A broker task can unwind or be aborted without reaching its normal
/// epilogue. In that case it must stop hiding its sandbox from reconciliation
/// and close admission until the manager metadata scan proves cleanup.
struct ActiveRunGuard {
    active: Arc<Mutex<HashMap<uuid::Uuid, watch::Sender<bool>>>>,
    cleanup_healthy: Arc<AtomicBool>,
    cleanup_epoch: Arc<AtomicU64>,
    run_id: uuid::Uuid,
}

impl Drop for ActiveRunGuard {
    fn drop(&mut self) {
        let uncertain = match self.active.lock() {
            Ok(mut runs) => runs.remove(&self.run_id).is_some(),
            Err(_) => true,
        };
        if uncertain {
            self.cleanup_epoch.fetch_add(1, Ordering::SeqCst);
            self.cleanup_healthy.store(false, Ordering::SeqCst);
        }
    }
}

struct DurableSandboxOwnership<'a> {
    db: &'a jarvis_store::Database,
    run_id: uuid::Uuid,
}

#[async_trait::async_trait]
impl SandboxOwnershipRecorder for DurableSandboxOwnership<'_> {
    async fn created(&self, handle: &SandboxHandle) -> Result<(), CodingRunError> {
        let mut response = self.db.query("UPDATE coding_runs SET sandbox_provider='opensandbox',sandbox_id=$sandbox,sandbox_state='active',sandbox_created_at=time::now(),sandbox_cleanup_status=NONE,status='running',updated_at=time::now() WHERE record::id(id)=$run AND status='preparing' AND sandbox_id=NONE RETURN record::id(id) AS id")
            .bind(json!({"run":self.run_id.to_string(),"sandbox":handle.provider_id}))
            .await.map_err(|_|CodingRunError::SandboxFailed)?;
        let rows: Vec<serde_json::Value> = response
            .take(0)
            .map_err(|_| CodingRunError::SandboxFailed)?;
        if rows.len() != 1 {
            return Err(CodingRunError::SandboxFailed);
        }
        Ok(())
    }

    async fn cleanup(
        &self,
        handle: &SandboxHandle,
        terminated: bool,
    ) -> Result<(), CodingRunError> {
        let state = if terminated {
            "terminated"
        } else {
            "cleanup_failed"
        };
        let mut response = self.db.query("UPDATE coding_runs SET sandbox_state=$state,sandbox_cleanup_status=$state,updated_at=time::now() WHERE record::id(id)=$run AND sandbox_id=$sandbox RETURN record::id(id) AS id")
            .bind(json!({"run":self.run_id.to_string(),"sandbox":handle.provider_id,"state":state}))
            .await.map_err(|_|CodingRunError::SandboxFailed)?;
        let rows: Vec<serde_json::Value> = response
            .take(0)
            .map_err(|_| CodingRunError::SandboxFailed)?;
        if rows.len() != 1 {
            return Err(CodingRunError::SandboxFailed);
        }
        Ok(())
    }
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
    let state = Arc::new(BrokerState {
        db,
        core_uid: lookup_core_uid()?,
        sandbox: configured_sandbox(|name| std::env::var(name).ok())?,
        adapter: Arc::new(UnavailableSubscriptionAdapter),
        artifacts_root: PathBuf::from(ARTIFACTS_ROOT),
        authority: Arc::new(RunCapabilityAuthority::default()),
        active: Arc::new(Mutex::new(HashMap::new())),
        run_slots: Arc::new(Semaphore::new(2)),
        cleanup_healthy: Arc::new(AtomicBool::new(false)),
        cleanup_epoch: Arc::new(AtomicU64::new(0)),
        reconcile_lock: Arc::new(AsyncMutex::new(())),
    });
    reconcile_owned_sandboxes(&state).await?;
    state.cleanup_healthy.store(true, Ordering::SeqCst);
    let maintenance = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await;
        loop {
            interval.tick().await;
            maintenance.cleanup_healthy.store(false, Ordering::SeqCst);
            let observed_epoch = maintenance.cleanup_epoch.load(Ordering::SeqCst);
            let result = reconcile_owned_sandboxes(&maintenance).await;
            if let Err(error) = result {
                tracing::error!(%error, "Codex owned sandbox reconciliation requires recovery");
            } else if maintenance.cleanup_epoch.load(Ordering::SeqCst) == observed_epoch {
                maintenance.cleanup_healthy.store(true, Ordering::SeqCst);
            }
        }
    });
    prepare_socket(socket)?;
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
    tracing::info!(
        socket,
        "Codex broker ready; scoped credential gate remains enforced"
    );
    let slots = Arc::new(Semaphore::new(4));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            // Overload cannot create unbounded signed-request/snapshot tasks.
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle(stream, &state).await {
                tracing::warn!(%error, "Codex broker request denied");
            }
        });
    }
}

fn lookup_core_uid() -> anyhow::Result<u32> {
    let name = CString::new("jarvis")?;
    // Resolved once before request handling: peer UID, not a user-supplied
    // field or socket group, gates status/cancel control.
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        bail!("Core service identity missing");
    }
    Ok(unsafe { (*entry).pw_uid })
}

fn configured_sandbox(
    env: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<Arc<dyn SandboxProvider>>> {
    if env("JARVIS_CODEX_EXECUTION_ENABLED").as_deref() != Some("1") {
        return Ok(None);
    }
    let endpoint =
        env("JARVIS_CODEX_OPENSANDBOX_ENDPOINT").context("OpenSandbox endpoint missing")?;
    let key = env("JARVIS_CODEX_OPENSANDBOX_API_KEY").context("OpenSandbox API key missing")?;
    let digest = env("JARVIS_CODEX_WORKLOAD_IMAGE").context("Codex workload image missing")?;
    Ok(Some(Arc::new(OpenSandboxProvider::for_codex_broker(
        endpoint, key, digest,
    )?)))
}

async fn reconcile_owned_sandboxes(state: &BrokerState) -> anyhow::Result<()> {
    let _reconcile_guard = state.reconcile_lock.lock().await;
    // Without the manager there is no proof that an old workload is gone.
    // Execution is already disabled in this state, so do not release a lease.
    let Some(manager) = state.sandbox.as_ref() else {
        return Ok(());
    };
    let owned = manager.list_owned_codex().await?;
    let active = state
        .active
        .lock()
        .map_err(|_| anyhow::anyhow!("run registry unavailable"))?
        .keys()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for item in &owned {
        if active.contains(&item.handle.task_id) {
            continue;
        }
        // The authenticated manager metadata is the second ownership key for
        // the crash window before sandbox_id could be persisted.
        manager.terminate(item.handle.clone()).await?;
        let _ = state.db.query("UPDATE coding_runs SET sandbox_provider='opensandbox',sandbox_id=$sandbox,sandbox_state='orphan_recovered',sandbox_cleanup_status='terminated',updated_at=time::now() WHERE record::id(id)=$run AND coding_session_id=$coding_session_key AND (sandbox_id=NONE OR sandbox_id=$sandbox) RETURN NONE")
            .bind(json!({"run":item.handle.task_id.to_string(),"coding_session_key":item.coding_session_id.to_string(),"sandbox":item.handle.provider_id})).await?.check()?;
        audit(
            &state.db,
            None,
            "orphan_cleanup",
            "Jarvis Codex sandbox terminated",
        )
        .await;
    }
    // Never claim cleanup while a manager-owned workload is still listed.
    let remaining = manager.list_owned_codex().await?;
    if remaining
        .iter()
        .any(|item| !active.contains(&item.handle.task_id))
    {
        bail!("Jarvis Codex orphan sandbox cleanup incomplete");
    }
    let mut response = state.db.query("SELECT record::id(id) AS run_id,reservation_id,user_id,status,sandbox_id,sandbox_state FROM coding_runs WHERE status IN ['queued','preparing','running','cancelling'] OR sandbox_state IN ['active','termination_requested','cleanup_failed'] LIMIT 1000")
        .await?.check()?;
    let rows: Vec<serde_json::Value> = response.take(0)?;
    if rows.len() == 1000 {
        bail!("Codex recovery scan exceeded bound");
    }
    for row in rows {
        let run_id = row
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .context("invalid durable run ID")?;
        if active.contains(&run_id) {
            continue;
        }
        let reservation_id = row
            .get("reservation_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .context("invalid durable reservation ID")?;
        let status = row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .context("invalid durable run state")?;
        // Metadata discovery alone is not proof for a workload whose run row
        // still names it: delete it by ID (idempotent) before recording it.
        if let (Some(sandbox_id), Some("active" | "termination_requested" | "cleanup_failed")) = (
            row.get("sandbox_id").and_then(serde_json::Value::as_str),
            row.get("sandbox_state").and_then(serde_json::Value::as_str),
        ) {
            manager
                .terminate(SandboxHandle {
                    provider_id: sandbox_id.to_owned(),
                    task_id: run_id,
                    profile: jarvis_sandbox::SandboxProfile::Codex,
                })
                .await?;
        }
        let _ =
            coding_reservations::finish(&state.db, reservation_id, run_id, status == "completed")
                .await?;
        if matches!(status, "queued" | "preparing" | "running" | "cancelling") {
            state.db.query("UPDATE coding_runs SET status='failed',failure_category='broker_restarted',reservation_status='released',sandbox_state='terminated',sandbox_cleanup_status='terminated',updated_at=time::now(),completed_at=time::now() WHERE record::id(id)=$run AND status IN ['queued','preparing','running','cancelling'] RETURN NONE")
                .bind(json!({"run":run_id.to_string()})).await?.check()?;
        } else {
            // A terminal run whose cleanup record never landed: the manager
            // scan above proved its workload is gone.
            state.db.query("UPDATE coding_runs SET sandbox_state='terminated',sandbox_cleanup_status='terminated',reservation_status='released',updated_at=time::now() WHERE record::id(id)=$run AND sandbox_state IN ['active','termination_requested','cleanup_failed'] RETURN NONE")
                .bind(json!({"run":run_id.to_string()})).await?.check()?;
        }
    }
    // A broker can die after the conditional lease but before CREATE
    // coding_runs. Such a lease cannot be revived or spent again; release it
    // only after the manager metadata scan proved no unowned workload remains.
    let mut leases = state.db.query("SELECT record::id(id) AS reservation_id,run_id FROM coding_reservations WHERE status='leased' LIMIT 1000")
        .await?.check()?;
    let leases: Vec<serde_json::Value> = leases.take(0)?;
    if leases.len() == 1000 {
        bail!("Codex reservation recovery scan exceeded bound");
    }
    for lease in leases {
        let reservation_id = lease
            .get("reservation_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .context("invalid leased reservation ID")?;
        let run_id = lease
            .get("run_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .context("invalid leased run ID")?;
        if active.contains(&run_id) {
            continue;
        }
        let mut present = state
            .db
            .query("SELECT status,sandbox_state FROM coding_runs WHERE record::id(id)=$run LIMIT 1")
            .bind(json!({"run":run_id.to_string()}))
            .await?
            .check()?;
        let rows: Vec<serde_json::Value> = present.take(0)?;
        if rows.is_empty() {
            let _ = coding_reservations::finish(&state.db, reservation_id, run_id, false).await?;
            audit(
                &state.db,
                None,
                "orphan_reservation",
                "unattached subscription lease released",
            )
            .await;
        } else if let Some(row) = rows.first() {
            let status = row
                .get("status")
                .and_then(serde_json::Value::as_str)
                .context("invalid leased run state")?;
            let sandbox_state = row.get("sandbox_state").and_then(serde_json::Value::as_str);
            if matches!(status, "completed" | "failed" | "timed_out" | "cancelled")
                && matches!(
                    sandbox_state,
                    Some("terminated" | "orphan_recovered" | "not_created")
                )
            {
                let _ = coding_reservations::finish(
                    &state.db,
                    reservation_id,
                    run_id,
                    status == "completed",
                )
                .await?;
            }
        }
    }
    Ok(())
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

async fn handle(stream: UnixStream, state: &Arc<BrokerState>) -> anyhow::Result<()> {
    if stream.peer_cred()?.uid() != state.core_uid {
        bail!("untrusted Codex broker peer");
    }
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let length = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        BufReader::new(read.take((MAX_REQUEST_BYTES + 1) as u64)).read_line(&mut line),
    )
    .await??;
    if length == 0 || length > MAX_REQUEST_BYTES || !line.ends_with('\n') {
        bail!("invalid Codex broker request size");
    }
    let request: jarvis_codex::BrokerRequest =
        serde_json::from_str(&line).context("invalid Codex broker request")?;
    request
        .validate_shape()
        .map_err(|_| anyhow::anyhow!("invalid Codex operation"))?;
    if let jarvis_codex::BrokerRequest::CancelCodingRun { run_id, user_id } = request {
        cancel_run(state, run_id, user_id).await?;
        return write_reply(
            &mut write,
            Reply {
                status: "cancelling",
                run_id: Some(run_id),
            },
        )
        .await;
    }
    if let jarvis_codex::BrokerRequest::GetCodingRunStatus { run_id, user_id } = request {
        let status = run_status(&state.db, run_id, user_id).await?;
        let bytes = serde_json::to_vec(&json!({"status":status,"run_id":run_id}))?;
        write.write_all(&bytes).await?;
        write.write_all(b"\n").await?;
        return Ok(());
    }
    if let jarvis_codex::BrokerRequest::GetCodingArtifact {
        run_id,
        user_id,
        artifact,
    } = request
    {
        let content = read_artifact(state, run_id, user_id, artifact).await?;
        let bytes = serde_json::to_vec(
            &json!({"status":"artifact","name":artifact.file_name(),"content":content}),
        )?;
        if bytes.len() > 1_100_000 {
            bail!("artifact reply exceeds bound");
        }
        write.write_all(&bytes).await?;
        write.write_all(b"\n").await?;
        return Ok(());
    }
    let signed = request
        .signed_request()
        .context("signed operation required")?;
    let outcome = validate_signature(&state.db, signed).await;
    if let Err(error) = outcome {
        audit(
            &state.db,
            Some(signed.device_id),
            "denied",
            "signature or expiry",
        )
        .await;
        return Err(error);
    }
    // Resolve the signed logical repository and exact commit through the
    // root-managed registry before the subscription adapter gate. Even a
    // currently unavailable adapter must not become a path to arbitrary host
    // snapshots when an official provider-only interface becomes available.
    let result = start_run(state, signed).await;
    match result {
        Ok(run_id) => {
            write_reply(
                &mut write,
                Reply {
                    status: "accepted",
                    run_id: Some(run_id),
                },
            )
            .await
        }
        Err(error) => {
            audit(
                &state.db,
                Some(signed.device_id),
                "denied",
                "runtime gate or preparation",
            )
            .await;
            write_reply(
                &mut write,
                Reply {
                    status: "denied",
                    run_id: None,
                },
            )
            .await?;
            Err(error)
        }
    }
}

async fn read_artifact(
    state: &BrokerState,
    run_id: uuid::Uuid,
    user_id: uuid::Uuid,
    artifact: jarvis_codex::CodingArtifactName,
) -> anyhow::Result<String> {
    if run_status(&state.db, run_id, user_id).await? != "completed" {
        bail!("coding run has no completed artifact");
    }
    let path = state
        .artifacts_root
        .join(run_id.to_string())
        .join(artifact.file_name());
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    let size = metadata.len();
    if !metadata.file_type().is_file() || size > 512 * 1024 {
        bail!("coding artifact exceeds bound");
    }
    let mut content = String::new();
    file.take(512 * 1024 + 1).read_to_string(&mut content)?;
    if content.len() > 512 * 1024 {
        bail!("coding artifact exceeds bound");
    }
    Ok(content)
}

async fn write_reply<W: tokio::io::AsyncWrite + Unpin>(
    write: &mut W,
    reply: Reply,
) -> anyhow::Result<()> {
    write.write_all(&serde_json::to_vec(&reply)?).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

async fn start_run(
    state: &Arc<BrokerState>,
    signed: &SignedCodingRequest,
) -> anyhow::Result<uuid::Uuid> {
    let reconcile_guard = state.reconcile_lock.lock().await;
    if !state.cleanup_healthy.load(Ordering::SeqCst) {
        bail!("Codex sandbox cleanup requires recovery");
    }
    if state.sandbox.is_none() {
        bail!("Codex execution not owner-enabled");
    }
    if !state.adapter.available() {
        bail!("reviewed subscription provider-only interface unavailable");
    }
    let slot = state
        .run_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Codex run capacity reached"))?;
    let (snapshot, context) = prepare_signed_run(&state.db, signed).await?;
    let (run_id, _worker) =
        admit_prepared_run(state, &reconcile_guard, signed, slot, snapshot, context).await?;
    Ok(run_id)
}

/// Everything after the registry snapshot: manager availability, the single
/// reservation lease, the durable run row and the worker. Requiring the
/// reconcile guard keeps admission and orphan recovery mutually exclusive.
async fn admit_prepared_run(
    state: &Arc<BrokerState>,
    _reconcile: &tokio::sync::MutexGuard<'_, ()>,
    signed: &SignedCodingRequest,
    slot: OwnedSemaphorePermit,
    snapshot: jarvis_codex::RepositorySnapshot,
    context: TaskContextInput,
) -> anyhow::Result<(uuid::Uuid, tokio::task::JoinHandle<()>)> {
    let sandbox = state
        .sandbox
        .as_ref()
        .context("Codex execution not owner-enabled")?;
    if !state.adapter.available() {
        bail!("reviewed subscription provider-only interface unavailable");
    }
    if sandbox.availability().await != SandboxAvailability::Available {
        bail!("OpenSandbox unavailable");
    }
    let run_id = uuid::Uuid::now_v7();
    let claims = RunCapabilityClaims::from_signed_request(run_id, signed, 1)?;
    let requested_runtime_secs = match &signed.operation {
        CodingOperation::StartCodingRun { timeout_secs, .. }
        | CodingOperation::ResumeCodingRun { timeout_secs, .. } => *timeout_secs,
    };
    let lease = coding_reservations::lease(
        &state.db,
        claims.budget_reservation_id,
        signed.user_id,
        claims.coding_session_id,
        run_id,
        requested_runtime_secs,
    )
    .await?
    .context("server-issued subscription reservation missing, expired or already leased")?;
    if lease.max_provider_turns == 0 || lease.max_runtime_secs < requested_runtime_secs as u32 {
        coding_reservations::finish(&state.db, lease.id, run_id, false).await?;
        bail!("subscription reservation limits do not authorize this run");
    }
    let snapshot_sha = snapshot.sha256_hex();
    let created = state.db.query("CREATE coding_runs SET id=$run,request_id=$request,coding_session_id=$coding_session_key,user_id=$user,device_id=$device,repository_id=$repository_id,repository_owner=$repository_owner,repository_name=$repository_name,base_sha=$base,snapshot_sha256=$snapshot,reservation_id=$reservation,reservation_units=1,reservation_status='active',status='queued',summary=NONE,failure_category=NONE,artifacts=[],compute_class='subscription',sandbox_provider=NONE,sandbox_id=NONE,sandbox_state='not_created',sandbox_created_at=NONE,sandbox_cleanup_status=NONE,created_at=time::now(),updated_at=time::now(),completed_at=NONE RETURN NONE")
        .bind(json!({
            "run":run_id.to_string(),"request":signed.request_id.to_string(),
            "coding_session_key":claims.coding_session_id.to_string(),"user":signed.user_id.to_string(),
            "device":signed.device_id.to_string(),"repository_id":claims.repository.id,
            "repository_owner":claims.repository.owner,"repository_name":claims.repository.name,
            "base":claims.base_commit_sha,"snapshot":snapshot_sha,
            "reservation":claims.budget_reservation_id.to_string(),
        })).await.map_err(anyhow::Error::from).and_then(|response| response.check().map_err(anyhow::Error::from));
    if let Err(error) = created {
        coding_reservations::finish(&state.db, lease.id, run_id, false).await?;
        return Err(error);
    }
    let capability = match state.authority.mint(claims) {
        Ok(token) => token,
        Err(error) => {
            mark_run_finished(
                &state.db,
                run_id,
                "failed",
                None,
                Some("capability_unavailable"),
                &[],
            )
            .await?;
            coding_reservations::finish(&state.db, lease.id, run_id, false).await?;
            return Err(error.into());
        }
    };
    let (sender, receiver) = watch::channel(false);
    state
        .active
        .lock()
        .map_err(|_| anyhow::anyhow!("run registry unavailable"))?
        .insert(run_id, sender);
    let state = state.clone();
    let job = RunJob {
        signed: signed.clone(),
        snapshot,
        context,
        run_id,
        capability,
        receiver,
        _slot: slot,
    };
    let worker = tokio::spawn(async move {
        run_worker(state, job).await;
    });
    Ok((run_id, worker))
}

async fn run_worker(state: Arc<BrokerState>, job: RunJob) {
    let RunJob {
        signed,
        snapshot,
        context,
        run_id,
        capability,
        receiver,
        _slot,
    } = job;
    let _active_guard = ActiveRunGuard {
        active: state.active.clone(),
        cleanup_healthy: state.cleanup_healthy.clone(),
        cleanup_epoch: state.cleanup_epoch.clone(),
        run_id,
    };
    let started = state.db.query("UPDATE coding_runs SET status='preparing',updated_at=time::now() WHERE record::id(id)=$run AND status='queued' RETURN record::id(id) AS id")
        .bind(json!({"run":run_id.to_string()})).await;
    let started = started
        .ok()
        .and_then(|mut result| result.take::<Vec<serde_json::Value>>(0).ok())
        .is_some_and(|rows| !rows.is_empty());
    if !started {
        state.authority.revoke_run(run_id, false);
        let cancelled = *receiver.borrow();
        let _ = mark_run_finished(
            &state.db,
            run_id,
            if cancelled { "cancelled" } else { "failed" },
            None,
            Some(if cancelled {
                "owner_cancelled"
            } else {
                "state_transition"
            }),
            &[],
        )
        .await;
        let reservation_id = match &signed.operation {
            CodingOperation::StartCodingRun {
                budget_reservation_id,
                ..
            }
            | CodingOperation::ResumeCodingRun {
                budget_reservation_id,
                ..
            } => *budget_reservation_id,
        };
        let _ = coding_reservations::finish(&state.db, reservation_id, run_id, false).await;
        state
            .active
            .lock()
            .ok()
            .map(|mut runs| runs.remove(&run_id));
        return;
    }
    let ownership = DurableSandboxOwnership {
        db: &state.db,
        run_id,
    };
    let result = jarvis_codex::execute_in_sandbox_tracked(
        state
            .sandbox
            .as_ref()
            .expect("run started with configured sandbox")
            .as_ref(),
        &signed,
        snapshot,
        context,
        ApprovedSandboxRun {
            adapter: state.adapter.as_ref(),
            authority: &state.authority,
            run_id,
            capability: Some(capability),
        },
        Some(receiver.clone()),
        &ownership,
    )
    .await;
    if matches!(&result, Err(CodingRunError::CleanupRequired)) {
        state.cleanup_epoch.fetch_add(1, Ordering::SeqCst);
        state.cleanup_healthy.store(false, Ordering::SeqCst);
        state.authority.revoke_run(run_id, false);
        let _ = state.db.query("UPDATE coding_runs SET failure_category='cleanup_required',updated_at=time::now() WHERE record::id(id)=$run AND status IN ['preparing','running','cancelling'] RETURN NONE")
            .bind(json!({"run":run_id.to_string()})).await;
        state
            .active
            .lock()
            .ok()
            .map(|mut runs| runs.remove(&run_id));
        return;
    }
    let cancelled = *receiver.borrow();
    match result {
        Ok(mut result) if !cancelled && result.status == "completed" => {
            let summary = result
                .stdout_summary
                .chars()
                .take(8_000)
                .collect::<String>();
            let saved = save_artifacts_in(
                &state.artifacts_root,
                run_id,
                &result.take_artifact_contents(),
            );
            if saved.is_err() {
                let _ = mark_run_finished(
                    &state.db,
                    run_id,
                    "failed",
                    None,
                    Some("artifact_persistence"),
                    &[],
                )
                .await;
                state.authority.revoke_run(run_id, false);
                let reservation_id = match &signed.operation {
                    CodingOperation::StartCodingRun {
                        budget_reservation_id,
                        ..
                    }
                    | CodingOperation::ResumeCodingRun {
                        budget_reservation_id,
                        ..
                    } => *budget_reservation_id,
                };
                let _ = coding_reservations::finish(&state.db, reservation_id, run_id, false).await;
                state
                    .active
                    .lock()
                    .ok()
                    .map(|mut runs| runs.remove(&run_id));
                return;
            }
            let checkpoint = jarvis_codex::CodingCheckpoint {
                summary: summary.clone(),
                decisions: Vec::new(),
                pending: Vec::new(),
                tests: String::new(),
            };
            // Never expose a completed run/artifact before its factual resume
            // checkpoint has been stored. A concurrent cancellation may still
            // win the final transition; its run remains cancelled.
            let checkpoint_saved = state.db.query("UPDATE coding_sessions SET checkpoint=$checkpoint,state='suspended',updated_at=time::now() WHERE record::id(id)=$coding_session_key AND user_id=$user RETURN record::id(id) AS id")
                .bind(json!({"checkpoint":checkpoint,"coding_session_key":result.coding_session_id.to_string(),"user":signed.user_id.to_string()}))
                .await.ok().and_then(|mut response| response.take::<Vec<serde_json::Value>>(0).ok())
                .is_some_and(|rows| !rows.is_empty());
            let completed = if checkpoint_saved {
                mark_run_finished(
                    &state.db,
                    run_id,
                    "completed",
                    Some(&summary),
                    None,
                    &result.artifacts,
                )
                .await
                .unwrap_or(false)
            } else {
                let _ = mark_run_finished(
                    &state.db,
                    run_id,
                    "failed",
                    None,
                    Some("checkpoint_persistence"),
                    &[],
                )
                .await;
                false
            };
            if !completed {
                discard_artifacts(&state.artifacts_root, run_id);
            }
        }
        _ if cancelled => {
            let _ = mark_run_finished(
                &state.db,
                run_id,
                "cancelled",
                None,
                Some("owner_cancelled"),
                &[],
            )
            .await;
        }
        Ok(result) if result.status == "plan_limit" => {
            let _ =
                mark_run_finished(&state.db, run_id, "failed", None, Some("plan_limit"), &[]).await;
        }
        Err(jarvis_codex::CodingRunError::TimedOut) => {
            let _ = mark_run_finished(
                &state.db,
                run_id,
                "timed_out",
                None,
                Some("runtime_timeout"),
                &[],
            )
            .await;
        }
        Ok(_) | Err(_) => {
            let _ = mark_run_finished(
                &state.db,
                run_id,
                "failed",
                None,
                Some("runtime_failure"),
                &[],
            )
            .await;
        }
    }
    let _ = state.db.query("UPDATE coding_runs SET status='cancelled',failure_category='owner_cancelled',reservation_status='released',updated_at=time::now(),completed_at=time::now() WHERE record::id(id)=$run AND status='cancelling' RETURN NONE")
        .bind(json!({"run":run_id.to_string()})).await;
    state.authority.revoke_run(run_id, false);
    let reservation_id = match &signed.operation {
        CodingOperation::StartCodingRun {
            budget_reservation_id,
            ..
        }
        | CodingOperation::ResumeCodingRun {
            budget_reservation_id,
            ..
        } => *budget_reservation_id,
    };
    let final_status = run_status(&state.db, run_id, signed.user_id).await;
    let settled = matches!(final_status.as_deref(), Ok("completed"));
    if matches!(
        coding_reservations::finish(&state.db, reservation_id, run_id, settled).await,
        Ok(true)
    ) {
        let _ = state.db.query("UPDATE coding_runs SET reservation_status='released' WHERE record::id(id)=$run AND reservation_status='active' RETURN NONE")
            .bind(json!({"run":run_id.to_string()})).await;
    }
    state
        .active
        .lock()
        .ok()
        .map(|mut runs| runs.remove(&run_id));
    audit(
        &state.db,
        Some(signed.device_id),
        "finished",
        "bounded coding run",
    )
    .await;
}

fn discard_artifacts(root: &Path, run_id: uuid::Uuid) {
    let dir = root.join(run_id.to_string());
    for name in ["result.json", "patch.diff"] {
        let _ = fs::remove_file(dir.join(name));
    }
    let _ = fs::remove_dir(dir);
}

fn save_artifacts_in(
    root: &Path,
    run_id: uuid::Uuid,
    artifacts: &[jarvis_sandbox::CollectedArtifact],
) -> anyhow::Result<()> {
    if artifacts.len() != 2
        || !artifacts.iter().any(|a| a.path == "result.json")
        || !artifacts.iter().any(|a| a.path == "patch.diff")
        || artifacts
            .iter()
            .any(|a| !matches!(a.path.as_str(), "result.json" | "patch.diff"))
        || artifacts.iter().map(|a| a.contents.len()).sum::<usize>() > 640 * 1024
    {
        bail!("untrusted artifact set");
    }
    if !root.exists() {
        fs::create_dir(root)?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.file_type().is_dir()
        || metadata.permissions().mode() & 0o077 != 0
        || std::os::unix::fs::MetadataExt::uid(&metadata) != unsafe { libc::geteuid() }
    {
        bail!("unsafe Codex artifact store");
    }
    let mut stored_runs = 0usize;
    let mut stored_bytes = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let meta = fs::symlink_metadata(entry.path())?;
        if !meta.file_type().is_dir()
            || uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
        {
            bail!("unsafe Codex artifact store entry");
        }
        stored_runs += 1;
        if stored_runs >= MAX_STORED_RUNS {
            bail!("Codex artifact retention limit reached");
        }
        for artifact in fs::read_dir(entry.path())? {
            let artifact = artifact?;
            let name = artifact.file_name();
            if !matches!(name.to_str(), Some("result.json" | "patch.diff")) {
                bail!("unsafe stored artifact");
            }
            let meta = fs::symlink_metadata(artifact.path())?;
            if !meta.file_type().is_file() {
                bail!("unsafe stored artifact");
            }
            stored_bytes = stored_bytes.saturating_add(meta.len());
            if stored_bytes > MAX_STORED_BYTES {
                bail!("Codex artifact retention limit reached");
            }
        }
    }
    if stored_bytes.saturating_add(
        artifacts
            .iter()
            .map(|a| a.contents.len() as u64)
            .sum::<u64>(),
    ) > MAX_STORED_BYTES
    {
        bail!("Codex artifact retention limit reached");
    }
    let run_dir = root.join(run_id.to_string());
    fs::create_dir(&run_dir)?;
    fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))?;
    let saved = (|| -> anyhow::Result<()> {
        for artifact in artifacts {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(run_dir.join(&artifact.path))?;
            file.write_all(&artifact.contents)?;
            file.sync_all()?;
        }
        Ok(())
    })();
    if saved.is_err() {
        for name in ["result.json", "patch.diff"] {
            let _ = fs::remove_file(run_dir.join(name));
        }
        let _ = fs::remove_dir(&run_dir);
    }
    saved
}

async fn mark_run_finished(
    db: &jarvis_store::Database,
    run_id: uuid::Uuid,
    status: &str,
    summary: Option<&str>,
    failure: Option<&str>,
    artifacts: &[String],
) -> anyhow::Result<bool> {
    let mut response = db.query("UPDATE coding_runs SET status=$status,summary=$summary,failure_category=$failure,artifacts=$artifacts,updated_at=time::now(),completed_at=time::now() WHERE record::id(id)=$run AND status IN ['queued','preparing','running','cancelling'] AND ($status!='completed' OR status='running') AND ($status='cancelled' OR status!='cancelling') RETURN record::id(id) AS id")
        .bind(json!({"status":status,"summary":summary,"failure":failure,"artifacts":artifacts,"run":run_id.to_string()}))
        .await?.check()?;
    let rows: Vec<serde_json::Value> = response.take(0)?;
    Ok(!rows.is_empty())
}

async fn run_status(
    db: &jarvis_store::Database,
    run_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> anyhow::Result<String> {
    let mut response = db
        .query("SELECT status FROM coding_runs WHERE record::id(id)=$run AND user_id=$user LIMIT 1")
        .bind(json!({"run":run_id.to_string(),"user":user_id.to_string()}))
        .await?;
    let rows: Vec<serde_json::Value> = response.take(0)?;
    rows.into_iter()
        .next()
        .and_then(|row| {
            row.get("status")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .context("coding run does not belong to owner")
}

async fn cancel_run(
    state: &BrokerState,
    run_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let status = run_status(&state.db, run_id, user_id).await?;
    if !matches!(
        status.as_str(),
        "queued" | "preparing" | "running" | "cancelling"
    ) {
        bail!("coding run is not active");
    }
    let sender = state
        .active
        .lock()
        .map_err(|_| anyhow::anyhow!("run registry unavailable"))?
        .get(&run_id)
        .cloned()
        .context("run no longer active")?;
    let mut result = state.db.query("UPDATE coding_runs SET status='cancelling',updated_at=time::now() WHERE record::id(id)=$run AND user_id=$user AND status IN ['queued','preparing','running'] RETURN record::id(id) AS id")
        .bind(json!({"run":run_id.to_string(),"user":user_id.to_string()})).await?;
    let changed: Vec<serde_json::Value> = result.take(0)?;
    if changed.is_empty() && status != "cancelling" {
        bail!("coding run cannot be cancelled");
    }
    state.authority.revoke_run(run_id, false);
    let _ = sender.send(true);
    Ok(())
}

async fn prepare_signed_run(
    db: &jarvis_store::Database,
    signed: &jarvis_codex::SignedCodingRequest,
) -> anyhow::Result<(jarvis_codex::RepositorySnapshot, TaskContextInput)> {
    let (session_id, repository, base_sha, start_objective, checkpoint, owner_delta) =
        match &signed.operation {
            CodingOperation::StartCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                objective,
                checkpoint,
                ..
            } => (
                coding_session_id,
                repository,
                base_commit_sha,
                Some(objective.as_str()),
                checkpoint.as_ref(),
                None,
            ),
            CodingOperation::ResumeCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                checkpoint,
                owner_delta,
                ..
            } => (
                coding_session_id,
                repository,
                base_commit_sha,
                None,
                Some(checkpoint),
                owner_delta.as_deref(),
            ),
        };
    let mut result = db
        .query("SELECT repository,base_revision,objective,owner_constraints,state,checkpoint FROM coding_sessions WHERE record::id(id)=$id AND user_id=$user LIMIT 1")
        .bind(json!({"id":session_id.to_string(),"user":signed.user_id.to_string()}))
        .await?;
    let rows: Vec<serde_json::Value> = result.take(0)?;
    let row = rows
        .into_iter()
        .next()
        .context("coding session does not belong to signed owner")?;
    let expected_repository = format!("{}/{}", repository.owner, repository.name);
    if row.get("repository").and_then(|v| v.as_str()) != Some(expected_repository.as_str())
        || row.get("base_revision").and_then(|v| v.as_str()) != Some(base_sha)
        || row.get("state").and_then(|v| v.as_str())
            != Some(if start_objective.is_some() {
                "active"
            } else {
                "suspended"
            })
    {
        bail!("signed coding session binding mismatch");
    }
    let objective = row
        .get("objective")
        .and_then(|v| v.as_str())
        .context("coding session objective missing")?;
    if objective.len() > jarvis_codex::MAX_TASK_SUMMARY_CHARS
        || start_objective.is_some_and(|value| value != objective)
    {
        bail!("signed coding objective mismatch");
    }
    if start_objective.is_none() {
        let stored: jarvis_codex::CodingCheckpoint = serde_json::from_value(
            row.get("checkpoint")
                .cloned()
                .context("resume checkpoint missing")?,
        )?;
        if checkpoint != Some(&stored) {
            bail!("signed coding checkpoint mismatch");
        }
    }
    let owner_constraints: Vec<String> = match row.get("owner_constraints") {
        Some(serde_json::Value::Array(_)) => {
            serde_json::from_value(row.get("owner_constraints").cloned().unwrap_or_default())?
        }
        None | Some(serde_json::Value::Null) => vec![],
        _ => bail!("invalid owner constraints in coding session"),
    };
    let context = TaskContextInput {
        original_objective: start_objective.is_none().then(|| objective.to_owned()),
        owner_constraints,
        recent_deltas: owner_delta.into_iter().map(str::to_owned).collect(),
        ..TaskContextInput::default()
    };
    signed.operation.compile_task_envelope(TaskContextInput {
        original_objective: context.original_objective.clone(),
        owner_constraints: context.owner_constraints.clone(),
        recent_deltas: context.recent_deltas.clone(),
        ..TaskContextInput::default()
    })?;
    let snapshot = TrustedRepositoryRegistry::load_production()?.snapshot(repository, base_sha)?;
    jarvis_codex::snapshot::validate_archive(&snapshot.archive, base_sha)?;
    Ok((snapshot, context))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path as RoutePath, State},
        http::{HeaderMap, StatusCode},
        routing::{delete, get},
        Json, Router,
    };
    use jarvis_codex::RepositoryIdentity;
    use jarvis_sandbox::{
        CollectedArtifact, ExecutionResult, NetworkPolicy, OwnedCodexSandbox, SandboxError,
        SandboxTask, ScopedSecret, TaskInput,
    };
    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    type ManagerItems = Arc<Mutex<Vec<serde_json::Value>>>;

    /// Records every manager call. Any call at all is a fail-closed violation
    /// in the default-configuration tests.
    #[derive(Default)]
    struct CountingManager {
        calls: Mutex<Vec<&'static str>>,
    }

    impl CountingManager {
        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }
    }

    #[async_trait::async_trait]
    impl SandboxProvider for CountingManager {
        async fn availability(&self) -> SandboxAvailability {
            self.record("availability");
            SandboxAvailability::Available
        }
        async fn create(&self, _: &SandboxTask) -> Result<SandboxHandle, SandboxError> {
            self.record("create");
            Err(SandboxError::Unsupported)
        }
        async fn upload(&self, _: &SandboxHandle, _: TaskInput) -> Result<(), SandboxError> {
            self.record("upload");
            Err(SandboxError::Unsupported)
        }
        async fn set_network_policy(
            &self,
            _: &SandboxHandle,
            _: &NetworkPolicy,
        ) -> Result<(), SandboxError> {
            self.record("network");
            Err(SandboxError::Unsupported)
        }
        async fn provide_scoped_secret(
            &self,
            _: &SandboxHandle,
            _: ScopedSecret,
        ) -> Result<(), SandboxError> {
            self.record("credential");
            Err(SandboxError::Unsupported)
        }
        async fn exec(
            &self,
            _: &SandboxHandle,
            _: &[String],
        ) -> Result<ExecutionResult, SandboxError> {
            self.record("exec");
            Err(SandboxError::Unsupported)
        }
        async fn collect_artifacts(
            &self,
            _: &SandboxHandle,
            _: &[String],
        ) -> Result<Vec<CollectedArtifact>, SandboxError> {
            self.record("artifacts");
            Err(SandboxError::Unsupported)
        }
        async fn terminate(&self, _: SandboxHandle) -> Result<(), SandboxError> {
            self.record("terminate");
            Err(SandboxError::Unsupported)
        }
        async fn list_owned_codex(&self) -> Result<Vec<OwnedCodexSandbox>, SandboxError> {
            self.record("list");
            Err(SandboxError::Unsupported)
        }
    }

    fn broker(
        db: jarvis_store::Database,
        sandbox: Option<Arc<dyn SandboxProvider>>,
        adapter: Arc<dyn CodexSubscriptionAdapter>,
        artifacts_root: PathBuf,
    ) -> Arc<BrokerState> {
        Arc::new(BrokerState {
            db,
            core_uid: 0,
            sandbox,
            adapter,
            artifacts_root,
            authority: Arc::new(RunCapabilityAuthority::default()),
            active: Arc::new(Mutex::new(HashMap::new())),
            run_slots: Arc::new(Semaphore::new(2)),
            cleanup_healthy: Arc::new(AtomicBool::new(true)),
            cleanup_epoch: Arc::new(AtomicU64::new(0)),
            reconcile_lock: Arc::new(AsyncMutex::new(())),
        })
    }

    /// A database handle that was never connected: any query fails, so a
    /// passing test proves the code path issued none.
    fn unconnected_db() -> jarvis_store::Database {
        Surreal::init()
    }

    async fn disposable_db(prefix: &str) -> anyhow::Result<jarvis_store::Database> {
        let db = Surreal::new::<Ws>(std::env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &std::env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &std::env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("{prefix}_{}", uuid::Uuid::now_v7().simple()))
            .use_db("test")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        Ok(db)
    }

    const BASE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn signed_start(
        user: uuid::Uuid,
        session: uuid::Uuid,
        reservation: uuid::Uuid,
    ) -> SignedCodingRequest {
        signed_start_with_timeout(user, session, reservation, 60)
    }

    fn signed_start_with_timeout(
        user: uuid::Uuid,
        session: uuid::Uuid,
        reservation: uuid::Uuid,
        timeout_secs: u64,
    ) -> SignedCodingRequest {
        let now = time::OffsetDateTime::now_utc();
        SignedCodingRequest {
            request_id: uuid::Uuid::now_v7(),
            nonce_hex: hex::encode([7_u8; 32]),
            user_id: user,
            device_id: uuid::Uuid::now_v7(),
            issued_at: now,
            expires_at: now + time::Duration::minutes(2),
            operation: CodingOperation::StartCodingRun {
                coding_session_id: session,
                repository: RepositoryIdentity {
                    id: "fixture".into(),
                    owner: "Example".into(),
                    name: "Repo".into(),
                },
                base_commit_sha: BASE_SHA.into(),
                worktree_label: None,
                objective: "Fix the bounded parser".into(),
                checkpoint: None,
                timeout_secs,
                budget_reservation_id: reservation,
                max_artifacts: 2,
                max_output_bytes: 1024,
            },
            signature_hex: hex::encode([0_u8; 64]),
        }
    }

    async fn reservation_status(
        db: &jarvis_store::Database,
        reservation: uuid::Uuid,
    ) -> anyhow::Result<String> {
        let mut response = db
            .query("SELECT status FROM coding_reservations WHERE record::id(id)=$id LIMIT 1")
            .bind(json!({"id":reservation.to_string()}))
            .await?
            .check()?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        rows.first()
            .and_then(|row| row["status"].as_str())
            .map(str::to_owned)
            .context("reservation missing")
    }

    async fn run_row(
        db: &jarvis_store::Database,
        run: uuid::Uuid,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let mut response = db
            .query("SELECT status,sandbox_state,reservation_status,failure_category FROM coding_runs WHERE record::id(id)=$run LIMIT 1")
            .bind(json!({"run":run.to_string()}))
            .await?
            .check()?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        Ok(rows.into_iter().next())
    }

    #[derive(Clone, Copy, PartialEq)]
    enum DeleteMode {
        Confirmed,
        Rejected,
        /// The manager removes the workload but the broker never sees the
        /// confirmation (crash or lost response during reconciliation).
        RemovedButErrored,
    }

    /// In-process manager metadata: no socket, no host network.
    struct ReconcileManager {
        owned: Mutex<Vec<OwnedCodexSandbox>>,
        delete: Mutex<DeleteMode>,
    }

    impl ReconcileManager {
        fn new(owned: Vec<OwnedCodexSandbox>) -> Self {
            Self {
                owned: Mutex::new(owned),
                delete: Mutex::new(DeleteMode::Confirmed),
            }
        }
    }

    fn owned(provider_id: &str, run: uuid::Uuid, session: uuid::Uuid) -> OwnedCodexSandbox {
        OwnedCodexSandbox {
            handle: SandboxHandle {
                provider_id: provider_id.into(),
                task_id: run,
                profile: jarvis_sandbox::SandboxProfile::Codex,
            },
            coding_session_id: session,
        }
    }

    #[async_trait::async_trait]
    impl SandboxProvider for ReconcileManager {
        async fn availability(&self) -> SandboxAvailability {
            SandboxAvailability::Available
        }
        async fn create(&self, _: &SandboxTask) -> Result<SandboxHandle, SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn upload(&self, _: &SandboxHandle, _: TaskInput) -> Result<(), SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn set_network_policy(
            &self,
            _: &SandboxHandle,
            _: &NetworkPolicy,
        ) -> Result<(), SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn provide_scoped_secret(
            &self,
            _: &SandboxHandle,
            _: ScopedSecret,
        ) -> Result<(), SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn exec(
            &self,
            _: &SandboxHandle,
            _: &[String],
        ) -> Result<ExecutionResult, SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn collect_artifacts(
            &self,
            _: &SandboxHandle,
            _: &[String],
        ) -> Result<Vec<CollectedArtifact>, SandboxError> {
            Err(SandboxError::Unsupported)
        }
        async fn terminate(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
            let mode = *self.delete.lock().unwrap();
            if mode != DeleteMode::Rejected {
                self.owned
                    .lock()
                    .unwrap()
                    .retain(|item| item.handle.provider_id != handle.provider_id);
            }
            match mode {
                DeleteMode::Confirmed => Ok(()),
                _ => Err(SandboxError::ProviderRequestFailed),
            }
        }
        async fn list_owned_codex(&self) -> Result<Vec<OwnedCodexSandbox>, SandboxError> {
            Ok(self.owned.lock().unwrap().clone())
        }
    }

    struct CrashCase {
        name: &'static str,
        /// None: the broker died after the lease, before CREATE coding_runs.
        status: Option<&'static str>,
        sandbox_id: Option<&'static str>,
        sandbox_state: &'static str,
        /// The manager still lists this run's workload after the crash.
        workload: Option<&'static str>,
        settled: bool,
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn broker_restart_in_every_durable_state_leaves_no_orphan_or_lease() -> anyhow::Result<()>
    {
        let db = disposable_db("codex_crash_matrix").await?;
        let cases = [
            CrashCase {
                name: "leased, run row never created",
                status: None,
                sandbox_id: None,
                sandbox_state: "not_created",
                workload: None,
                settled: false,
            },
            CrashCase {
                name: "queued",
                status: Some("queued"),
                sandbox_id: None,
                sandbox_state: "not_created",
                workload: None,
                settled: false,
            },
            CrashCase {
                name: "preparing, before sandbox create",
                status: Some("preparing"),
                sandbox_id: None,
                sandbox_state: "not_created",
                workload: None,
                settled: false,
            },
            CrashCase {
                name: "preparing, after create before sandbox_id persisted",
                status: Some("preparing"),
                sandbox_id: None,
                sandbox_state: "not_created",
                workload: Some("unrecorded"),
                settled: false,
            },
            CrashCase {
                name: "running, workload live",
                status: Some("running"),
                sandbox_id: Some("live"),
                sandbox_state: "active",
                workload: Some("live"),
                settled: false,
            },
            CrashCase {
                name: "running, after DELETE before cleanup record",
                status: Some("running"),
                sandbox_id: Some("deleted"),
                sandbox_state: "active",
                workload: None,
                settled: false,
            },
            CrashCase {
                name: "cancelling, workload live",
                status: Some("cancelling"),
                sandbox_id: Some("cancel"),
                sandbox_state: "active",
                workload: Some("cancel"),
                settled: false,
            },
            CrashCase {
                name: "completed, before reservation finish",
                status: Some("completed"),
                sandbox_id: Some("done"),
                sandbox_state: "terminated",
                workload: None,
                settled: true,
            },
            // Not reachable through run_worker (completion follows a recorded
            // cleanup); covers manual repair and future write-ordering changes.
            CrashCase {
                name: "completed, cleanup record and finish both lost",
                status: Some("completed"),
                sandbox_id: Some("done-unrecorded"),
                sandbox_state: "active",
                workload: None,
                settled: true,
            },
            CrashCase {
                name: "failed, cleanup_failed",
                status: Some("failed"),
                sandbox_id: Some("stuck"),
                sandbox_state: "cleanup_failed",
                workload: Some("stuck"),
                settled: false,
            },
            CrashCase {
                name: "cancelled, before reservation finish",
                status: Some("cancelled"),
                sandbox_id: Some("gone"),
                sandbox_state: "terminated",
                workload: None,
                settled: false,
            },
        ];
        let mut seeded = Vec::new();
        let mut workloads = Vec::new();
        for case in &cases {
            let user = uuid::Uuid::now_v7();
            let session = uuid::Uuid::now_v7();
            let run = uuid::Uuid::now_v7();
            let reservation = coding_reservations::reserve(&db, user, session).await?;
            coding_reservations::lease(&db, reservation, user, session, run, 60)
                .await?
                .context("lease")?;
            if let Some(status) = case.status {
                seed_run(
                    &db,
                    user,
                    session,
                    reservation,
                    run,
                    status,
                    case.sandbox_id,
                    case.sandbox_state,
                )
                .await?;
            }
            if let Some(id) = case.workload {
                workloads.push(owned(id, run, session));
            }
            seeded.push((run, reservation));
        }
        let manager = Arc::new(ReconcileManager::new(workloads));
        let state = broker(
            db.clone(),
            Some(manager.clone()),
            Arc::new(UnavailableSubscriptionAdapter),
            PathBuf::from("/nonexistent/jarvis-test-artifacts"),
        );
        reconcile_owned_sandboxes(&state).await?;
        reconcile_owned_sandboxes(&state).await?; // a second restart changes nothing
        assert!(manager.owned.lock().unwrap().is_empty());
        for (case, (run, reservation)) in cases.iter().zip(seeded) {
            let expected = if case.settled { "settled" } else { "released" };
            assert_eq!(
                reservation_status(&db, reservation).await?,
                expected,
                "{}",
                case.name
            );
            let Some(row) = run_row(&db, run).await? else {
                assert!(case.status.is_none(), "{}", case.name);
                continue;
            };
            assert!(
                matches!(
                    row["status"].as_str(),
                    Some("completed" | "failed" | "timed_out" | "cancelled")
                ),
                "{}: {row}",
                case.name
            );
            assert!(
                matches!(
                    row["sandbox_state"].as_str(),
                    Some("terminated" | "orphan_recovered")
                ),
                "{}: {row}",
                case.name
            );
            if matches!(
                case.status,
                Some("queued" | "preparing" | "running" | "cancelling")
            ) {
                assert_eq!(row["failure_category"], "broker_restarted", "{}", case.name);
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn crash_during_reconciliation_keeps_lease_until_a_later_scan_proves_cleanup(
    ) -> anyhow::Result<()> {
        let db = disposable_db("codex_reconcile_crash").await?;
        let user = uuid::Uuid::now_v7();
        let session = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let reservation = coding_reservations::reserve(&db, user, session).await?;
        coding_reservations::lease(&db, reservation, user, session, run, 60)
            .await?
            .context("lease")?;
        seed_run(
            &db,
            user,
            session,
            reservation,
            run,
            "running",
            Some("live"),
            "active",
        )
        .await?;
        let manager = Arc::new(ReconcileManager::new(vec![owned("live", run, session)]));
        let state = broker(
            db.clone(),
            Some(manager.clone()),
            Arc::new(UnavailableSubscriptionAdapter),
            PathBuf::from("/nonexistent/jarvis-test-artifacts"),
        );
        // Manager down: nothing may be released while the workload is listed.
        *manager.delete.lock().unwrap() = DeleteMode::Rejected;
        assert!(reconcile_owned_sandboxes(&state).await.is_err());
        assert_eq!(manager.owned.lock().unwrap().len(), 1);
        assert_eq!(reservation_status(&db, reservation).await?, "leased");
        assert_eq!(
            run_row(&db, run).await?.context("run")?["status"],
            "running"
        );
        // DELETE lands but its confirmation is lost: still no release.
        *manager.delete.lock().unwrap() = DeleteMode::RemovedButErrored;
        assert!(reconcile_owned_sandboxes(&state).await.is_err());
        assert!(manager.owned.lock().unwrap().is_empty());
        assert_eq!(reservation_status(&db, reservation).await?, "leased");
        // The next scan sees the workload gone and recovers both records.
        *manager.delete.lock().unwrap() = DeleteMode::Confirmed;
        reconcile_owned_sandboxes(&state).await?;
        assert_eq!(reservation_status(&db, reservation).await?, "released");
        let row = run_row(&db, run).await?.context("run")?;
        assert_eq!(row["status"], "failed");
        assert_eq!(row["sandbox_state"], "terminated");
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_run(
        db: &jarvis_store::Database,
        user: uuid::Uuid,
        session: uuid::Uuid,
        reservation: uuid::Uuid,
        run: uuid::Uuid,
        status: &str,
        sandbox_id: Option<&str>,
        sandbox_state: &str,
    ) -> anyhow::Result<()> {
        let sandbox = match sandbox_id {
            Some(_) => "sandbox_provider='opensandbox',sandbox_id=$sandbox",
            None => "sandbox_provider=NONE,sandbox_id=NONE",
        };
        let completed_at = if matches!(status, "completed" | "failed" | "timed_out" | "cancelled") {
            "time::now()"
        } else {
            "NONE"
        };
        db.query(format!("CREATE coding_runs SET id=$run,request_id=$request,coding_session_id=$coding_session_key,user_id=$user,device_id=$device,repository_id='fixture',repository_owner='Example',repository_name='Repo',base_sha=$base,snapshot_sha256=NONE,reservation_id=$reservation,reservation_units=1,reservation_status='active',status=$status,summary=NONE,failure_category=NONE,artifacts=[],compute_class='subscription',{sandbox},sandbox_state=$sandbox_state,sandbox_created_at=NONE,sandbox_cleanup_status=NONE,created_at=time::now(),updated_at=time::now(),completed_at={completed_at} RETURN NONE"))
            .bind(json!({"run":run.to_string(),"request":uuid::Uuid::now_v7().to_string(),"coding_session_key":session.to_string(),"user":user.to_string(),"device":uuid::Uuid::now_v7().to_string(),"base":BASE_SHA,"reservation":reservation.to_string(),"status":status,"sandbox":sandbox_id,"sandbox_state":sandbox_state}))
            .await?
            .check()?;
        Ok(())
    }

    /// Minimal ustar archive in the shape the trusted registry produces: a
    /// pax global header carrying the commit, then one regular file.
    fn snapshot_archive() -> jarvis_codex::RepositorySnapshot {
        fn entry(out: &mut Vec<u8>, name: &str, kind: u8, body: &[u8]) {
            let mut header = [0_u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000644\0");
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[148..156].copy_from_slice(b"        ");
            header[156] = kind;
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            out.extend_from_slice(&header);
            out.extend_from_slice(body);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        let mut archive = Vec::new();
        let pax = format!("{} comment={BASE_SHA}\n", 12 + BASE_SHA.len());
        entry(&mut archive, "pax_global_header", b'g', pax.as_bytes());
        entry(&mut archive, "README.md", b'0', b"safe");
        archive.resize(archive.len() + 1024, 0);
        jarvis_codex::RepositorySnapshot {
            base_commit_sha: BASE_SHA.into(),
            archive,
        }
    }

    #[test]
    fn fixture_snapshot_passes_the_production_archive_validation() {
        let snapshot = snapshot_archive();
        jarvis_codex::snapshot::validate_archive(&snapshot.archive, BASE_SHA).unwrap();
    }

    /// In-process OpenSandbox stand-in: the reviewed workload runtime runs
    /// on a temporary directory. No socket, container, credential or host
    /// network is involved.
    struct WorkspaceManager {
        root: PathBuf,
        live: Mutex<Vec<OwnedCodexSandbox>>,
        calls: Mutex<Vec<&'static str>>,
        stalled_upload: Option<Arc<tokio::sync::Notify>>,
        terminate_fails: AtomicBool,
    }

    impl WorkspaceManager {
        fn new(root: PathBuf) -> Self {
            Self {
                root,
                live: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                stalled_upload: None,
                terminate_fails: AtomicBool::new(false),
            }
        }
        fn called(&self, call: &str) -> bool {
            self.calls.lock().unwrap().contains(&call)
        }
    }

    #[async_trait::async_trait]
    impl SandboxProvider for WorkspaceManager {
        async fn availability(&self) -> SandboxAvailability {
            SandboxAvailability::Available
        }
        async fn create(&self, task: &SandboxTask) -> Result<SandboxHandle, SandboxError> {
            self.calls.lock().unwrap().push("create");
            fs::create_dir_all(self.root.join("input"))
                .map_err(|_| SandboxError::ProviderRequestFailed)?;
            let session = task.codex_session_id.ok_or(SandboxError::InvalidTask)?;
            let workload = owned(&format!("workload-{}", task.task_id), task.task_id, session);
            self.live.lock().unwrap().push(workload.clone());
            Ok(workload.handle)
        }
        async fn upload(&self, _: &SandboxHandle, input: TaskInput) -> Result<(), SandboxError> {
            if let (Some(signal), "repository.tar") = (&self.stalled_upload, input.name.as_str()) {
                signal.notify_one();
                std::future::pending::<()>().await;
            }
            fs::write(self.root.join("input").join(&input.name), input.bytes)
                .map_err(|_| SandboxError::ProviderRequestFailed)
        }
        async fn set_network_policy(
            &self,
            _: &SandboxHandle,
            _: &NetworkPolicy,
        ) -> Result<(), SandboxError> {
            Ok(())
        }
        async fn provide_scoped_secret(
            &self,
            _: &SandboxHandle,
            _: ScopedSecret,
        ) -> Result<(), SandboxError> {
            self.calls.lock().unwrap().push("credential");
            Err(SandboxError::Unsupported)
        }
        async fn exec(
            &self,
            _: &SandboxHandle,
            _: &[String],
        ) -> Result<ExecutionResult, SandboxError> {
            let root = self.root.clone();
            tokio::task::spawn_blocking(move || jarvis_codex::runtime::run_in_workspace(&root))
                .await
                .map_err(|_| SandboxError::ProviderRequestFailed)?
                .map_err(|_| SandboxError::ProviderRequestFailed)?;
            Ok(ExecutionResult {
                exit_code: Some(0),
                timed_out: false,
                stdout_summary: String::new(),
                stderr_summary: String::new(),
                duration_ms: 1,
            })
        }
        async fn collect_artifacts(
            &self,
            _: &SandboxHandle,
            paths: &[String],
        ) -> Result<Vec<CollectedArtifact>, SandboxError> {
            paths
                .iter()
                .map(|path| {
                    Ok(CollectedArtifact {
                        path: path.clone(),
                        contents: fs::read(self.root.join("artifacts").join(path))
                            .map_err(|_| SandboxError::InvalidArtifact)?,
                    })
                })
                .collect()
        }
        async fn read_codex_task_request(
            &self,
            _: &SandboxHandle,
        ) -> Result<Option<Vec<u8>>, SandboxError> {
            let path = self.root.join("channel/task-request.json");
            if !path.exists() {
                return Ok(None);
            }
            fs::read(path)
                .map(Some)
                .map_err(|_| SandboxError::InvalidArtifact)
        }
        async fn terminate(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
            self.calls.lock().unwrap().push("terminate");
            if self.terminate_fails.load(Ordering::SeqCst) {
                return Err(SandboxError::ProviderRequestFailed);
            }
            self.live
                .lock()
                .unwrap()
                .retain(|item| item.handle.provider_id != handle.provider_id);
            Ok(())
        }
        async fn list_owned_codex(&self) -> Result<Vec<OwnedCodexSandbox>, SandboxError> {
            Ok(self.live.lock().unwrap().clone())
        }
    }

    struct NoDatabase;

    #[async_trait::async_trait]
    impl SandboxOwnershipRecorder for NoDatabase {
        async fn created(&self, _: &SandboxHandle) -> Result<(), CodingRunError> {
            Ok(())
        }
        async fn cleanup(&self, _: &SandboxHandle, _: bool) -> Result<(), CodingRunError> {
            Ok(())
        }
    }

    /// Proves the in-process manager drives the reviewed workload runtime
    /// end to end without a database, so the DB lifecycle tests exercise
    /// broker state rather than fixture plumbing.
    #[tokio::test]
    async fn workspace_manager_runs_the_reviewed_runtime_end_to_end() {
        let workspace = tempfile::tempdir().unwrap();
        let manager = WorkspaceManager::new(workspace.path().join("sandbox"));
        let adapter = FixtureAdapter::Outcome(jarvis_codex::runtime::TaskOutcome::Completed);
        let request = signed_start(
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        );
        let authority = RunCapabilityAuthority::default();
        let run_id = uuid::Uuid::now_v7();
        let capability = authority
            .mint(RunCapabilityClaims::from_signed_request(run_id, &request, 1).unwrap())
            .unwrap();
        let result = jarvis_codex::execute_in_sandbox_tracked(
            &manager,
            &request,
            snapshot_archive(),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &adapter,
                authority: &authority,
                run_id,
                capability: Some(capability),
            },
            None,
            &NoDatabase,
        )
        .await
        .unwrap();
        assert_eq!(result.status, "completed");
        assert_eq!(result.stdout_summary, "Reviewed fixture");
        assert!(manager.live.lock().unwrap().is_empty());
        assert!(!manager.called("credential"));
    }

    /// Test-only stand-in for the future reviewed subscription adapter.
    /// Production keeps `UnavailableSubscriptionAdapter` hard-wired in main.
    enum FixtureAdapter {
        Outcome(jarvis_codex::runtime::TaskOutcome),
        Fails,
        Sleeps(Duration),
        Blocks(Arc<tokio::sync::Notify>),
    }

    #[async_trait::async_trait]
    impl CodexSubscriptionAdapter for FixtureAdapter {
        fn available(&self) -> bool {
            true
        }
        async fn run_approved_task(
            &self,
            binding: &jarvis_codex::BrokeredCodexRequest,
            _: &jarvis_codex::TaskEnvelope,
        ) -> Result<jarvis_codex::runtime::TaskChannelResponse, CodingRunError> {
            let outcome = match self {
                Self::Outcome(outcome) => *outcome,
                Self::Fails => return Err(CodingRunError::SandboxFailed),
                Self::Sleeps(duration) => {
                    tokio::time::sleep(*duration).await;
                    return Err(CodingRunError::SandboxFailed);
                }
                Self::Blocks(entered) => {
                    entered.notify_one();
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            };
            Ok(jarvis_codex::runtime::TaskChannelResponse {
                binding: binding.clone(),
                outcome,
                summary: "Reviewed fixture".into(),
                patch_diff: String::new(),
            })
        }
    }

    async fn create_session(
        db: &jarvis_store::Database,
        user: uuid::Uuid,
        session: uuid::Uuid,
        state: &str,
    ) -> anyhow::Result<()> {
        db.query("CREATE coding_sessions SET id=$id,user_id=$user,repository='Example/Repo',base_revision=$base,objective='Fix the bounded parser',owner_constraints=[],state=$state,checkpoint=NONE,created_at=time::now(),updated_at=time::now() RETURN NONE")
            .bind(json!({"id":session.to_string(),"user":user.to_string(),"base":BASE_SHA,"state":state}))
            .await?
            .check()?;
        Ok(())
    }

    struct Lifecycle {
        db: jarvis_store::Database,
        user: uuid::Uuid,
        session: uuid::Uuid,
        reservation: uuid::Uuid,
        workspace: tempfile::TempDir,
    }

    async fn lifecycle(prefix: &str, with_session: bool) -> anyhow::Result<Lifecycle> {
        let db = disposable_db(prefix).await?;
        let user = uuid::Uuid::now_v7();
        let session = uuid::Uuid::now_v7();
        if with_session {
            create_session(&db, user, session, "active").await?;
        }
        let reservation = coding_reservations::reserve(&db, user, session).await?;
        Ok(Lifecycle {
            db,
            user,
            session,
            reservation,
            workspace: tempfile::tempdir()?,
        })
    }

    /// Admit one prepared run exactly as `start_run` does after the
    /// registry snapshot, and return the worker so the test can await it.
    async fn admit(
        state: &Arc<BrokerState>,
        request: &SignedCodingRequest,
    ) -> anyhow::Result<(uuid::Uuid, tokio::task::JoinHandle<()>)> {
        let slot = state.run_slots.clone().try_acquire_owned()?;
        let reconcile = state.reconcile_lock.lock().await;
        admit_prepared_run(
            state,
            &reconcile,
            request,
            slot,
            snapshot_archive(),
            TaskContextInput::default(),
        )
        .await
    }

    async fn run_to_end(
        fixture: &Lifecycle,
        adapter: FixtureAdapter,
        timeout_secs: u64,
        artifacts_root: PathBuf,
    ) -> anyhow::Result<(Arc<BrokerState>, Arc<WorkspaceManager>, uuid::Uuid)> {
        let manager = Arc::new(WorkspaceManager::new(
            fixture.workspace.path().join("sandbox"),
        ));
        let state = broker(
            fixture.db.clone(),
            Some(manager.clone()),
            Arc::new(adapter),
            artifacts_root,
        );
        let request = signed_start_with_timeout(
            fixture.user,
            fixture.session,
            fixture.reservation,
            timeout_secs,
        );
        let (run, worker) = admit(&state, &request).await?;
        worker.await?;
        Ok((state, manager, run))
    }

    /// Every finished worker must leave: a terminal run, a destroyed
    /// workload, no credential hand-off, a free slot and open admission.
    async fn assert_finished(
        fixture: &Lifecycle,
        state: &BrokerState,
        manager: &WorkspaceManager,
        run: uuid::Uuid,
        status: &str,
        reservation: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let row = run_row(&fixture.db, run).await?.context("run row")?;
        assert_eq!(row["status"], status, "{row}");
        assert_eq!(row["sandbox_state"], "terminated", "{row}");
        assert_eq!(
            reservation_status(&fixture.db, fixture.reservation).await?,
            reservation
        );
        assert!(manager.live.lock().unwrap().is_empty());
        assert!(manager.called("terminate"));
        assert!(!manager.called("credential"));
        assert!(state.active.lock().unwrap().is_empty());
        assert_eq!(state.run_slots.available_permits(), 2);
        assert!(state.cleanup_healthy.load(Ordering::SeqCst));
        Ok(row)
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn disposable_lifecycle_completes_settles_and_destroys_the_sandbox() -> anyhow::Result<()>
    {
        let fixture = lifecycle("codex_lifecycle", true).await?;
        let (state, manager, run) = run_to_end(
            &fixture,
            FixtureAdapter::Outcome(jarvis_codex::runtime::TaskOutcome::Completed),
            60,
            fixture.workspace.path().join("artifacts"),
        )
        .await?;
        assert_finished(&fixture, &state, &manager, run, "completed", "settled").await?;
        let mut session = fixture
            .db
            .query("SELECT state,checkpoint FROM coding_sessions WHERE record::id(id)=$id LIMIT 1")
            .bind(json!({"id":fixture.session.to_string()}))
            .await?
            .check()?;
        let session: Vec<serde_json::Value> = session.take(0)?;
        assert_eq!(session[0]["state"], "suspended");
        assert_eq!(session[0]["checkpoint"]["summary"], "Reviewed fixture");
        let result = read_artifact(
            &state,
            run,
            fixture.user,
            jarvis_codex::CodingArtifactName::ResultJson,
        )
        .await?;
        assert!(result.contains("Reviewed fixture"));
        // Another owner can neither read the artifact nor learn the status.
        assert!(read_artifact(
            &state,
            run,
            uuid::Uuid::now_v7(),
            jarvis_codex::CodingArtifactName::ResultJson
        )
        .await
        .is_err());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn disposable_lifecycle_owner_cancel_releases_and_destroys() -> anyhow::Result<()> {
        let fixture = lifecycle("codex_lifecycle_cancel", true).await?;
        let entered = Arc::new(tokio::sync::Notify::new());
        let manager = Arc::new(WorkspaceManager::new(
            fixture.workspace.path().join("sandbox"),
        ));
        let state = broker(
            fixture.db.clone(),
            Some(manager.clone()),
            Arc::new(FixtureAdapter::Blocks(entered.clone())),
            fixture.workspace.path().join("artifacts"),
        );
        let request =
            signed_start_with_timeout(fixture.user, fixture.session, fixture.reservation, 5);
        let (run, worker) = admit(&state, &request).await?;
        entered.notified().await;
        assert_eq!(
            run_row(&fixture.db, run).await?.context("run")?["status"],
            "running"
        );
        // Only the owner may cancel.
        assert!(cancel_run(&state, run, uuid::Uuid::now_v7()).await.is_err());
        cancel_run(&state, run, fixture.user).await?;
        worker.await?;
        let row = assert_finished(&fixture, &state, &manager, run, "cancelled", "released").await?;
        assert_eq!(row["reservation_status"], "released");
        assert!(!fixture
            .workspace
            .path()
            .join("artifacts")
            .join(run.to_string())
            .exists());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn worker_exit_paths_release_the_reservation() -> anyhow::Result<()> {
        use jarvis_codex::runtime::TaskOutcome;
        // (case, adapter, signed timeout, run status, failure category)
        let cases = [
            (
                "provider failure",
                FixtureAdapter::Fails,
                5,
                "failed",
                "runtime_failure",
            ),
            (
                "plan limit",
                FixtureAdapter::Outcome(TaskOutcome::PlanLimit),
                5,
                "failed",
                "plan_limit",
            ),
            (
                "signed timeout",
                FixtureAdapter::Sleeps(Duration::from_secs(3)),
                1,
                "timed_out",
                "runtime_timeout",
            ),
        ];
        for (case, adapter, timeout, status, failure) in cases {
            let fixture = lifecycle("codex_exit_path", true).await?;
            let (state, manager, run) = run_to_end(
                &fixture,
                adapter,
                timeout,
                fixture.workspace.path().join("artifacts"),
            )
            .await?;
            let row = assert_finished(&fixture, &state, &manager, run, status, "released").await?;
            assert_eq!(row["failure_category"], failure, "{case}");
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn completed_run_that_cannot_persist_releases_instead_of_settling() -> anyhow::Result<()>
    {
        // Artifact store is unusable: the run must not be reported completed.
        let fixture = lifecycle("codex_artifact_failure", true).await?;
        let blocked = fixture.workspace.path().join("not-a-directory");
        fs::write(&blocked, b"")?;
        let (state, manager, run) = run_to_end(
            &fixture,
            FixtureAdapter::Outcome(jarvis_codex::runtime::TaskOutcome::Completed),
            5,
            blocked,
        )
        .await?;
        let row = assert_finished(&fixture, &state, &manager, run, "failed", "released").await?;
        assert_eq!(row["failure_category"], "artifact_persistence");

        // The resume checkpoint cannot be stored (session row missing).
        let fixture = lifecycle("codex_checkpoint_failure", false).await?;
        let artifacts = fixture.workspace.path().join("artifacts");
        let (state, manager, run) = run_to_end(
            &fixture,
            FixtureAdapter::Outcome(jarvis_codex::runtime::TaskOutcome::Completed),
            5,
            artifacts.clone(),
        )
        .await?;
        let row = assert_finished(&fixture, &state, &manager, run, "failed", "released").await?;
        assert_eq!(row["failure_category"], "checkpoint_persistence");
        assert!(!artifacts.join(run.to_string()).exists());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn unconfirmed_teardown_closes_admission_until_reconciliation() -> anyhow::Result<()> {
        let fixture = lifecycle("codex_cleanup_required", true).await?;
        let manager = Arc::new(WorkspaceManager::new(
            fixture.workspace.path().join("sandbox"),
        ));
        manager.terminate_fails.store(true, Ordering::SeqCst);
        let state = broker(
            fixture.db.clone(),
            Some(manager.clone()),
            Arc::new(FixtureAdapter::Outcome(
                jarvis_codex::runtime::TaskOutcome::Completed,
            )),
            fixture.workspace.path().join("artifacts"),
        );
        let request =
            signed_start_with_timeout(fixture.user, fixture.session, fixture.reservation, 5);
        let (run, worker) = admit(&state, &request).await?;
        worker.await?;
        // Workload still listed: no completion, no settlement, no admission.
        assert_eq!(manager.live.lock().unwrap().len(), 1);
        assert!(!state.cleanup_healthy.load(Ordering::SeqCst));
        assert_eq!(
            reservation_status(&fixture.db, fixture.reservation).await?,
            "leased"
        );
        let row = run_row(&fixture.db, run).await?.context("run")?;
        assert_eq!(row["status"], "running");
        assert_eq!(row["failure_category"], "cleanup_required");
        let next = coding_reservations::reserve(&fixture.db, fixture.user, fixture.session).await?;
        let blocked = start_run(&state, &signed_start(fixture.user, fixture.session, next)).await;
        assert!(blocked
            .unwrap_err()
            .to_string()
            .contains("cleanup requires recovery"));

        manager.terminate_fails.store(false, Ordering::SeqCst);
        reconcile_owned_sandboxes(&state).await?;
        assert!(manager.live.lock().unwrap().is_empty());
        assert_eq!(
            reservation_status(&fixture.db, fixture.reservation).await?,
            "released"
        );
        assert_eq!(reservation_status(&fixture.db, next).await?, "reserved");
        let row = run_row(&fixture.db, run).await?.context("run")?;
        assert_eq!(row["status"], "failed");
        assert_eq!(row["failure_category"], "broker_restarted");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn aborted_worker_mid_upload_is_recovered_by_reconciliation() -> anyhow::Result<()> {
        let fixture = lifecycle("codex_worker_abort", true).await?;
        let stalled = Arc::new(tokio::sync::Notify::new());
        let mut manager = WorkspaceManager::new(fixture.workspace.path().join("sandbox"));
        manager.stalled_upload = Some(stalled.clone());
        let manager = Arc::new(manager);
        let state = broker(
            fixture.db.clone(),
            Some(manager.clone()),
            Arc::new(FixtureAdapter::Outcome(
                jarvis_codex::runtime::TaskOutcome::Completed,
            )),
            fixture.workspace.path().join("artifacts"),
        );
        let request = signed_start(fixture.user, fixture.session, fixture.reservation);
        let (run, worker) = admit(&state, &request).await?;
        stalled.notified().await;
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        // The dropped worker could not tear down: admission closes.
        assert!(!state.cleanup_healthy.load(Ordering::SeqCst));
        assert!(state.active.lock().unwrap().is_empty());
        assert_eq!(manager.live.lock().unwrap().len(), 1);
        assert_eq!(
            reservation_status(&fixture.db, fixture.reservation).await?,
            "leased"
        );

        reconcile_owned_sandboxes(&state).await?;
        assert!(manager.live.lock().unwrap().is_empty());
        assert_eq!(
            reservation_status(&fixture.db, fixture.reservation).await?,
            "released"
        );
        let row = run_row(&fixture.db, run).await?.context("run")?;
        assert_eq!(row["status"], "failed");
        assert!(matches!(
            row["sandbox_state"].as_str(),
            Some("terminated" | "orphan_recovered")
        ));
        Ok(())
    }

    /// Reservation verification: one test per check, and every rejected
    /// lease leaves the reservation unspent.
    async fn reservation_fixture(
        prefix: &str,
    ) -> anyhow::Result<(jarvis_store::Database, uuid::Uuid, uuid::Uuid, uuid::Uuid)> {
        let db = disposable_db(prefix).await?;
        let user = uuid::Uuid::now_v7();
        let session = uuid::Uuid::now_v7();
        let reservation = coding_reservations::reserve(&db, user, session).await?;
        Ok((db, user, session, reservation))
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_lease_rejects_an_unknown_id() -> anyhow::Result<()> {
        let (db, user, session, reservation) =
            reservation_fixture("codex_reservation_unknown").await?;
        let unknown = uuid::Uuid::now_v7();
        assert!(
            coding_reservations::lease(&db, unknown, user, session, uuid::Uuid::now_v7(), 60)
                .await?
                .is_none()
        );
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_lease_rejects_another_owner() -> anyhow::Result<()> {
        let (db, _, session, reservation) = reservation_fixture("codex_reservation_owner").await?;
        let other = uuid::Uuid::now_v7();
        assert!(coding_reservations::lease(
            &db,
            reservation,
            other,
            session,
            uuid::Uuid::now_v7(),
            60
        )
        .await?
        .is_none());
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_lease_rejects_another_coding_session() -> anyhow::Result<()> {
        let (db, user, _, reservation) = reservation_fixture("codex_reservation_session").await?;
        let other = uuid::Uuid::now_v7();
        assert!(coding_reservations::lease(
            &db,
            reservation,
            user,
            other,
            uuid::Uuid::now_v7(),
            60
        )
        .await?
        .is_none());
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn inactive_coding_session_is_rejected_before_any_lease() -> anyhow::Result<()> {
        let (db, user, session, reservation) =
            reservation_fixture("codex_reservation_inactive").await?;
        create_session(&db, user, session, "completed").await?;
        let Err(denied) = prepare_signed_run(&db, &signed_start(user, session, reservation)).await
        else {
            panic!("inactive coding session was prepared");
        };
        assert!(denied.to_string().contains("binding mismatch"));
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_lease_rejects_insufficient_runtime() -> anyhow::Result<()> {
        let (db, user, session, reservation) =
            reservation_fixture("codex_reservation_runtime").await?;
        let too_long = u64::from(coding_reservations::MAX_RUNTIME_SECS) + 1;
        assert!(coding_reservations::lease(
            &db,
            reservation,
            user,
            session,
            uuid::Uuid::now_v7(),
            too_long
        )
        .await?
        .is_none());
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_lease_rejects_an_expired_reservation() -> anyhow::Result<()> {
        let (db, user, session, reservation) =
            reservation_fixture("codex_reservation_expired").await?;
        db.query("UPDATE coding_reservations SET expires_at=time::now()-1s WHERE record::id(id)=$id RETURN NONE")
            .bind(json!({"id":reservation.to_string()}))
            .await?
            .check()?;
        assert!(coding_reservations::lease(
            &db,
            reservation,
            user,
            session,
            uuid::Uuid::now_v7(),
            60
        )
        .await?
        .is_none());
        assert_eq!(reservation_status(&db, reservation).await?, "reserved");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn reservation_is_single_use_across_concurrent_and_later_leases() -> anyhow::Result<()> {
        let db = disposable_db("codex_reservation_single_use").await?;
        let user = uuid::Uuid::now_v7();
        let session = uuid::Uuid::now_v7();
        for settled in [true, false] {
            let reservation = coding_reservations::reserve(&db, user, session).await?;
            let (a, b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
            let (lease_a, lease_b) = tokio::join!(
                coding_reservations::lease(&db, reservation, user, session, a, 60),
                coding_reservations::lease(&db, reservation, user, session, b, 60),
            );
            let (lease_a, lease_b) = (lease_a?, lease_b?);
            assert_eq!(
                usize::from(lease_a.is_some()) + usize::from(lease_b.is_some()),
                1
            );
            let (winner, loser) = if lease_a.is_some() { (a, b) } else { (b, a) };
            // Only the leasing run can finish it, and only once.
            assert!(!coding_reservations::finish(&db, reservation, loser, settled).await?);
            assert!(coding_reservations::finish(&db, reservation, winner, settled).await?);
            assert!(!coding_reservations::finish(&db, reservation, winner, settled).await?);
            assert!(coding_reservations::lease(
                &db,
                reservation,
                user,
                session,
                uuid::Uuid::now_v7(),
                60
            )
            .await?
            .is_none());
            let expected = if settled { "settled" } else { "released" };
            assert_eq!(reservation_status(&db, reservation).await?, expected);
        }
        Ok(())
    }

    #[test]
    fn default_configuration_reads_only_the_execution_flag() {
        for flag in [None, Some("0"), Some("true"), Some(" 1"), Some("")] {
            let read = Mutex::new(Vec::new());
            let sandbox = configured_sandbox(|name| {
                read.lock().unwrap().push(name.to_owned());
                // Every other key is present: only the flag may decide.
                if name == "JARVIS_CODEX_EXECUTION_ENABLED" {
                    flag.map(str::to_owned)
                } else {
                    Some("must-not-be-read".into())
                }
            })
            .unwrap();
            assert!(sandbox.is_none());
            assert_eq!(
                *read.lock().unwrap(),
                ["JARVIS_CODEX_EXECUTION_ENABLED".to_owned()]
            );
        }
        // Enabled but incomplete configuration fails closed instead of
        // defaulting to some manager.
        let read = Mutex::new(Vec::new());
        let enabled_without_manager = configured_sandbox(|name| {
            read.lock().unwrap().push(name.to_owned());
            (name == "JARVIS_CODEX_EXECUTION_ENABLED").then(|| "1".to_owned())
        });
        assert!(enabled_without_manager.is_err());
        assert!(!read
            .lock()
            .unwrap()
            .contains(&"JARVIS_CODEX_OPENSANDBOX_API_KEY".to_owned()));
    }

    #[tokio::test]
    async fn default_configuration_admits_nothing_and_touches_nothing() {
        let request = signed_start(
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        );
        // Production default: execution flag unset, so no manager at all.
        let state = broker(
            unconnected_db(),
            None,
            Arc::new(UnavailableSubscriptionAdapter),
            PathBuf::from("/nonexistent/jarvis-test-artifacts"),
        );
        let denied = start_run(&state, &request).await.unwrap_err();
        assert!(denied.to_string().contains("not owner-enabled"));
        // Without a manager there is nothing to reconcile and no lease may
        // be released; the unconnected database proves no query was issued.
        reconcile_owned_sandboxes(&state).await.unwrap();
        assert!(state.active.lock().unwrap().is_empty());
        assert_eq!(state.run_slots.available_permits(), 2);

        // Even an owner-enabled manager stays untouched while the reviewed
        // subscription adapter is unavailable.
        let manager = Arc::new(CountingManager::default());
        let state = broker(
            unconnected_db(),
            Some(manager.clone()),
            Arc::new(UnavailableSubscriptionAdapter),
            PathBuf::from("/nonexistent/jarvis-test-artifacts"),
        );
        let denied = start_run(&state, &request).await.unwrap_err();
        assert!(denied
            .to_string()
            .contains("provider-only interface unavailable"));
        assert!(manager.calls.lock().unwrap().is_empty());
        assert!(state.active.lock().unwrap().is_empty());
        assert_eq!(state.run_slots.available_permits(), 2);
    }

    #[derive(Clone)]
    struct ManagerFixture {
        items: ManagerItems,
        reject_delete: Arc<AtomicBool>,
    }

    #[test]
    fn lost_worker_cannot_hide_an_owned_sandbox_from_reconciliation() {
        let run_id = uuid::Uuid::now_v7();
        let (sender, _) = watch::channel(false);
        let active = Arc::new(Mutex::new(HashMap::from([(run_id, sender)])));
        let healthy = Arc::new(AtomicBool::new(true));
        let epoch = Arc::new(AtomicU64::new(0));
        let guard = ActiveRunGuard {
            active: active.clone(),
            cleanup_healthy: healthy.clone(),
            cleanup_epoch: epoch.clone(),
            run_id,
        };
        drop(guard);
        assert!(!active.lock().unwrap().contains_key(&run_id));
        assert!(!healthy.load(Ordering::SeqCst));
        assert_eq!(epoch.load(Ordering::SeqCst), 1);

        // A normal worker already removed itself after confirmed teardown.
        healthy.store(true, Ordering::SeqCst);
        let guard = ActiveRunGuard {
            active,
            cleanup_healthy: healthy.clone(),
            cleanup_epoch: epoch.clone(),
            run_id,
        };
        drop(guard);
        assert!(healthy.load(Ordering::SeqCst));
        assert_eq!(epoch.load(Ordering::SeqCst), 1);
    }

    async fn listed(
        State(fixture): State<ManagerFixture>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, StatusCode> {
        if headers
            .get("OPEN-SANDBOX-API-KEY")
            .and_then(|v| v.to_str().ok())
            != Some("fixture-key")
        {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Json(
            json!({"items":*fixture.items.lock().unwrap(),"pagination":{"hasNextPage":false}}),
        ))
    }

    async fn deleted(
        State(fixture): State<ManagerFixture>,
        RoutePath(id): RoutePath<String>,
        headers: HeaderMap,
    ) -> StatusCode {
        if headers
            .get("OPEN-SANDBOX-API-KEY")
            .and_then(|v| v.to_str().ok())
            != Some("fixture-key")
        {
            return StatusCode::UNAUTHORIZED;
        }
        if fixture.reject_delete.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        let mut items = fixture.items.lock().unwrap();
        let before = items.len();
        items.retain(|item| item.get("id").and_then(serde_json::Value::as_str) != Some(&id));
        if before == items.len() {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::NO_CONTENT
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn startup_reconciles_stale_and_unrecorded_owned_sandboxes_without_touching_others(
    ) -> anyhow::Result<()> {
        let db = disposable_db("codex_recovery_fixture").await?;
        let user = uuid::Uuid::now_v7();
        let session = uuid::Uuid::now_v7();
        let run = uuid::Uuid::now_v7();
        let orphan = uuid::Uuid::now_v7();
        let reservation = coding_reservations::reserve(&db, user, session).await?;
        coding_reservations::lease(&db, reservation, user, session, run, 60)
            .await?
            .context("reservation lease failed")?;
        db.query("CREATE coding_runs SET id=$run,request_id=$request,coding_session_id=$coding_session_key,user_id=$user,device_id=$device,repository_id='fixture',repository_owner='Example',repository_name='Repo',base_sha=$base,snapshot_sha256=NONE,reservation_id=$reservation,reservation_units=1,reservation_status='active',status='running',summary=NONE,failure_category=NONE,artifacts=[],compute_class='subscription',sandbox_provider='opensandbox',sandbox_id='stale-owned',sandbox_state='active',sandbox_created_at=time::now(),sandbox_cleanup_status=NONE,created_at=time::now(),updated_at=time::now(),completed_at=NONE RETURN NONE")
            .bind(json!({"run":run.to_string(),"request":uuid::Uuid::now_v7().to_string(),"coding_session_key":session.to_string(),"user":user.to_string(),"device":uuid::Uuid::now_v7().to_string(),"base":"a".repeat(40),"reservation":reservation.to_string()}))
            .await?.check()?;
        let items: ManagerItems = Arc::new(Mutex::new(vec![
            json!({"id":"stale-owned","metadata":{"jarvis.profile":"codex","jarvis.run_id":run.to_string(),"jarvis.session_id":session.to_string()}}),
            json!({"id":"unrecorded-owned","metadata":{"jarvis.profile":"codex","jarvis.run_id":orphan.to_string(),"jarvis.session_id":session.to_string()}}),
            json!({"id":"unrelated","metadata":{"jarvis.profile":"research"}}),
        ]));
        let reject_delete = Arc::new(AtomicBool::new(true));
        let app = Router::new()
            .route("/v1/sandboxes", get(listed))
            .route("/v1/sandboxes/{id}", delete(deleted))
            .with_state(ManagerFixture {
                items: items.clone(),
                reject_delete: reject_delete.clone(),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let manager = Arc::new(OpenSandboxProvider::for_codex_broker(
            format!("http://{address}/"),
            "fixture-key".into(),
            format!("fixture/codex@sha256:{}", "a".repeat(64)),
        )?);
        let state = BrokerState {
            db: db.clone(),
            core_uid: 0,
            sandbox: Some(manager),
            adapter: Arc::new(UnavailableSubscriptionAdapter),
            artifacts_root: PathBuf::from("/nonexistent/jarvis-test-artifacts"),
            authority: Arc::new(RunCapabilityAuthority::default()),
            active: Arc::new(Mutex::new(HashMap::new())),
            run_slots: Arc::new(Semaphore::new(2)),
            cleanup_healthy: Arc::new(AtomicBool::new(false)),
            cleanup_epoch: Arc::new(AtomicU64::new(0)),
            reconcile_lock: Arc::new(AsyncMutex::new(())),
        };
        // A failed manager deletion must not release the subscription lease
        // or mark the stale run complete. Admission stays closed until a
        // later scan proves every Jarvis-owned workload was removed.
        assert!(reconcile_owned_sandboxes(&state).await.is_err());
        assert!(!state.cleanup_healthy.load(Ordering::SeqCst));
        assert_eq!(items.lock().unwrap().len(), 3);
        let mut blocked = db
            .query("SELECT status FROM coding_reservations WHERE record::id(id)=$id LIMIT 1")
            .bind(json!({"id":reservation.to_string()}))
            .await?
            .check()?;
        let blocked: Vec<serde_json::Value> = blocked.take(0)?;
        assert_eq!(blocked[0]["status"], "leased");
        let mut stale = db
            .query("SELECT status,sandbox_state FROM coding_runs WHERE record::id(id)=$run LIMIT 1")
            .bind(json!({"run":run.to_string()}))
            .await?
            .check()?;
        let stale: Vec<serde_json::Value> = stale.take(0)?;
        assert_eq!(stale[0]["status"], "running");
        assert_eq!(stale[0]["sandbox_state"], "active");
        reject_delete.store(false, Ordering::SeqCst);
        reconcile_owned_sandboxes(&state).await?;
        reconcile_owned_sandboxes(&state).await?; // idempotent restart
        let remaining = items.lock().unwrap().clone();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["id"], "unrelated");
        let mut rows = db
            .query("SELECT status,sandbox_state FROM coding_runs WHERE record::id(id)=$run LIMIT 1")
            .bind(json!({"run":run.to_string()}))
            .await?
            .check()?;
        let rows: Vec<serde_json::Value> = rows.take(0)?;
        assert_eq!(rows[0]["status"], "failed");
        assert_eq!(rows[0]["sandbox_state"], "terminated");
        let mut lease = db.query("SELECT status,api_spend_cents FROM coding_reservations WHERE record::id(id)=$id LIMIT 1")
            .bind(json!({"id":reservation.to_string()})).await?.check()?;
        let lease: Vec<serde_json::Value> = lease.take(0)?;
        assert_eq!(lease[0]["status"], "released");
        assert_eq!(lease[0]["api_spend_cents"], 0);
        server.abort();
        Ok(())
    }

    #[test]
    fn artifact_store_accepts_only_fixed_bounded_regular_outputs() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("artifacts");
        let run = uuid::Uuid::now_v7();
        let valid = vec![
            CollectedArtifact {
                path: "result.json".into(),
                contents: b"{}".to_vec(),
            },
            CollectedArtifact {
                path: "patch.diff".into(),
                contents: Vec::new(),
            },
        ];
        save_artifacts_in(&root, run, &valid).unwrap();
        assert_eq!(
            fs::read(root.join(run.to_string()).join("result.json")).unwrap(),
            b"{}"
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let bad = vec![
            CollectedArtifact {
                path: "result.json".into(),
                contents: b"{}".to_vec(),
            },
            CollectedArtifact {
                path: "../escape".into(),
                contents: Vec::new(),
            },
        ];
        assert!(save_artifacts_in(&root, uuid::Uuid::now_v7(), &bad).is_err());
        assert!(!fixture.path().join("escape").exists());
        assert!(save_artifacts_in(
            &root,
            uuid::Uuid::now_v7(),
            &[
                CollectedArtifact {
                    path: "result.json".into(),
                    contents: vec![0; 700 * 1024]
                },
                CollectedArtifact {
                    path: "patch.diff".into(),
                    contents: Vec::new()
                },
            ]
        )
        .is_err());
    }
}
