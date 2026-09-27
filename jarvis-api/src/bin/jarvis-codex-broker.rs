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
    path::Path,
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
    sandbox: Option<Arc<OpenSandboxProvider>>,
    adapter: Arc<dyn CodexSubscriptionAdapter>,
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
        sandbox: configured_sandbox()?,
        adapter: Arc::new(UnavailableSubscriptionAdapter),
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

fn configured_sandbox() -> anyhow::Result<Option<Arc<OpenSandboxProvider>>> {
    if std::env::var("JARVIS_CODEX_EXECUTION_ENABLED")
        .ok()
        .as_deref()
        != Some("1")
    {
        return Ok(None);
    }
    let endpoint = std::env::var("JARVIS_CODEX_OPENSANDBOX_ENDPOINT")?;
    let key = std::env::var("JARVIS_CODEX_OPENSANDBOX_API_KEY")?;
    let digest = std::env::var("JARVIS_CODEX_WORKLOAD_IMAGE")?;
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
    let mut response = state.db.query("SELECT record::id(id) AS run_id,reservation_id,user_id,status,sandbox_id,sandbox_state FROM coding_runs WHERE status IN ['queued','preparing','running','cancelling'] OR sandbox_state='cleanup_failed' LIMIT 1000")
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
        let _ =
            coding_reservations::finish(&state.db, reservation_id, run_id, status == "completed")
                .await?;
        if matches!(status, "queued" | "preparing" | "running" | "cancelling") {
            state.db.query("UPDATE coding_runs SET status='failed',failure_category='broker_restarted',reservation_status='released',sandbox_state='terminated',sandbox_cleanup_status='terminated',updated_at=time::now(),completed_at=time::now() WHERE record::id(id)=$run AND status IN ['queued','preparing','running','cancelling'] RETURN NONE")
                .bind(json!({"run":run_id.to_string()})).await?.check()?;
        } else {
            state.db.query("UPDATE coding_runs SET sandbox_state='terminated',sandbox_cleanup_status='terminated',reservation_status='released',updated_at=time::now() WHERE record::id(id)=$run AND sandbox_state='cleanup_failed' RETURN NONE")
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
        let content = read_artifact(&state.db, run_id, user_id, artifact).await?;
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
    db: &jarvis_store::Database,
    run_id: uuid::Uuid,
    user_id: uuid::Uuid,
    artifact: jarvis_codex::CodingArtifactName,
) -> anyhow::Result<String> {
    if run_status(db, run_id, user_id).await? != "completed" {
        bail!("coding run has no completed artifact");
    }
    let path = Path::new(ARTIFACTS_ROOT)
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
    let _reconcile_guard = state.reconcile_lock.lock().await;
    if !state.cleanup_healthy.load(Ordering::SeqCst) {
        bail!("Codex sandbox cleanup requires recovery");
    }
    let sandbox = state
        .sandbox
        .as_ref()
        .context("Codex execution not owner-enabled")?;
    if !state.adapter.available() {
        bail!("reviewed subscription provider-only interface unavailable");
    }
    let slot = state
        .run_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| anyhow::anyhow!("Codex run capacity reached"))?;
    let (snapshot, context) = prepare_signed_run(&state.db, signed).await?;
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
    tokio::spawn(async move {
        run_worker(state, job).await;
    });
    Ok(run_id)
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
            let saved = save_artifacts(run_id, &result.take_artifact_contents());
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
                discard_artifacts(run_id);
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

fn save_artifacts(
    run_id: uuid::Uuid,
    artifacts: &[jarvis_sandbox::CollectedArtifact],
) -> anyhow::Result<()> {
    save_artifacts_in(Path::new(ARTIFACTS_ROOT), run_id, artifacts)
}

fn discard_artifacts(run_id: uuid::Uuid) {
    let dir = Path::new(ARTIFACTS_ROOT).join(run_id.to_string());
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
    use jarvis_sandbox::CollectedArtifact;

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
