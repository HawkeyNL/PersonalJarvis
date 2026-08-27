//! Typed, local-only foundation for Codex engineering tasks (ADR-037).
//!
//! This crate deliberately does not spawn Codex, expose a listener, create a
//! workspace, or grant a tool. It models the small safe subset of the Codex App
//! Server JSON-RPC protocol that a later, policy-gated adapter may use.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_TASK_SUMMARY_CHARS: usize = 8_000;
pub const MAX_CHECKPOINT_CHARS: usize = 16_000;

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
}
