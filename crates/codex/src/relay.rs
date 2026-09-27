//! One-shot manager-mediated task relay. The sandbox never opens a Home Node
//! socket or receives the persistent Codex credential. The broker reads one
//! fixed sandbox file through the authenticated OpenSandbox manager, validates
//! the run capability, and writes one fixed response file back.

use std::time::Duration;

use async_trait::async_trait;
use jarvis_sandbox::{SandboxHandle, SandboxProvider};
use time::OffsetDateTime;

use crate::{
    runtime::{TaskChannelRequest, TaskChannelResponse},
    CodingRunError, RunCapabilityAuthority, SandboxRunInput, TaskEnvelope,
};

#[async_trait]
pub trait CodexSubscriptionAdapter: Send + Sync {
    /// Must be false unless a reviewed subscription-authenticated provider
    /// interface exists. A metered API key is never a compatible adapter.
    fn available(&self) -> bool;

    async fn run_approved_task(
        &self,
        binding: &crate::BrokeredCodexRequest,
        context: &TaskEnvelope,
    ) -> Result<TaskChannelResponse, CodingRunError>;
}

/// Explicitly closed production gate until the official runtime supports the
/// required credential-isolated, non-shell provider interaction.
pub struct UnavailableSubscriptionAdapter;

#[async_trait]
impl CodexSubscriptionAdapter for UnavailableSubscriptionAdapter {
    fn available(&self) -> bool {
        false
    }

    async fn run_approved_task(
        &self,
        _: &crate::BrokeredCodexRequest,
        _: &TaskEnvelope,
    ) -> Result<TaskChannelResponse, CodingRunError> {
        Err(CodingRunError::CodexAuthenticationUnavailable)
    }
}

pub async fn relay_once<P: SandboxProvider, A: CodexSubscriptionAdapter + ?Sized>(
    provider: &P,
    adapter: &A,
    handle: &SandboxHandle,
    authority: &RunCapabilityAuthority,
    expected: &SandboxRunInput,
    context: &TaskEnvelope,
) -> Result<(), CodingRunError> {
    if !adapter.available() {
        return Err(CodingRunError::CodexAuthenticationUnavailable);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let raw = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(CodingRunError::TimedOut);
        }
        match provider.read_codex_task_request(handle).await {
            Ok(Some(bytes)) => break bytes,
            Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(_) => return Err(CodingRunError::SandboxFailed),
        }
    };
    if raw.len() > 48 * 1024 {
        return Err(CodingRunError::SandboxFailed);
    }
    let request: TaskChannelRequest =
        serde_json::from_slice(&raw).map_err(|_| CodingRunError::SandboxFailed)?;
    if request.binding != expected.brokered_request() || request.task != *context {
        return Err(CodingRunError::SandboxFailed);
    }
    // This reserves one bounded capability operation, not an EUR API charge.
    // Subscription telemetry and paid API spending remain separate ledgers.
    authority
        .authorize_raw(
            &request.capability_token,
            &request.binding,
            1,
            OffsetDateTime::now_utc(),
        )
        .map_err(|_| CodingRunError::SandboxFailed)?;
    let response = tokio::time::timeout(
        Duration::from_secs(expected.timeout_secs),
        adapter.run_approved_task(&request.binding, context),
    )
    .await
    .map_err(|_| CodingRunError::TimedOut)??;
    if response.binding != request.binding
        || response.summary.len() > 8 * 1024
        || response.patch_diff.len() > 512 * 1024
    {
        return Err(CodingRunError::SandboxFailed);
    }
    let encoded = serde_json::to_vec(&response).map_err(|_| CodingRunError::SandboxFailed)?;
    provider
        .write_codex_task_response(handle, encoded)
        .await
        .map_err(|_| CodingRunError::SandboxFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::TaskOutcome, BrokeredCodexOperation, RepositoryIdentity, RunCapabilityClaims,
    };
    use jarvis_sandbox::{
        CollectedArtifact, ExecutionResult, NetworkPolicy, SandboxAvailability, SandboxError,
        SandboxProfile, SandboxTask, ScopedSecret, TaskInput,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    use uuid::Uuid;

    struct FileRelayFixture {
        request: Vec<u8>,
        response: Mutex<Option<Vec<u8>>>,
    }

    #[async_trait]
    impl SandboxProvider for FileRelayFixture {
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
        async fn read_codex_task_request(
            &self,
            _: &SandboxHandle,
        ) -> Result<Option<Vec<u8>>, SandboxError> {
            Ok(Some(self.request.clone()))
        }
        async fn write_codex_task_response(
            &self,
            _: &SandboxHandle,
            bytes: Vec<u8>,
        ) -> Result<(), SandboxError> {
            *self.response.lock().unwrap() = Some(bytes);
            Ok(())
        }
        async fn terminate(&self, _: SandboxHandle) -> Result<(), SandboxError> {
            Ok(())
        }
    }

    struct FakeSubscriptionAdapter(AtomicUsize);
    #[async_trait]
    impl CodexSubscriptionAdapter for FakeSubscriptionAdapter {
        fn available(&self) -> bool {
            true
        }
        async fn run_approved_task(
            &self,
            binding: &crate::BrokeredCodexRequest,
            _: &TaskEnvelope,
        ) -> Result<TaskChannelResponse, CodingRunError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(TaskChannelResponse {
                binding: binding.clone(),
                outcome: TaskOutcome::Completed,
                summary: "fixture complete".into(),
                patch_diff: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn manager_relay_authorizes_exactly_one_bound_subscription_task() {
        let repository = RepositoryIdentity {
            id: "fixture".into(),
            owner: "Example".into(),
            name: "Repo".into(),
        };
        let input = SandboxRunInput {
            request_id: Uuid::now_v7(),
            run_id: Uuid::now_v7(),
            coding_session_id: Uuid::now_v7(),
            repository: repository.clone(),
            base_commit_sha: "a".repeat(40),
            snapshot_sha256: "b".repeat(64),
            budget_reservation_id: Uuid::now_v7(),
            operation: BrokeredCodexOperation::RunApprovedTask,
            max_output_bytes: 4096,
            timeout_secs: 5,
        };
        let context = TaskEnvelope {
            intent: "coding".into(),
            objective: "Inspect fixture".into(),
            owner_constraints: Vec::new(),
            recent_deltas: Vec::new(),
            selected_facts: Vec::new(),
            repository: repository.clone(),
            base_commit_sha: input.base_commit_sha.clone(),
            coding_session_id: input.coding_session_id,
            checkpoint: None,
            timeout_secs: 5,
            max_output_bytes: 4096,
        };
        let authority = RunCapabilityAuthority::default();
        let token = authority
            .mint(RunCapabilityClaims {
                run_id: input.run_id,
                coding_session_id: input.coding_session_id,
                repository,
                base_commit_sha: input.base_commit_sha.clone(),
                expires_at: OffsetDateTime::now_utc() + time::Duration::minutes(1),
                budget_reservation_id: input.budget_reservation_id,
                budget_limit_cents: 1,
                operation: BrokeredCodexOperation::RunApprovedTask,
            })
            .unwrap();
        let token_json: serde_json::Value = serde_json::from_slice(&token.sandbox_input()).unwrap();
        let task_request = TaskChannelRequest {
            binding: input.brokered_request(),
            capability_token: token_json["capability_token"].as_str().unwrap().into(),
            task: context.clone(),
        };
        let fixture = FileRelayFixture {
            request: serde_json::to_vec(&task_request).unwrap(),
            response: Mutex::new(None),
        };
        let handle = SandboxHandle {
            provider_id: "fixture".into(),
            task_id: input.run_id,
            profile: SandboxProfile::Codex,
        };
        let adapter = FakeSubscriptionAdapter(AtomicUsize::new(0));
        relay_once(&fixture, &adapter, &handle, &authority, &input, &context)
            .await
            .unwrap();
        assert_eq!(adapter.0.load(Ordering::SeqCst), 1);
        let response: TaskChannelResponse =
            serde_json::from_slice(fixture.response.lock().unwrap().as_ref().unwrap()).unwrap();
        assert_eq!(response.binding, input.brokered_request());
        assert_eq!(
            relay_once(&fixture, &adapter, &handle, &authority, &input, &context).await,
            Err(CodingRunError::SandboxFailed)
        );
        assert_eq!(adapter.0.load(Ordering::SeqCst), 1);
        authority.revoke_run(input.run_id, true);
        assert_eq!(
            relay_once(&fixture, &adapter, &handle, &authority, &input, &context).await,
            Err(CodingRunError::SandboxFailed)
        );
    }
}
