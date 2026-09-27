//! Typed, local-only foundation for Codex engineering tasks (ADR-037).
//!
//! This crate deliberately does not spawn Codex, expose a listener, create a
//! workspace, or grant a tool. It models the small safe subset of the Codex App
//! Server JSON-RPC protocol that a later, policy-gated adapter may use.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

pub mod relay;
pub mod runtime;
pub mod snapshot;

pub const MAX_TASK_SUMMARY_CHARS: usize = 8_000;
pub const MAX_CHECKPOINT_CHARS: usize = 16_000;
pub const MAX_CODING_TIMEOUT_SECS: u64 = 15 * 60;
pub const MAX_CODING_ARTIFACTS: u8 = 32;
pub const MAX_CODING_OUTPUT_BYTES: u64 = 512 * 1024;
pub const MAX_TASK_ENVELOPE_BYTES: usize = 32 * 1024;
/// The OpenSandbox upload boundary accepts at most this many bytes per input.
/// A larger repository snapshot cannot be transferred through the reviewed
/// provider API and must fail before creating a workload.
pub const MAX_REPOSITORY_ARCHIVE_BYTES: usize = jarvis_sandbox::MAX_INPUT_BYTES;
pub const ACTION_CODING_START: &str = "coding.start";
pub const ACTION_CODING_RESUME: &str = "coding.resume";
pub const ACTION_CODEX_RUN_APPROVED_TASK: &str = "codex.run_approved_task";
pub const MAX_CAPABILITY_REQUESTS: usize = 4;

/// This is the only runtime command the trusted broker may pass to the Codex
/// image. It is server-owned; untrusted API input cannot supply a shell,
/// executable, host path, environment or image argument.
pub const CODEX_SANDBOX_COMMAND: [&str; 3] = [
    "/usr/local/bin/jarvis-codex-runtime",
    "run-approved-task",
    "/workspace/input/request.json",
];

/// The local broker protocol has a deliberately finite message set.  It is
/// transported over a Unix socket only; none of these operations are public
/// HTTP tools and there is no generic `exec` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrokerRequest {
    StartCodingRun {
        request: SignedCodingRequest,
    },
    ResumeCodingRun {
        request: SignedCodingRequest,
    },
    CancelCodingRun {
        run_id: Uuid,
        user_id: Uuid,
    },
    GetCodingRunStatus {
        run_id: Uuid,
        user_id: Uuid,
    },
    GetCodingArtifact {
        run_id: Uuid,
        user_id: Uuid,
        artifact: CodingArtifactName,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodingArtifactName {
    ResultJson,
    PatchDiff,
}

impl CodingArtifactName {
    pub fn file_name(self) -> &'static str {
        match self {
            Self::ResultJson => "result.json",
            Self::PatchDiff => "patch.diff",
        }
    }
}

impl BrokerRequest {
    pub fn signed_request(&self) -> Option<&SignedCodingRequest> {
        match self {
            Self::StartCodingRun { request } | Self::ResumeCodingRun { request } => Some(request),
            Self::CancelCodingRun { .. }
            | Self::GetCodingRunStatus { .. }
            | Self::GetCodingArtifact { .. } => None,
        }
    }

    pub fn validate_shape(&self) -> Result<(), CodingProtocolError> {
        match self {
            Self::StartCodingRun { request }
                if matches!(&request.operation, CodingOperation::StartCodingRun { .. }) =>
            {
                request.operation.validate()
            }
            Self::ResumeCodingRun { request }
                if matches!(&request.operation, CodingOperation::ResumeCodingRun { .. }) =>
            {
                request.operation.validate()
            }
            Self::CancelCodingRun { run_id, user_id }
            | Self::GetCodingRunStatus { run_id, user_id }
            | Self::GetCodingArtifact {
                run_id, user_id, ..
            } if !run_id.is_nil() && !user_id.is_nil() => Ok(()),
            _ => Err(CodingProtocolError::InvalidOperation),
        }
    }
}

/// The only API operation a sandbox Codex runtime may invoke on the trusted
/// broker. It is not an OpenAI proxy: no model, URL, arbitrary headers or
/// arbitrary request body can be selected by the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokeredCodexOperation {
    RunApprovedTask,
}

impl BrokeredCodexOperation {
    pub fn action(self) -> &'static str {
        ACTION_CODEX_RUN_APPROVED_TASK
    }
}

/// Claims recorded exclusively in the trusted broker. The sandbox sees an
/// opaque token, never an OpenAI/Codex credential or the long-lived broker
/// secret used to contact a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCapabilityClaims {
    pub run_id: Uuid,
    pub coding_session_id: Uuid,
    pub repository: RepositoryIdentity,
    pub base_commit_sha: String,
    pub expires_at: OffsetDateTime,
    pub budget_reservation_id: Uuid,
    pub budget_limit_cents: u64,
    pub operation: BrokeredCodexOperation,
}

impl RunCapabilityClaims {
    pub fn from_signed_request(
        run_id: Uuid,
        request: &SignedCodingRequest,
        budget_limit_cents: u64,
    ) -> Result<Self, CapabilityError> {
        request
            .operation
            .validate()
            .map_err(|_| CapabilityError::InvalidClaims)?;
        if budget_limit_cents == 0 {
            return Err(CapabilityError::InvalidClaims);
        }
        let (coding_session_id, repository, base_commit_sha, budget_reservation_id) =
            match &request.operation {
                CodingOperation::StartCodingRun {
                    coding_session_id,
                    repository,
                    base_commit_sha,
                    budget_reservation_id,
                    ..
                }
                | CodingOperation::ResumeCodingRun {
                    coding_session_id,
                    repository,
                    base_commit_sha,
                    budget_reservation_id,
                    ..
                } => (
                    *coding_session_id,
                    repository.clone(),
                    base_commit_sha.clone(),
                    *budget_reservation_id,
                ),
            };
        Ok(Self {
            run_id,
            coding_session_id,
            repository,
            base_commit_sha,
            expires_at: request.expires_at,
            budget_reservation_id,
            budget_limit_cents,
            operation: BrokeredCodexOperation::RunApprovedTask,
        })
    }
}

/// The opaque value copied to a disposable sandbox. It intentionally does not
/// implement Debug or Serialize, preventing accidental tracing or API output.
pub struct RunCapabilityToken(String);

impl RunCapabilityToken {
    fn random() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(hex::encode(bytes))
    }

    /// Bytes for the one generated sandbox input file. This is the sole place
    /// an ephemeral capability crosses the trust boundary.
    pub fn sandbox_input(&self) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "broker_operation": ACTION_CODEX_RUN_APPROVED_TASK,
            "capability_token": self.0,
        }))
        .expect("fixed capability input serializes")
    }
}

/// A request sent by the Codex runtime to the local broker API. All binding
/// fields are repeated and checked against stored claims; no caller supplied
/// path, command or provider/model selection exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokeredCodexRequest {
    pub request_id: Uuid,
    pub run_id: Uuid,
    pub coding_session_id: Uuid,
    pub repository: RepositoryIdentity,
    pub base_commit_sha: String,
    pub budget_reservation_id: Uuid,
    pub operation: BrokeredCodexOperation,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityError {
    #[error("invalid run capability claims")]
    InvalidClaims,
    #[error("run capability is unknown or invalid")]
    InvalidToken,
    #[error("run capability expired")]
    Expired,
    #[error("run capability was revoked")]
    Revoked,
    #[error("run capability request is not bound to this run")]
    WrongRun,
    #[error("run capability request is not bound to this repository")]
    WrongRepository,
    #[error("run capability operation is denied")]
    OperationDenied,
    #[error("run capability request was replayed")]
    Replay,
    #[error("run capability budget is exhausted")]
    BudgetExceeded,
}

struct StoredCapability {
    token_hash: [u8; 32],
    claims: RunCapabilityClaims,
    status: CapabilityStatus,
    used_request_ids: HashSet<Uuid>,
    used_cents: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapabilityStatus {
    Active,
    Completed,
    Cancelled,
}

/// Trusted broker-side in-memory capability authority. A broker restart loses
/// the authority and thus revokes all outstanding tokens by design. Production
/// durable run state may be added later, but must never recreate token secrets.
#[derive(Default)]
pub struct RunCapabilityAuthority {
    capabilities: Mutex<HashMap<[u8; 32], StoredCapability>>,
}

impl RunCapabilityAuthority {
    pub fn mint(&self, claims: RunCapabilityClaims) -> Result<RunCapabilityToken, CapabilityError> {
        claims
            .repository
            .validate()
            .map_err(|_| CapabilityError::InvalidClaims)?;
        if !is_commit_sha(&claims.base_commit_sha) || claims.expires_at <= OffsetDateTime::now_utc()
        {
            return Err(CapabilityError::InvalidClaims);
        }
        let token = RunCapabilityToken::random();
        let hash: [u8; 32] = Sha256::digest(token.0.as_bytes()).into();
        let mut capabilities = self
            .capabilities
            .lock()
            .map_err(|_| CapabilityError::Revoked)?;
        capabilities.insert(
            hash,
            StoredCapability {
                token_hash: hash,
                claims,
                status: CapabilityStatus::Active,
                used_request_ids: HashSet::new(),
                used_cents: 0,
            },
        );
        Ok(token)
    }

    /// Authorize a single narrow runtime request and atomically reserve its
    /// budget. Replaying the same request ID, changing any binding, exceeding
    /// the budget or using a cancelled/completed token fails closed.
    pub fn authorize(
        &self,
        token: &RunCapabilityToken,
        request: &BrokeredCodexRequest,
        reserve_cents: u64,
        now: OffsetDateTime,
    ) -> Result<(), CapabilityError> {
        self.authorize_raw(&token.0, request, reserve_cents, now)
    }

    /// Validate raw token text received from the sandbox narrow broker API.
    /// The text is hashed immediately and is never stored, logged or returned.
    pub fn authorize_raw(
        &self,
        token: &str,
        request: &BrokeredCodexRequest,
        reserve_cents: u64,
        now: OffsetDateTime,
    ) -> Result<(), CapabilityError> {
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CapabilityError::InvalidToken);
        }
        let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut capabilities = self
            .capabilities
            .lock()
            .map_err(|_| CapabilityError::Revoked)?;
        let Some(stored) = capabilities.get_mut(&hash) else {
            return Err(CapabilityError::InvalidToken);
        };
        if stored.token_hash.ct_eq(&hash).unwrap_u8() != 1 {
            return Err(CapabilityError::InvalidToken);
        }
        if stored.status != CapabilityStatus::Active {
            return Err(CapabilityError::Revoked);
        }
        if now >= stored.claims.expires_at {
            return Err(CapabilityError::Expired);
        }
        if request.run_id != stored.claims.run_id
            || request.coding_session_id != stored.claims.coding_session_id
            || request.budget_reservation_id != stored.claims.budget_reservation_id
        {
            return Err(CapabilityError::WrongRun);
        }
        if request.repository != stored.claims.repository
            || request.base_commit_sha != stored.claims.base_commit_sha
        {
            return Err(CapabilityError::WrongRepository);
        }
        if request.operation != stored.claims.operation {
            return Err(CapabilityError::OperationDenied);
        }
        if stored.used_request_ids.contains(&request.request_id)
            || stored.used_request_ids.len() >= MAX_CAPABILITY_REQUESTS
        {
            return Err(CapabilityError::Replay);
        }
        if reserve_cents == 0
            || stored.used_cents.saturating_add(reserve_cents) > stored.claims.budget_limit_cents
        {
            return Err(CapabilityError::BudgetExceeded);
        }
        stored.used_request_ids.insert(request.request_id);
        stored.used_cents = stored.used_cents.saturating_add(reserve_cents);
        Ok(())
    }

    pub fn revoke_for_cancel(&self, token: &RunCapabilityToken) {
        self.set_status(token, CapabilityStatus::Cancelled);
    }
    pub fn revoke_for_completion(&self, token: &RunCapabilityToken) {
        self.set_status(token, CapabilityStatus::Completed);
    }

    fn set_status(&self, token: &RunCapabilityToken, status: CapabilityStatus) {
        let hash: [u8; 32] = Sha256::digest(token.0.as_bytes()).into();
        if let Ok(mut capabilities) = self.capabilities.lock() {
            if let Some(stored) = capabilities.get_mut(&hash) {
                stored.status = status;
            }
        }
    }

    /// Cancellation/completion are driven by the trusted run registry, which
    /// has the run ID but never needs to retain or re-read token plaintext.
    pub fn revoke_run(&self, run_id: Uuid, completed: bool) {
        if let Ok(mut capabilities) = self.capabilities.lock() {
            for capability in capabilities
                .values_mut()
                .filter(|item| item.claims.run_id == run_id)
            {
                capability.status = if completed {
                    CapabilityStatus::Completed
                } else {
                    CapabilityStatus::Cancelled
                };
            }
        }
    }
}

/// Repository bytes are copied into the disposable sandbox.  This type has no
/// host path, bind mount or live checkout reference: a trusted broker obtains
/// the archive from its server-owned repository registry after checking the
/// requested base commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySnapshot {
    pub base_commit_sha: String,
    pub archive: Vec<u8>,
}

/// Fixed sandbox input produced by the trusted broker after signed approval.
/// The sandbox never receives the device signature or a host path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxRunInput {
    pub request_id: Uuid,
    pub run_id: Uuid,
    pub coding_session_id: Uuid,
    pub repository: RepositoryIdentity,
    pub base_commit_sha: String,
    pub snapshot_sha256: String,
    pub budget_reservation_id: Uuid,
    pub operation: BrokeredCodexOperation,
    pub max_output_bytes: u64,
    pub timeout_secs: u64,
}

impl SandboxRunInput {
    pub fn from_approved(
        request: &SignedCodingRequest,
        run_id: Uuid,
        snapshot: &RepositorySnapshot,
    ) -> Result<Self, CodingProtocolError> {
        request.operation.validate()?;
        snapshot
            .validate_for(&request.operation)
            .map_err(|_| CodingProtocolError::InvalidOperation)?;
        let (
            coding_session_id,
            repository,
            base_commit_sha,
            budget_reservation_id,
            max_output_bytes,
            timeout_secs,
        ) = match &request.operation {
            CodingOperation::StartCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                budget_reservation_id,
                max_output_bytes,
                timeout_secs,
                ..
            }
            | CodingOperation::ResumeCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                budget_reservation_id,
                max_output_bytes,
                timeout_secs,
                ..
            } => (
                *coding_session_id,
                repository.clone(),
                base_commit_sha.clone(),
                *budget_reservation_id,
                *max_output_bytes,
                *timeout_secs,
            ),
        };
        Ok(Self {
            request_id: request.request_id,
            run_id,
            coding_session_id,
            repository,
            base_commit_sha,
            snapshot_sha256: snapshot.sha256_hex(),
            budget_reservation_id,
            operation: BrokeredCodexOperation::RunApprovedTask,
            max_output_bytes,
            timeout_secs,
        })
    }

    pub fn validate_with_context(&self, context: &TaskEnvelope) -> Result<(), CodingProtocolError> {
        if self.run_id.is_nil()
            || self.request_id.is_nil()
            || self.coding_session_id.is_nil()
            || self.budget_reservation_id.is_nil()
            || self.repository != context.repository
            || self.base_commit_sha != context.base_commit_sha
            || self.coding_session_id != context.coding_session_id
            || self.timeout_secs != context.timeout_secs
            || self.max_output_bytes != context.max_output_bytes
            || !is_commit_sha(&self.base_commit_sha)
            || self.snapshot_sha256.len() != 64
            || !self.snapshot_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.operation != BrokeredCodexOperation::RunApprovedTask
        {
            return Err(CodingProtocolError::InvalidOperation);
        }
        Ok(())
    }

    pub fn brokered_request(&self) -> BrokeredCodexRequest {
        BrokeredCodexRequest {
            request_id: self.request_id,
            run_id: self.run_id,
            coding_session_id: self.coding_session_id,
            repository: self.repository.clone(),
            base_commit_sha: self.base_commit_sha.clone(),
            budget_reservation_id: self.budget_reservation_id,
            operation: self.operation,
        }
    }
}

impl RepositorySnapshot {
    pub fn validate_for(&self, operation: &CodingOperation) -> Result<(), CodingRunError> {
        let expected = match operation {
            CodingOperation::StartCodingRun {
                base_commit_sha, ..
            }
            | CodingOperation::ResumeCodingRun {
                base_commit_sha, ..
            } => base_commit_sha,
        };
        if self.base_commit_sha != *expected
            || self.archive.is_empty()
            || self.archive.len() > MAX_REPOSITORY_ARCHIVE_BYTES
        {
            return Err(CodingRunError::RepositoryIsolationUnavailable);
        }
        Ok(())
    }
}

/// Safe, bounded facts returned to the trusted broker.  No hidden reasoning,
/// raw unlimited output or host path crosses this boundary.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingRunResult {
    pub run_id: Uuid,
    pub coding_session_id: Uuid,
    pub status: String,
    pub base_commit_sha: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed_ms: u64,
    pub stdout_summary: String,
    pub stderr_summary: String,
    pub artifacts: Vec<String>,
    pub termination_reason: String,
    #[serde(skip)]
    artifact_contents: Vec<jarvis_sandbox::CollectedArtifact>,
}

impl std::fmt::Debug for CodingRunResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodingRunResult")
            .field("run_id", &self.run_id)
            .field("coding_session_id", &self.coding_session_id)
            .field("status", &self.status)
            .field("artifacts", &self.artifacts)
            .finish_non_exhaustive()
    }
}

impl CodingRunResult {
    pub fn take_artifact_contents(&mut self) -> Vec<jarvis_sandbox::CollectedArtifact> {
        std::mem::take(&mut self.artifact_contents)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodingRunError {
    #[error("sandbox execution is unavailable")]
    SandboxUnavailable,
    #[error("secure Codex authentication is unavailable")]
    CodexAuthenticationUnavailable,
    #[error("repository isolation is unavailable")]
    RepositoryIsolationUnavailable,
    #[error("sandbox execution failed")]
    SandboxFailed,
    #[error("sandbox execution timed out")]
    TimedOut,
}

/// Run exactly one approved coding task in a disposable OpenSandbox workload.
/// This has no host-process branch: an unavailable provider, missing broker
/// capability, invalid snapshot or provider error fails before or during the
/// sandbox lifecycle and never invokes a local Codex CLI.
pub struct ApprovedSandboxRun<'a, A: relay::CodexSubscriptionAdapter + ?Sized> {
    pub adapter: &'a A,
    pub authority: &'a RunCapabilityAuthority,
    pub run_id: Uuid,
    pub capability: Option<RunCapabilityToken>,
}

pub async fn execute_in_sandbox<
    P: jarvis_sandbox::SandboxProvider,
    A: relay::CodexSubscriptionAdapter + ?Sized,
>(
    provider: &P,
    request: &SignedCodingRequest,
    snapshot: RepositorySnapshot,
    context: TaskContextInput,
    gate: ApprovedSandboxRun<'_, A>,
) -> Result<CodingRunResult, CodingRunError> {
    execute_in_sandbox_with_cancel(provider, request, snapshot, context, gate, None).await
}

/// The caller must keep this future alive until it returns. Cancellation is a
/// signal, not task abortion, so sandbox teardown and capability revocation run
/// on the same controlled path as success and failure.
pub async fn execute_in_sandbox_with_cancel<
    P: jarvis_sandbox::SandboxProvider,
    A: relay::CodexSubscriptionAdapter + ?Sized,
>(
    provider: &P,
    request: &SignedCodingRequest,
    snapshot: RepositorySnapshot,
    context: TaskContextInput,
    gate: ApprovedSandboxRun<'_, A>,
    mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<CodingRunResult, CodingRunError> {
    request
        .operation
        .validate()
        .map_err(|_| CodingRunError::SandboxFailed)?;
    let task_context = request
        .operation
        .compile_task_envelope(context)
        .map_err(|_| CodingRunError::SandboxFailed)?;
    snapshot.validate_for(&request.operation)?;
    snapshot::validate_archive(&snapshot.archive, &snapshot.base_commit_sha)
        .map_err(|_| CodingRunError::RepositoryIsolationUnavailable)?;
    let capability = gate
        .capability
        .ok_or(CodingRunError::CodexAuthenticationUnavailable)?;
    if !gate.adapter.available() {
        return Err(CodingRunError::CodexAuthenticationUnavailable);
    }
    if !matches!(
        provider.availability().await,
        jarvis_sandbox::SandboxAvailability::Available
    ) {
        return Err(CodingRunError::SandboxUnavailable);
    }
    let mut task = request
        .operation
        .sandbox_task()
        .map_err(|_| CodingRunError::SandboxFailed)?;
    task.task_id = gate.run_id;
    let sandbox_input = SandboxRunInput::from_approved(request, gate.run_id, &snapshot)
        .map_err(|_| CodingRunError::SandboxFailed)?;
    let handle = provider
        .create(&task)
        .await
        .map_err(|_| CodingRunError::SandboxUnavailable)?;
    // The signed limit bounds upload, relay, provider execution and artifact
    // collection together. A stalled manager cannot stretch a 1-second owner
    // approval into the profile's much larger infrastructure ceiling.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(sandbox_input.timeout_secs),
        async {
        provider
            .set_network_policy(&handle, &task.network_policy)
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let request_json =
            serde_json::to_vec(&sandbox_input).map_err(|_| CodingRunError::SandboxFailed)?;
        provider
            .upload(
                &handle,
                jarvis_sandbox::TaskInput {
                    name: "request.json".into(),
                    bytes: request_json,
                },
            )
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        provider
            .upload(
                &handle,
                jarvis_sandbox::TaskInput {
                    name: "task-context.json".into(),
                    bytes: serde_json::to_vec(&task_context)
                        .map_err(|_| CodingRunError::SandboxFailed)?,
                },
            )
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        provider
            .upload(
                &handle,
                jarvis_sandbox::TaskInput {
                    name: "repository.tar".into(),
                    bytes: snapshot.archive,
                },
            )
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        provider
            .upload(
                &handle,
                jarvis_sandbox::TaskInput {
                    name: "codex-capability.json".into(),
                    bytes: capability.sandbox_input(),
                },
            )
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let execution_future = provider.exec(&handle, &task.command);
        let relay_future = relay::relay_once(
            provider,
            gate.adapter,
            &handle,
            gate.authority,
            &sandbox_input,
            &task_context,
        );
        tokio::pin!(execution_future);
        tokio::pin!(relay_future);
        let execution_first = tokio::select! {
            biased;
            _ = async {
                if let Some(receiver) = cancel.as_mut() {
                    if !*receiver.borrow() {
                        let _ = receiver.changed().await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            } => return Err(CodingRunError::SandboxFailed),
            relay_result = &mut relay_future => { relay_result?; None },
            execution = &mut execution_future => {
                let execution = execution.map_err(|_| CodingRunError::SandboxFailed)?;
                if execution.timed_out { return Err(CodingRunError::TimedOut); }
                if execution.exit_code != Some(0) { return Err(CodingRunError::SandboxFailed); }
                Some(execution)
            },
        };
        let execution = if let Some(execution) = execution_first {
            // The workload can read the response and exit before the manager
            // acknowledges its upload. Do not turn that ordering into a false
            // failure; the relay still must finish successfully.
            relay_future.await?;
            execution
        } else {
            tokio::select! {
                biased;
                _ = async {
                    if let Some(receiver) = cancel.as_mut() {
                        if !*receiver.borrow() {
                            let _ = receiver.changed().await;
                        }
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => return Err(CodingRunError::SandboxFailed),
                execution = &mut execution_future => execution.map_err(|_| CodingRunError::SandboxFailed)?,
            }
        };
        if execution.timed_out {
            return Err(CodingRunError::TimedOut);
        }
        if execution.exit_code != Some(0) {
            return Err(CodingRunError::SandboxFailed);
        }
        let artifacts = provider
            .collect_artifacts(&handle, &["result.json".into(), "patch.diff".into()])
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let final_result = runtime::validate_final_artifacts(&artifacts, &sandbox_input)
            .map_err(|_| CodingRunError::SandboxFailed)?;
        Ok(CodingRunResult {
            run_id: gate.run_id,
            coding_session_id: sandbox_input.coding_session_id,
            status: match final_result.outcome {
                runtime::TaskOutcome::Completed => "completed",
                runtime::TaskOutcome::PlanLimit => "plan_limit",
                runtime::TaskOutcome::SubscriptionUnavailable => "subscription_unavailable",
                runtime::TaskOutcome::RuntimeFailure => "failed",
            }
            .into(),
            base_commit_sha: sandbox_input.base_commit_sha.clone(),
            exit_code: execution.exit_code,
            timed_out: false,
            elapsed_ms: execution.duration_ms,
            stdout_summary: final_result.summary,
            stderr_summary: String::new(),
            artifacts: artifacts
                .iter()
                .map(|artifact| artifact.path.clone())
                .collect(),
            termination_reason: "completed".into(),
            artifact_contents: artifacts,
        })
        },
    )
    .await
    .unwrap_or(Err(CodingRunError::TimedOut));
    // Destruction is mandatory for success, failure and cancellation paths.
    // There is no retained sandbox to resume later.
    let terminated = provider.terminate(handle).await;
    gate.authority.revoke_run(gate.run_id, result.is_ok());
    if terminated.is_err() {
        return Err(CodingRunError::SandboxFailed);
    }
    result
}

/// A root-managed registry resolves this logical identity to a trusted source
/// snapshot. It is deliberately not a Git URL or local filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIdentity {
    pub id: String,
    pub owner: String,
    pub name: String,
}

impl RepositoryIdentity {
    pub fn validate(&self) -> Result<(), CodingProtocolError> {
        if !is_safe_identifier(&self.id, 96)
            || !is_safe_identifier(&self.owner, 96)
            || !is_safe_identifier(&self.name, 96)
        {
            return Err(CodingProtocolError::InvalidOperation);
        }
        Ok(())
    }
}

/// The complete allowlist exposed by the local coding broker. It intentionally
/// has no arbitrary process, environment, container, host path or network
/// fields. Start/resume are device-signed operations; cancel/status use
/// separate typed messages and never become generic execution primitives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodingOperation {
    StartCodingRun {
        coding_session_id: Uuid,
        repository: RepositoryIdentity,
        base_commit_sha: String,
        worktree_label: Option<String>,
        objective: String,
        checkpoint: Option<CodingCheckpoint>,
        timeout_secs: u64,
        budget_reservation_id: Uuid,
        max_artifacts: u8,
        max_output_bytes: u64,
    },
    ResumeCodingRun {
        coding_session_id: Uuid,
        repository: RepositoryIdentity,
        base_commit_sha: String,
        checkpoint: CodingCheckpoint,
        /// A newly approved owner instruction, never reconstructed from chat history.
        owner_delta: Option<String>,
        timeout_secs: u64,
        budget_reservation_id: Uuid,
        max_artifacts: u8,
        max_output_bytes: u64,
    },
}

impl CodingOperation {
    pub fn action(&self) -> &'static str {
        match self {
            Self::StartCodingRun { .. } => ACTION_CODING_START,
            Self::ResumeCodingRun { .. } => ACTION_CODING_RESUME,
        }
    }

    pub fn validate(&self) -> Result<(), CodingProtocolError> {
        let (repository, base, objective, timeout, artifacts, output) = match self {
            Self::StartCodingRun {
                repository,
                base_commit_sha,
                objective,
                worktree_label,
                checkpoint,
                timeout_secs,
                max_artifacts,
                max_output_bytes,
                ..
            } => {
                if worktree_label
                    .as_deref()
                    .is_some_and(|value| !is_safe_identifier(value, 96))
                {
                    return Err(CodingProtocolError::InvalidOperation);
                }
                if let Some(checkpoint) = checkpoint {
                    checkpoint
                        .validate()
                        .map_err(|_| CodingProtocolError::InvalidOperation)?;
                }
                (
                    repository,
                    base_commit_sha,
                    objective,
                    timeout_secs,
                    max_artifacts,
                    max_output_bytes,
                )
            }
            Self::ResumeCodingRun {
                repository,
                base_commit_sha,
                checkpoint,
                owner_delta,
                timeout_secs,
                max_artifacts,
                max_output_bytes,
                ..
            } => {
                checkpoint
                    .validate()
                    .map_err(|_| CodingProtocolError::InvalidOperation)?;
                if owner_delta
                    .as_deref()
                    .is_some_and(|delta| delta.trim().is_empty() || delta.chars().count() > 2_048)
                {
                    return Err(CodingProtocolError::InvalidOperation);
                }
                (
                    repository,
                    base_commit_sha,
                    &checkpoint.summary,
                    timeout_secs,
                    max_artifacts,
                    max_output_bytes,
                )
            }
        };
        repository.validate()?;
        if !is_commit_sha(base)
            || objective.trim().is_empty()
            || objective.chars().count() > MAX_TASK_SUMMARY_CHARS
            || *timeout == 0
            || *timeout > MAX_CODING_TIMEOUT_SECS
            // The fixed runtime emits result.json and patch.diff (which may
            // be empty), so a smaller signed ceiling cannot be honored.
            || *artifacts != 2
            || *output == 0
            || *output > MAX_CODING_OUTPUT_BYTES
        {
            return Err(CodingProtocolError::InvalidOperation);
        }
        Ok(())
    }

    pub fn canonical_payload(&self) -> Result<Vec<u8>, CodingProtocolError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|_| CodingProtocolError::InvalidOperation)
    }

    /// Bind approval to exactly the selected logical repository and base commit.
    pub fn target_state_hash(&self) -> Result<[u8; 32], CodingProtocolError> {
        self.validate()?;
        let (repository, base) = match self {
            Self::StartCodingRun {
                repository,
                base_commit_sha,
                ..
            }
            | Self::ResumeCodingRun {
                repository,
                base_commit_sha,
                ..
            } => (repository, base_commit_sha),
        };
        let mut bytes = Vec::new();
        for value in [&repository.id, &repository.owner, &repository.name, base] {
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        Ok(Sha256::digest(bytes).into())
    }

    /// Form a profile-owned task for OpenSandbox. A broker must use this exact
    /// method rather than accept a command from Core or a client.
    pub fn sandbox_task(&self) -> Result<jarvis_sandbox::SandboxTask, CodingProtocolError> {
        self.validate()?;
        jarvis_sandbox::SandboxTask::new(
            jarvis_sandbox::SandboxProfile::Codex,
            CODEX_SANDBOX_COMMAND
                .iter()
                .map(|part| (*part).to_string())
                .collect(),
        )
        .map_err(|_| CodingProtocolError::InvalidOperation)
    }
}

/// The exact approval envelope passed to the local broker. The broker must
/// independently validate the active device signature and consume request_id
/// exactly once; ordinary bearer authentication is only an API transport gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCodingRequest {
    pub request_id: Uuid,
    pub nonce_hex: String,
    pub user_id: Uuid,
    pub device_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub operation: CodingOperation,
    pub signature_hex: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodingProtocolError {
    #[error("invalid coding operation")]
    InvalidOperation,
    #[error("invalid coding approval")]
    InvalidApproval,
    #[error("coding approval expired")]
    Expired,
}

impl SignedCodingRequest {
    pub fn message(&self) -> Result<Vec<u8>, CodingProtocolError> {
        let nonce: [u8; 32] = hex::decode(&self.nonce_hex)
            .map_err(|_| CodingProtocolError::InvalidApproval)?
            .try_into()
            .map_err(|_| CodingProtocolError::InvalidApproval)?;
        let signature =
            hex::decode(&self.signature_hex).map_err(|_| CodingProtocolError::InvalidApproval)?;
        if signature.len() != 64
            || self.expires_at <= self.issued_at
            || self.expires_at - self.issued_at > time::Duration::minutes(5)
        {
            return Err(CodingProtocolError::InvalidApproval);
        }
        let payload_hash: [u8; 32] = Sha256::digest(self.operation.canonical_payload()?).into();
        let state = self.operation.target_state_hash()?;
        jarvis_identity::codex_coding_approval_message(
            self.operation.action(),
            &payload_hash,
            self.request_id,
            &nonce,
            self.user_id,
            self.device_id,
            self.issued_at,
            self.expires_at,
            &state,
        )
        .map_err(|_| CodingProtocolError::InvalidApproval)
    }

    pub fn reject_if_expired(&self, now: OffsetDateTime) -> Result<(), CodingProtocolError> {
        if now > self.expires_at || now < self.issued_at - time::Duration::seconds(30) {
            Err(CodingProtocolError::Expired)
        } else {
            Ok(())
        }
    }
}

fn is_safe_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_commit_sha(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The authoritative policy decision for requesting Codex engineering work.
///
/// This is intentionally an adapter over `jarvis-policy`, not a Codex-specific
/// risk rule. A future API route must still bind a `RequireApproval` result to a
/// real, device-signed pending action immediately before starting any process.
pub fn request_policy(trusted_device: bool) -> jarvis_policy::PolicyDecision {
    jarvis_policy::decide(&jarvis_policy::PolicyContext {
        capability: jarvis_policy::Capability::ExecuteCode,
        risk: jarvis_policy::RiskClass::Mutating,
        trusted_device,
        approved: false,
        reversible: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Starting,
    Running,
    Cancelling,
    Completed,
    Failed,
    TimedOut,
    Cancelled,
}

/// Durable logical state for a coding objective. A session is not a container:
/// every resume starts a new sandbox run from this factual checkpoint and the
/// current worktree/revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Suspended,
    Completed,
    Cancelled,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodingCheckpoint {
    pub summary: String,
    pub decisions: Vec<String>,
    pub pending: Vec<String>,
    pub tests: String,
}

/// Provider-neutral, bounded context for a delegated coding task. This is a
/// projection of an already signed operation, not a new authorization surface.
/// In particular, file references are logical identifiers, never host paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskEnvelope {
    pub intent: String,
    pub objective: String,
    pub owner_constraints: Vec<String>,
    pub recent_deltas: Vec<String>,
    pub selected_facts: Vec<ProvenancedFact>,
    pub repository: RepositoryIdentity,
    pub base_commit_sha: String,
    pub coding_session_id: Uuid,
    pub checkpoint: Option<CodingCheckpoint>,
    pub timeout_secs: u64,
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenancedFact {
    pub fact: String,
    pub source: String,
}

/// Optional relevant material supplied by a trusted compiler caller. Raw
/// history is never automatically loaded; the caller selects deltas/facts.
#[derive(Default)]
pub struct TaskContextInput {
    /// Loaded from the owner-scoped, durable coding session for resume. A
    /// checkpoint summary must never replace the original owner objective.
    pub original_objective: Option<String>,
    pub owner_constraints: Vec<String>,
    pub recent_deltas: Vec<String>,
    pub selected_facts: Vec<ProvenancedFact>,
}

impl CodingOperation {
    pub fn compile_task_envelope(
        &self,
        input: TaskContextInput,
    ) -> Result<TaskEnvelope, CodingProtocolError> {
        self.validate()?;
        if input.original_objective.as_deref().is_some_and(|value| {
            value.trim().is_empty() || value.chars().count() > MAX_TASK_SUMMARY_CHARS
        }) {
            return Err(CodingProtocolError::InvalidOperation);
        }
        if input.owner_constraints.len() > 8
            || input
                .owner_constraints
                .iter()
                .any(|value| value.trim().is_empty() || value.chars().count() > 1_000)
            || input.selected_facts.len() > 16
            || input.selected_facts.iter().any(|value| {
                value.fact.trim().is_empty()
                    || value.fact.chars().count() > 1_000
                    || !is_safe_identifier(&value.source, 128)
            })
        {
            return Err(CodingProtocolError::InvalidOperation);
        }
        let (
            coding_session_id,
            repository,
            base_commit_sha,
            objective,
            checkpoint,
            timeout_secs,
            max_output_bytes,
        ) = match self {
            Self::StartCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                objective,
                checkpoint,
                timeout_secs,
                max_output_bytes,
                ..
            } => (
                *coding_session_id,
                repository.clone(),
                base_commit_sha.clone(),
                if input.original_objective.is_some() {
                    return Err(CodingProtocolError::InvalidOperation);
                } else {
                    objective.clone()
                },
                checkpoint.clone(),
                *timeout_secs,
                *max_output_bytes,
            ),
            Self::ResumeCodingRun {
                coding_session_id,
                repository,
                base_commit_sha,
                checkpoint,
                timeout_secs,
                max_output_bytes,
                ..
            } => (
                *coding_session_id,
                repository.clone(),
                base_commit_sha.clone(),
                input
                    .original_objective
                    .clone()
                    .ok_or(CodingProtocolError::InvalidOperation)?,
                Some(checkpoint.clone()),
                *timeout_secs,
                *max_output_bytes,
            ),
        };
        let mut envelope = TaskEnvelope {
            intent: "coding".into(),
            objective,
            owner_constraints: input.owner_constraints,
            recent_deltas: input
                .recent_deltas
                .into_iter()
                .rev()
                .take(8)
                .map(|delta| {
                    delta
                        .chars()
                        .rev()
                        .take(2_048)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect()
                })
                .collect(),
            selected_facts: input.selected_facts,
            repository,
            base_commit_sha,
            coding_session_id,
            checkpoint,
            timeout_secs,
            max_output_bytes,
        };
        envelope.recent_deltas.reverse();
        // Drop raw deltas first. Never silently drop explicit owner constraints
        // or the latest factual checkpoint merely to fit the worker budget.
        while serde_json::to_vec(&envelope)
            .map_err(|_| CodingProtocolError::InvalidOperation)?
            .len()
            > MAX_TASK_ENVELOPE_BYTES
        {
            if !envelope.recent_deltas.is_empty() {
                envelope.recent_deltas.remove(0);
            } else if !envelope.selected_facts.is_empty() {
                envelope.selected_facts.remove(0);
            } else {
                return Err(CodingProtocolError::InvalidOperation);
            }
        }
        Ok(envelope)
    }
}

impl CodingCheckpoint {
    pub fn validate(&self) -> Result<(), TaskError> {
        if self.summary.trim().is_empty()
            || self.summary.chars().count() > MAX_CHECKPOINT_CHARS
            || self.tests.chars().count() > 4_000
            || self.decisions.len() > 32
            || self.pending.len() > 32
            || self
                .decisions
                .iter()
                .chain(&self.pending)
                .any(|v| v.trim().is_empty() || v.chars().count() > 1_000)
        {
            return Err(TaskError::InvalidCheckpoint);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodingSession {
    pub id: Uuid,
    pub repository: String,
    pub base_revision: String,
    pub objective: String,
    pub state: SessionState,
    pub checkpoint: Option<CodingCheckpoint>,
}

impl CodingSession {
    pub fn new(
        repository: impl Into<String>,
        base_revision: impl Into<String>,
        objective: impl Into<String>,
    ) -> Result<Self, TaskError> {
        let repository = repository.into();
        let base_revision = base_revision.into();
        let objective = objective.into();
        if repository.trim().is_empty()
            || repository.chars().count() > 512
            || base_revision.len() > 128
            || objective.trim().is_empty()
            || objective.chars().count() > MAX_TASK_SUMMARY_CHARS
        {
            return Err(TaskError::InvalidSummary);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            repository,
            base_revision,
            objective,
            state: SessionState::Active,
            checkpoint: None,
        })
    }
    pub fn checkpoint(&mut self, checkpoint: CodingCheckpoint) -> Result<(), TaskError> {
        checkpoint.validate()?;
        if self.state != SessionState::Active {
            return Err(TaskError::InvalidSessionTransition);
        }
        self.checkpoint = Some(checkpoint);
        Ok(())
    }
    pub fn transition(&mut self, state: SessionState) -> Result<(), TaskError> {
        if !matches!(
            (self.state, state),
            (
                SessionState::Active,
                SessionState::Suspended | SessionState::Completed | SessionState::Cancelled
            ) | (
                SessionState::Suspended,
                SessionState::Active | SessionState::Cancelled | SessionState::Archived
            ) | (
                SessionState::Completed | SessionState::Cancelled,
                SessionState::Archived
            )
        ) {
            return Err(TaskError::InvalidSessionTransition);
        }
        self.state = state;
        Ok(())
    }
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineeringTask {
    pub id: Uuid,
    pub summary: String,
    pub state: TaskState,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub deadline: OffsetDateTime,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskError {
    #[error("task summary must be between 1 and {MAX_TASK_SUMMARY_CHARS} characters")]
    InvalidSummary,
    #[error("task deadline must be after creation")]
    InvalidDeadline,
    #[error("invalid task transition from {from:?} to {to:?}")]
    InvalidTransition { from: TaskState, to: TaskState },
    #[error("coding checkpoint is invalid or exceeds its bounded factual format")]
    InvalidCheckpoint,
    #[error("invalid coding session transition")]
    InvalidSessionTransition,
}

impl EngineeringTask {
    pub fn new(
        summary: impl Into<String>,
        created_at: OffsetDateTime,
        deadline: OffsetDateTime,
    ) -> Result<Self, TaskError> {
        let summary = summary.into();
        if summary.trim().is_empty() || summary.chars().count() > MAX_TASK_SUMMARY_CHARS {
            return Err(TaskError::InvalidSummary);
        }
        if deadline <= created_at {
            return Err(TaskError::InvalidDeadline);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            summary,
            state: TaskState::Queued,
            created_at,
            deadline,
        })
    }

    pub fn transition(&mut self, to: TaskState) -> Result<(), TaskError> {
        let allowed = matches!(
            (self.state, to),
            (TaskState::Queued, TaskState::Starting)
                | (TaskState::Queued, TaskState::Cancelled)
                | (TaskState::Starting, TaskState::Running)
                | (TaskState::Starting, TaskState::Failed)
                | (TaskState::Starting, TaskState::TimedOut)
                | (TaskState::Starting, TaskState::Cancelling)
                | (TaskState::Running, TaskState::Completed)
                | (TaskState::Running, TaskState::Failed)
                | (TaskState::Running, TaskState::TimedOut)
                | (TaskState::Running, TaskState::Cancelling)
                | (TaskState::Cancelling, TaskState::Cancelled)
                | (TaskState::Cancelling, TaskState::Failed)
                | (TaskState::Cancelling, TaskState::TimedOut)
        );
        if !allowed {
            return Err(TaskError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.state = to;
        Ok(())
    }
}

/// The only App Server requests Jarvis may form in phase 1. In particular this
/// intentionally excludes `thread/shellCommand`, `command/exec` and
/// `process/spawn`, which can escape an engineering task's future sandbox.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum AppServerRequest {
    #[serde(rename = "initialize")]
    Initialize {
        client_name: String,
        client_version: String,
    },
    #[serde(rename = "thread/start")]
    StartThread { model: Option<String> },
    #[serde(rename = "turn/start")]
    StartTurn { thread_id: String, input: String },
    #[serde(rename = "turn/interrupt")]
    InterruptTurn { thread_id: String, turn_id: String },
}

impl AppServerRequest {
    /// Encode one newline-delimited JSON-RPC request for the local stdio transport.
    pub fn json_rpc(&self, id: u64) -> Value {
        match self {
            Self::Initialize {
                client_name,
                client_version,
            } => json!({
                "method": "initialize", "id": id,
                "params": { "clientInfo": { "name": client_name, "title": "Jarvis Core", "version": client_version } }
            }),
            Self::StartThread { model } => {
                json!({ "method": "thread/start", "id": id, "params": { "model": model } })
            }
            Self::StartTurn { thread_id, input } => json!({
                "method": "turn/start", "id": id,
                "params": { "threadId": thread_id, "input": [{ "type": "text", "text": input }] }
            }),
            Self::InterruptTurn { thread_id, turn_id } => json!({
                "method": "turn/interrupt", "id": id,
                "params": { "threadId": thread_id, "turnId": turn_id }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use time::Duration;

    fn task() -> EngineeringTask {
        let now = OffsetDateTime::now_utc();
        EngineeringTask::new(
            "Inspect a bounded development worktree",
            now,
            now + Duration::minutes(5),
        )
        .unwrap()
    }

    #[test]
    fn lifecycle_allows_cancellation_but_not_resurrection() {
        let mut task = task();
        task.transition(TaskState::Starting).unwrap();
        task.transition(TaskState::Running).unwrap();
        task.transition(TaskState::Cancelling).unwrap();
        task.transition(TaskState::Cancelled).unwrap();
        assert!(task.state.is_terminal());
        assert_eq!(
            task.transition(TaskState::Running),
            Err(TaskError::InvalidTransition {
                from: TaskState::Cancelled,
                to: TaskState::Running
            })
        );
    }

    #[test]
    fn task_bounds_are_fail_closed() {
        let now = OffsetDateTime::now_utc();
        assert_eq!(
            EngineeringTask::new(" ", now, now + Duration::seconds(1)),
            Err(TaskError::InvalidSummary)
        );
        assert_eq!(
            EngineeringTask::new("x", now, now),
            Err(TaskError::InvalidDeadline)
        );
    }

    #[test]
    fn protocol_is_allowlisted_and_structured() {
        let request = AppServerRequest::StartTurn {
            thread_id: "thr_123".into(),
            input: "inspect only".into(),
        };
        let message = request.json_rpc(7);
        assert_eq!(message["method"], "turn/start");
        assert_eq!(message["params"]["threadId"], "thr_123");
        assert_eq!(message["params"]["input"][0]["text"], "inspect only");
        assert!(message.get("command").is_none());
    }

    #[test]
    fn engineering_work_uses_the_authoritative_policy_path() {
        assert_eq!(
            request_policy(true),
            jarvis_policy::PolicyDecision::RequireApproval
        );
        assert_eq!(request_policy(false), jarvis_policy::PolicyDecision::Deny);
    }

    #[test]
    fn coding_sessions_keep_only_bounded_factual_checkpoints() {
        let mut session =
            CodingSession::new("PersonalJarvis", "abc123", "fix bounded parser").unwrap();
        session
            .checkpoint(CodingCheckpoint {
                summary: "Parser changed; no secrets or reasoning retained.".into(),
                decisions: vec!["Use typed parser".into()],
                pending: vec!["Run integration test".into()],
                tests: "unit tests: pass".into(),
            })
            .unwrap();
        session.transition(SessionState::Suspended).unwrap();
        session.transition(SessionState::Active).unwrap();
        assert!(session.checkpoint.is_some());
        assert!(session.transition(SessionState::Archived).is_err());
    }

    fn signed_start() -> SignedCodingRequest {
        let now = OffsetDateTime::now_utc();
        SignedCodingRequest {
            request_id: Uuid::now_v7(),
            nonce_hex: hex::encode([7_u8; 32]),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
            issued_at: now,
            expires_at: now + Duration::minutes(2),
            operation: CodingOperation::StartCodingRun {
                coding_session_id: Uuid::now_v7(),
                repository: RepositoryIdentity {
                    id: "personaljarvis".into(),
                    owner: "HawkeyNL".into(),
                    name: "PersonalJarvis".into(),
                },
                base_commit_sha: "a".repeat(40),
                worktree_label: Some("approved-fix".into()),
                objective: "Fix the bounded parser".into(),
                checkpoint: None,
                timeout_secs: 60,
                budget_reservation_id: Uuid::now_v7(),
                max_artifacts: 2,
                max_output_bytes: 1024,
            },
            signature_hex: hex::encode([0_u8; 64]),
        }
    }

    fn capability_for(
        request: &SignedCodingRequest,
    ) -> (RunCapabilityAuthority, RunCapabilityToken, Uuid) {
        let authority = RunCapabilityAuthority::default();
        let run_id = Uuid::now_v7();
        let claims = RunCapabilityClaims::from_signed_request(run_id, request, 100).unwrap();
        let token = authority.mint(claims).unwrap();
        (authority, token, run_id)
    }

    fn valid_snapshot(request: &SignedCodingRequest) -> RepositorySnapshot {
        let base = match &request.operation {
            CodingOperation::StartCodingRun {
                base_commit_sha, ..
            }
            | CodingOperation::ResumeCodingRun {
                base_commit_sha, ..
            } => base_commit_sha,
        };
        let mut builder = tar::Builder::new(Vec::new());
        let pax = format!("{} comment={base}\n", 12 + base.len());
        let mut header = tar::Header::new_ustar();
        header.set_path("pax_global_header").unwrap();
        header.set_entry_type(tar::EntryType::XGlobalHeader);
        header.set_size(pax.len() as u64);
        header.set_cksum();
        builder.append(&header, std::io::Cursor::new(pax)).unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_path("README.md").unwrap();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(4);
        header.set_cksum();
        builder
            .append(&header, std::io::Cursor::new(b"safe"))
            .unwrap();
        RepositorySnapshot {
            base_commit_sha: base.clone(),
            archive: builder.into_inner().unwrap(),
        }
    }

    struct TestAdapter;
    #[async_trait]
    impl relay::CodexSubscriptionAdapter for TestAdapter {
        fn available(&self) -> bool {
            true
        }
        async fn run_approved_task(
            &self,
            _: &BrokeredCodexRequest,
            _: &TaskEnvelope,
        ) -> Result<runtime::TaskChannelResponse, CodingRunError> {
            Err(CodingRunError::CodexAuthenticationUnavailable)
        }
    }

    #[test]
    fn coding_protocol_has_no_arbitrary_command_path_or_environment() {
        let request = signed_start();
        let payload = String::from_utf8(request.operation.canonical_payload().unwrap()).unwrap();
        assert!(!payload.contains("command"));
        assert!(!payload.contains("environment"));
        assert!(!payload.contains("path"));
        let task = request.operation.sandbox_task().unwrap();
        assert_eq!(task.profile, jarvis_sandbox::SandboxProfile::Codex);
        assert_eq!(task.command, CODEX_SANDBOX_COMMAND.map(str::to_owned));
    }

    #[test]
    fn altered_signed_coding_payload_fails_signature_verification() {
        use ed25519_dalek::{Signer, SigningKey};
        let mut request = signed_start();
        let key = SigningKey::from_bytes(&[11; 32]);
        request.signature_hex = hex::encode(key.sign(&request.message().unwrap()).to_bytes());
        let CodingOperation::StartCodingRun { objective, .. } = &mut request.operation else {
            unreachable!("fixture creates a start operation");
        };
        *objective = "Do something else".into();
        assert!(jarvis_identity::verify_signature(
            key.verifying_key().as_bytes(),
            &request.message().unwrap(),
            &hex::decode(&request.signature_hex).unwrap(),
        )
        .is_err());
    }

    #[test]
    fn coding_protocol_rejects_paths_and_expired_approvals() {
        let mut request = signed_start();
        let CodingOperation::StartCodingRun {
            repository,
            worktree_label,
            ..
        } = &mut request.operation
        else {
            unreachable!("fixture creates a start operation");
        };
        repository.id = "../../etc/jarvis".into();
        *worktree_label = Some("sh -c id".into());
        assert_eq!(
            request.message(),
            Err(CodingProtocolError::InvalidOperation)
        );

        for id in [".", ".."] {
            let mut request = signed_start();
            let CodingOperation::StartCodingRun { repository, .. } = &mut request.operation else {
                unreachable!()
            };
            repository.id = id.into();
            assert_eq!(
                request.message(),
                Err(CodingProtocolError::InvalidOperation)
            );
        }

        let mut request = signed_start();
        request.expires_at = request.issued_at - Duration::seconds(1);
        assert!(request.message().is_err());

        let mut request = signed_start();
        let CodingOperation::StartCodingRun { max_artifacts, .. } = &mut request.operation else {
            unreachable!()
        };
        *max_artifacts = 1;
        assert_eq!(
            request.message(),
            Err(CodingProtocolError::InvalidOperation)
        );
    }

    #[derive(Clone)]
    struct TestProvider {
        calls: Arc<Mutex<Vec<&'static str>>>,
        available: bool,
    }

    #[async_trait]
    impl jarvis_sandbox::SandboxProvider for TestProvider {
        async fn availability(&self) -> jarvis_sandbox::SandboxAvailability {
            self.calls.lock().unwrap().push("availability");
            if self.available {
                jarvis_sandbox::SandboxAvailability::Available
            } else {
                jarvis_sandbox::SandboxAvailability::Unavailable
            }
        }
        async fn create(
            &self,
            task: &jarvis_sandbox::SandboxTask,
        ) -> Result<jarvis_sandbox::SandboxHandle, jarvis_sandbox::SandboxError> {
            self.calls.lock().unwrap().push("create");
            Ok(jarvis_sandbox::SandboxHandle {
                provider_id: "test-run".into(),
                task_id: task.task_id,
                profile: task.profile,
            })
        }
        async fn upload(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            input: jarvis_sandbox::TaskInput,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            assert!(matches!(
                input.name.as_str(),
                "request.json" | "task-context.json" | "repository.tar" | "codex-capability.json"
            ));
            if input.name == "task-context.json" {
                assert!(input.bytes.len() <= MAX_TASK_ENVELOPE_BYTES);
                let envelope: TaskEnvelope = serde_json::from_slice(&input.bytes).unwrap();
                assert_eq!(envelope.intent, "coding");
                assert_eq!(envelope.base_commit_sha, "a".repeat(40));
            }
            self.calls.lock().unwrap().push("upload");
            Ok(())
        }
        async fn set_network_policy(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            _: &jarvis_sandbox::NetworkPolicy,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            self.calls.lock().unwrap().push("network");
            Ok(())
        }
        async fn provide_scoped_secret(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            _: jarvis_sandbox::ScopedSecret,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            self.calls.lock().unwrap().push("credential");
            Ok(())
        }
        async fn exec(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            command: &[String],
        ) -> Result<jarvis_sandbox::ExecutionResult, jarvis_sandbox::SandboxError> {
            assert_eq!(command, CODEX_SANDBOX_COMMAND.map(str::to_owned));
            self.calls.lock().unwrap().push("exec");
            Ok(jarvis_sandbox::ExecutionResult {
                exit_code: Some(0),
                timed_out: false,
                stdout_summary: "done".into(),
                stderr_summary: String::new(),
                duration_ms: 1,
            })
        }
        async fn collect_artifacts(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            _: &[String],
        ) -> Result<Vec<jarvis_sandbox::CollectedArtifact>, jarvis_sandbox::SandboxError> {
            self.calls.lock().unwrap().push("artifacts");
            Ok(vec![])
        }
        async fn terminate(
            &self,
            _: jarvis_sandbox::SandboxHandle,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            self.calls.lock().unwrap().push("terminate");
            Ok(())
        }
    }

    #[tokio::test]
    async fn coding_execution_never_falls_back_without_broker_capability() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = TestProvider {
            calls: calls.clone(),
            available: true,
        };
        let err = execute_in_sandbox(
            &provider,
            &signed_start(),
            valid_snapshot(&signed_start()),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &TestAdapter,
                authority: &RunCapabilityAuthority::default(),
                run_id: Uuid::now_v7(),
                capability: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, CodingRunError::CodexAuthenticationUnavailable);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn repository_snapshot_cannot_exceed_provider_upload_limit() {
        let request = signed_start();
        assert_eq!(
            RepositorySnapshot {
                base_commit_sha: "a".repeat(40),
                archive: vec![0; MAX_REPOSITORY_ARCHIVE_BYTES + 1],
            }
            .validate_for(&request.operation),
            Err(CodingRunError::RepositoryIsolationUnavailable)
        );
    }

    #[tokio::test]
    async fn failed_task_relay_uses_fixed_sandbox_and_always_terminates() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = TestProvider {
            calls: calls.clone(),
            available: true,
        };
        let request = signed_start();
        let (authority, capability, run_id) = capability_for(&request);
        let error = execute_in_sandbox(
            &provider,
            &request,
            valid_snapshot(&request),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &TestAdapter,
                authority: &authority,
                run_id,
                capability: Some(capability),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, CodingRunError::SandboxFailed);
        assert_eq!(calls.lock().unwrap().last(), Some(&"terminate"));
    }

    #[tokio::test]
    async fn signalled_cancellation_reaches_sandbox_termination() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = TestProvider {
            calls: calls.clone(),
            available: true,
        };
        let request = signed_start();
        let (authority, capability, run_id) = capability_for(&request);
        let (sender, receiver) = tokio::sync::watch::channel(true);
        let result = execute_in_sandbox_with_cancel(
            &provider,
            &request,
            valid_snapshot(&request),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &TestAdapter,
                authority: &authority,
                run_id,
                capability: Some(capability),
            },
            Some(receiver),
        )
        .await;
        drop(sender);
        assert!(matches!(result, Err(CodingRunError::SandboxFailed)));
        assert_eq!(calls.lock().unwrap().last(), Some(&"terminate"));
    }

    struct WorkspaceRelayProvider {
        root: std::path::PathBuf,
        terminated: std::sync::atomic::AtomicBool,
        delay_upload: bool,
        delay_response_ack: bool,
    }

    #[async_trait]
    impl jarvis_sandbox::SandboxProvider for WorkspaceRelayProvider {
        async fn availability(&self) -> jarvis_sandbox::SandboxAvailability {
            jarvis_sandbox::SandboxAvailability::Available
        }
        async fn create(
            &self,
            task: &jarvis_sandbox::SandboxTask,
        ) -> Result<jarvis_sandbox::SandboxHandle, jarvis_sandbox::SandboxError> {
            std::fs::create_dir(self.root.join("input")).unwrap();
            Ok(jarvis_sandbox::SandboxHandle {
                provider_id: "fixture".into(),
                task_id: task.task_id,
                profile: task.profile,
            })
        }
        async fn upload(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            input: jarvis_sandbox::TaskInput,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            if self.delay_upload {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            let response = input.name == "task-response.json";
            std::fs::write(self.root.join("input").join(input.name), input.bytes).unwrap();
            if response && self.delay_response_ack {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            Ok(())
        }
        async fn set_network_policy(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            _: &jarvis_sandbox::NetworkPolicy,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            Ok(())
        }
        async fn provide_scoped_secret(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            _: jarvis_sandbox::ScopedSecret,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            Err(jarvis_sandbox::SandboxError::Unsupported)
        }
        async fn exec(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            command: &[String],
        ) -> Result<jarvis_sandbox::ExecutionResult, jarvis_sandbox::SandboxError> {
            assert_eq!(command, CODEX_SANDBOX_COMMAND.map(str::to_owned));
            let root = self.root.clone();
            let outcome = tokio::task::spawn_blocking(move || runtime::run_in_workspace(&root))
                .await
                .unwrap();
            outcome.map_err(|_| jarvis_sandbox::SandboxError::ProviderRequestFailed)?;
            Ok(jarvis_sandbox::ExecutionResult {
                exit_code: Some(0),
                timed_out: false,
                stdout_summary: String::new(),
                stderr_summary: String::new(),
                duration_ms: 1,
            })
        }
        async fn collect_artifacts(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
            paths: &[String],
        ) -> Result<Vec<jarvis_sandbox::CollectedArtifact>, jarvis_sandbox::SandboxError> {
            paths
                .iter()
                .map(|path| {
                    let contents = std::fs::read(self.root.join("artifacts").join(path))
                        .map_err(|_| jarvis_sandbox::SandboxError::InvalidArtifact)?;
                    Ok(jarvis_sandbox::CollectedArtifact {
                        path: path.clone(),
                        contents,
                    })
                })
                .collect()
        }
        async fn read_codex_task_request(
            &self,
            _: &jarvis_sandbox::SandboxHandle,
        ) -> Result<Option<Vec<u8>>, jarvis_sandbox::SandboxError> {
            let path = self.root.join("channel/task-request.json");
            if !path.exists() {
                return Ok(None);
            }
            std::fs::read(path)
                .map(Some)
                .map_err(|_| jarvis_sandbox::SandboxError::InvalidArtifact)
        }
        async fn terminate(
            &self,
            _: jarvis_sandbox::SandboxHandle,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
            self.terminated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    struct SuccessAdapter(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl relay::CodexSubscriptionAdapter for SuccessAdapter {
        fn available(&self) -> bool {
            true
        }
        async fn run_approved_task(
            &self,
            binding: &BrokeredCodexRequest,
            _: &TaskEnvelope,
        ) -> Result<runtime::TaskChannelResponse, CodingRunError> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(runtime::TaskChannelResponse {
                binding: binding.clone(),
                outcome: runtime::TaskOutcome::Completed,
                summary: "Reviewed fixture".into(),
                patch_diff: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn approved_run_traverses_runtime_relay_and_always_terminates() {
        let workspace = tempfile::tempdir().unwrap();
        let provider = WorkspaceRelayProvider {
            root: workspace.path().to_path_buf(),
            terminated: std::sync::atomic::AtomicBool::new(false),
            delay_upload: false,
            delay_response_ack: true,
        };
        let adapter = SuccessAdapter(std::sync::atomic::AtomicUsize::new(0));
        let request = signed_start();
        let (authority, capability, run_id) = capability_for(&request);
        let result = execute_in_sandbox(
            &provider,
            &request,
            valid_snapshot(&request),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &adapter,
                authority: &authority,
                run_id,
                capability: Some(capability),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.run_id, run_id);
        assert_eq!(result.status, "completed");
        assert_eq!(result.stdout_summary, "Reviewed fixture");
        assert_eq!(adapter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(provider
            .terminated
            .load(std::sync::atomic::Ordering::SeqCst));
        assert!(!workspace.path().join("channel/task-request.json").exists());
    }

    struct SlowAdapter;

    #[async_trait]
    impl relay::CodexSubscriptionAdapter for SlowAdapter {
        fn available(&self) -> bool {
            true
        }

        async fn run_approved_task(
            &self,
            _: &BrokeredCodexRequest,
            _: &TaskEnvelope,
        ) -> Result<runtime::TaskChannelResponse, CodingRunError> {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            Err(CodingRunError::SandboxFailed)
        }
    }

    #[tokio::test]
    async fn signed_timeout_terminates_even_when_manager_upload_is_pending() {
        let workspace = tempfile::tempdir().unwrap();
        let provider = WorkspaceRelayProvider {
            root: workspace.path().to_path_buf(),
            terminated: std::sync::atomic::AtomicBool::new(false),
            delay_upload: true,
            delay_response_ack: false,
        };
        let mut request = signed_start();
        if let CodingOperation::StartCodingRun { timeout_secs, .. } = &mut request.operation {
            *timeout_secs = 1;
        }
        let (authority, capability, run_id) = capability_for(&request);
        let result = execute_in_sandbox(
            &provider,
            &request,
            valid_snapshot(&request),
            TaskContextInput::default(),
            ApprovedSandboxRun {
                adapter: &SlowAdapter,
                authority: &authority,
                run_id,
                capability: Some(capability),
            },
        )
        .await;
        assert!(matches!(result, Err(CodingRunError::TimedOut)));
        assert!(provider
            .terminated
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn delegated_context_keeps_checkpoint_and_constraints_not_long_history() {
        let mut request = signed_start();
        if let CodingOperation::StartCodingRun { checkpoint, .. } = &mut request.operation {
            *checkpoint = Some(CodingCheckpoint {
                summary: "Observed baseline behavior".into(),
                decisions: vec!["Keep device-signed approval".into()],
                pending: vec!["Verify sandbox isolation".into()],
                tests: "Unit fixtures passed".into(),
            });
        }
        let envelope = request
            .operation
            .compile_task_envelope(TaskContextInput {
                original_objective: None,
                owner_constraints: vec!["Only run read-only tests".into()],
                recent_deltas: (0..100)
                    .map(|index| format!("old or recent {index}: {}", "x".repeat(8_000)))
                    .collect(),
                selected_facts: vec![ProvenancedFact {
                    fact: "baseline checks passed".into(),
                    source: "checkpoint-1".into(),
                }],
            })
            .unwrap();
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert!(encoded.len() <= MAX_TASK_ENVELOPE_BYTES);
        assert_eq!(envelope.owner_constraints, ["Only run read-only tests"]);
        assert_eq!(
            envelope
                .checkpoint
                .as_ref()
                .map(|item| item.summary.as_str()),
            Some("Observed baseline behavior")
        );
        assert!(!envelope
            .recent_deltas
            .iter()
            .any(|delta| delta.contains("old or recent 0:")));
        assert!(envelope.recent_deltas.len() <= 8);
        assert!(envelope
            .recent_deltas
            .iter()
            .all(|delta| delta.chars().count() <= 2_048));
        assert_eq!(
            envelope.repository,
            match request.operation {
                CodingOperation::StartCodingRun { repository, .. } => repository,
                _ => unreachable!(),
            }
        );
    }

    #[test]
    fn delegated_context_rejects_oversized_owner_constraints_without_dropping_them() {
        let request = signed_start();
        assert!(request
            .operation
            .compile_task_envelope(TaskContextInput {
                owner_constraints: vec!["x".repeat(1_001)],
                ..TaskContextInput::default()
            })
            .is_err());
    }

    #[test]
    fn resume_requires_trusted_original_objective_not_checkpoint_summary() {
        let start = signed_start();
        let CodingOperation::StartCodingRun {
            coding_session_id,
            repository,
            base_commit_sha,
            budget_reservation_id,
            ..
        } = start.operation
        else {
            unreachable!()
        };
        let resume = CodingOperation::ResumeCodingRun {
            coding_session_id,
            repository,
            base_commit_sha,
            checkpoint: CodingCheckpoint {
                summary: "Only the last checkpoint, not the task".into(),
                decisions: vec![],
                pending: vec![],
                tests: "Not run".into(),
            },
            owner_delta: Some("New failing test".into()),
            timeout_secs: 60,
            budget_reservation_id,
            max_artifacts: 2,
            max_output_bytes: 1_024,
        };
        assert_eq!(
            resume.compile_task_envelope(TaskContextInput::default()),
            Err(CodingProtocolError::InvalidOperation)
        );
        let envelope = resume
            .compile_task_envelope(TaskContextInput {
                original_objective: Some("Implement the approved parser fix".into()),
                recent_deltas: vec!["New failing test".into()],
                ..TaskContextInput::default()
            })
            .unwrap();
        assert_eq!(envelope.objective, "Implement the approved parser fix");
        assert_eq!(envelope.recent_deltas, ["New failing test"]);
        assert_eq!(
            envelope.checkpoint.unwrap().summary,
            "Only the last checkpoint, not the task"
        );
    }

    fn broker_request(claims: &RunCapabilityClaims) -> BrokeredCodexRequest {
        BrokeredCodexRequest {
            request_id: Uuid::now_v7(),
            run_id: claims.run_id,
            coding_session_id: claims.coding_session_id,
            repository: claims.repository.clone(),
            base_commit_sha: claims.base_commit_sha.clone(),
            budget_reservation_id: claims.budget_reservation_id,
            operation: claims.operation,
        }
    }

    #[test]
    fn capability_is_bound_and_replay_expiry_budget_and_revocation_fail_closed() {
        let request = signed_start();
        let authority = RunCapabilityAuthority::default();
        let claims =
            RunCapabilityClaims::from_signed_request(Uuid::now_v7(), &request, 10).unwrap();
        let token = authority.mint(claims.clone()).unwrap();
        let first_request = broker_request(&claims);
        let now = OffsetDateTime::now_utc();
        authority.authorize(&token, &first_request, 5, now).unwrap();
        assert_eq!(
            authority.authorize(&token, &first_request, 1, now),
            Err(CapabilityError::Replay)
        );
        assert_eq!(
            authority.authorize_raw("not-a-token", &broker_request(&claims), 1, now),
            Err(CapabilityError::InvalidToken)
        );

        let mut wrong_repo = broker_request(&claims);
        wrong_repo.repository.name = "OtherRepo".into();
        assert_eq!(
            authority.authorize(&token, &wrong_repo, 1, now),
            Err(CapabilityError::WrongRepository)
        );
        let mut wrong_run = broker_request(&claims);
        wrong_run.run_id = Uuid::now_v7();
        assert_eq!(
            authority.authorize(&token, &wrong_run, 1, now),
            Err(CapabilityError::WrongRun)
        );
        let too_much = broker_request(&claims);
        assert_eq!(
            authority.authorize(&token, &too_much, 6, now),
            Err(CapabilityError::BudgetExceeded)
        );
        // A rejected budget attempt does not burn its request ID.
        authority.authorize(&token, &too_much, 5, now).unwrap();
        authority.revoke_for_cancel(&token);
        let after_cancel = broker_request(&claims);
        assert_eq!(
            authority.authorize(&token, &after_cancel, 1, now),
            Err(CapabilityError::Revoked)
        );

        let expiry_claims = RunCapabilityClaims {
            expires_at: now + Duration::seconds(1),
            ..claims
        };
        let expiry_token = authority.mint(expiry_claims.clone()).unwrap();
        assert_eq!(
            authority.authorize(
                &expiry_token,
                &broker_request(&expiry_claims),
                1,
                now + Duration::seconds(2)
            ),
            Err(CapabilityError::Expired)
        );
    }

    #[test]
    fn capability_request_history_stays_bounded_under_rejected_flood() {
        let request = signed_start();
        let authority = RunCapabilityAuthority::default();
        let claims =
            RunCapabilityClaims::from_signed_request(Uuid::now_v7(), &request, 100).unwrap();
        let token = authority.mint(claims.clone()).unwrap();
        let now = OffsetDateTime::now_utc();
        for _ in 0..MAX_CAPABILITY_REQUESTS {
            authority
                .authorize(&token, &broker_request(&claims), 1, now)
                .unwrap();
        }
        for _ in 0..1_000 {
            assert_eq!(
                authority.authorize(&token, &broker_request(&claims), 1, now),
                Err(CapabilityError::Replay)
            );
        }
        let hash: [u8; 32] = Sha256::digest(token.0.as_bytes()).into();
        let stored = authority.capabilities.lock().unwrap();
        assert_eq!(
            stored[&hash].used_request_ids.len(),
            MAX_CAPABILITY_REQUESTS
        );
    }

    #[test]
    fn completion_revokes_a_capability_and_token_material_is_not_debuggable() {
        let request = signed_start();
        let (authority, token, _) = capability_for(&request);
        let claims =
            RunCapabilityClaims::from_signed_request(Uuid::now_v7(), &request, 10).unwrap();
        // Different claim/run deliberately cannot authorize against the first token.
        authority.revoke_for_completion(&token);
        assert_eq!(
            authority.authorize(
                &token,
                &broker_request(&claims),
                1,
                OffsetDateTime::now_utc()
            ),
            Err(CapabilityError::Revoked)
        );
    }
}
