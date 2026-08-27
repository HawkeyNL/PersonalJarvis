//! Typed, local-only foundation for Codex engineering tasks (ADR-037).
//!
//! This crate deliberately does not spawn Codex, expose a listener, create a
//! workspace, or grant a tool. It models the small safe subset of the Codex App
//! Server JSON-RPC protocol that a later, policy-gated adapter may use.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_TASK_SUMMARY_CHARS: usize = 8_000;
pub const MAX_CHECKPOINT_CHARS: usize = 16_000;
pub const MAX_CODING_TIMEOUT_SECS: u64 = 15 * 60;
pub const MAX_CODING_ARTIFACTS: u8 = 32;
pub const MAX_CODING_OUTPUT_BYTES: u64 = 512 * 1024;
pub const ACTION_CODING_START: &str = "coding.start";
pub const ACTION_CODING_RESUME: &str = "coding.resume";

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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrokerRequest {
    StartCodingRun { request: SignedCodingRequest },
    ResumeCodingRun { request: SignedCodingRequest },
    CancelCodingRun { coding_session_id: Uuid },
    GetCodingRunStatus { coding_session_id: Uuid },
}

impl BrokerRequest {
    pub fn signed_request(&self) -> Option<&SignedCodingRequest> {
        match self {
            Self::StartCodingRun { request } | Self::ResumeCodingRun { request } => Some(request),
            Self::CancelCodingRun { .. } | Self::GetCodingRunStatus { .. } => None,
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
            Self::CancelCodingRun { .. } | Self::GetCodingRunStatus { .. } => Ok(()),
            _ => Err(CodingProtocolError::InvalidOperation),
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
            || self.archive.len() > 64 * 1024 * 1024
        {
            return Err(CodingRunError::RepositoryIsolationUnavailable);
        }
        Ok(())
    }
}

/// Safe, bounded facts returned to the trusted broker.  No hidden reasoning,
/// raw unlimited output or host path crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Run exactly one approved coding task in a disposable OpenSandbox workload.
/// This has no host-process branch: an unavailable provider, missing scoped
/// Codex credential, invalid snapshot or provider error fails before or during
/// the sandbox lifecycle and never invokes a local Codex CLI.
pub async fn execute_in_sandbox<P: jarvis_sandbox::SandboxProvider>(
    provider: &P,
    request: &SignedCodingRequest,
    snapshot: RepositorySnapshot,
    codex_credential: Option<jarvis_sandbox::ScopedSecret>,
) -> Result<CodingRunResult, CodingRunError> {
    request
        .operation
        .validate()
        .map_err(|_| CodingRunError::SandboxFailed)?;
    snapshot.validate_for(&request.operation)?;
    let credential = codex_credential.ok_or(CodingRunError::CodexAuthenticationUnavailable)?;
    if !matches!(
        provider.availability().await,
        jarvis_sandbox::SandboxAvailability::Available
    ) {
        return Err(CodingRunError::SandboxUnavailable);
    }
    let task = request
        .operation
        .sandbox_task()
        .map_err(|_| CodingRunError::SandboxFailed)?;
    let handle = provider
        .create(&task)
        .await
        .map_err(|_| CodingRunError::SandboxUnavailable)?;
    let result = async {
        provider
            .set_network_policy(&handle, &task.network_policy)
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let request_json =
            serde_json::to_vec(&request.operation).map_err(|_| CodingRunError::SandboxFailed)?;
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
                    name: "repository.tar".into(),
                    bytes: snapshot.archive,
                },
            )
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        provider
            .provide_scoped_secret(&handle, credential)
            .await
            .map_err(|_| CodingRunError::CodexAuthenticationUnavailable)?;
        let execution = provider
            .exec(&handle, &task.command)
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let artifacts = provider
            .collect_artifacts(&handle, &["result.json".into(), "patch.diff".into()])
            .await
            .map_err(|_| CodingRunError::SandboxFailed)?;
        let (session_id, base) = match &request.operation {
            CodingOperation::StartCodingRun {
                coding_session_id,
                base_commit_sha,
                ..
            }
            | CodingOperation::ResumeCodingRun {
                coding_session_id,
                base_commit_sha,
                ..
            } => (*coding_session_id, base_commit_sha.clone()),
        };
        Ok(CodingRunResult {
            run_id: task.task_id,
            coding_session_id: session_id,
            status: if execution.timed_out {
                "timed_out"
            } else if execution.exit_code == Some(0) {
                "completed"
            } else {
                "failed"
            }
            .into(),
            base_commit_sha: base,
            exit_code: execution.exit_code,
            timed_out: execution.timed_out,
            elapsed_ms: execution.duration_ms,
            stdout_summary: execution.stdout_summary,
            stderr_summary: execution.stderr_summary,
            artifacts: artifacts
                .into_iter()
                .map(|artifact| artifact.path)
                .collect(),
            termination_reason: if execution.timed_out {
                "timeout"
            } else {
                "completed"
            }
            .into(),
        })
    }
    .await;
    // Destruction is mandatory for success, failure and cancellation paths.
    // There is no retained sandbox to resume later.
    let terminated = provider.terminate(handle).await;
    if terminated.is_err() {
        return Err(CodingRunError::SandboxFailed);
    }
    result
}

/// A root-managed registry resolves this logical identity to a trusted source
/// snapshot. It is deliberately not a Git URL or local filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[serde(tag = "action", rename_all = "snake_case")]
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
                timeout_secs,
                max_artifacts,
                max_output_bytes,
                ..
            } => {
                checkpoint
                    .validate()
                    .map_err(|_| CodingProtocolError::InvalidOperation)?;
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
            || *artifacts == 0
            || *artifacts > MAX_CODING_ARTIFACTS
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
pub struct CodingCheckpoint {
    pub summary: String,
    pub decisions: Vec<String>,
    pub pending: Vec<String>,
    pub tests: String,
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

        let mut request = signed_start();
        request.expires_at = request.issued_at - Duration::seconds(1);
        assert!(request.message().is_err());
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
            _: jarvis_sandbox::TaskInput,
        ) -> Result<(), jarvis_sandbox::SandboxError> {
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
    async fn coding_execution_never_falls_back_without_scoped_auth() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = TestProvider {
            calls: calls.clone(),
            available: true,
        };
        let err = execute_in_sandbox(
            &provider,
            &signed_start(),
            RepositorySnapshot {
                base_commit_sha: "a".repeat(40),
                archive: vec![1],
            },
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, CodingRunError::CodexAuthenticationUnavailable);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn coding_execution_uses_fixed_sandbox_and_always_terminates() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let provider = TestProvider {
            calls: calls.clone(),
            available: true,
        };
        let result = execute_in_sandbox(
            &provider,
            &signed_start(),
            RepositorySnapshot {
                base_commit_sha: "a".repeat(40),
                archive: vec![1],
            },
            Some(jarvis_sandbox::ScopedSecret {
                scope_id: "task-scoped".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.status, "completed");
        assert_eq!(calls.lock().unwrap().last(), Some(&"terminate"));
    }
}
