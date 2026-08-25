//! Execution-provider boundary for untrusted Jarvis work.
//!
//! This crate deliberately owns *no* public HTTP route and never falls back to
//! host-process execution.  A provider is selected by trusted orchestration only
//! after `jarvis-policy` and, where required, a device-signed approval have run.
//! See ADR-040 for the currently fail-closed OpenSandbox deployment gate.

use std::{collections::HashSet, fmt, net::IpAddr, time::Duration};

use async_trait::async_trait;
use reqwest::{redirect::Policy as RedirectPolicy, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

pub const MAX_COMMAND_ARGS: usize = 64;
pub const MAX_COMMAND_ARG_BYTES: usize = 8 * 1024;
pub const MAX_ALLOWED_DOMAINS: usize = 16;
pub const MAX_ARTIFACTS: usize = 32;
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_TOTAL_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const SANDBOX_WORKSPACE: &str = "/workspace";
const SANDBOX_INPUT_DIR: &str = "/workspace/input";
const SANDBOX_EXECD_PORT: u16 = 44_772;

/// Named, server-owned profiles. A caller cannot supply arbitrary resource or
/// network settings through this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    Research,
    Coding,
    Browser,
    DataAnalysis,
    Codex,
}

/// Explicit resource ceiling selected by a profile, never by untrusted input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub disk_mib: u32,
    pub pids: u32,
    pub max_runtime_secs: u64,
    pub max_output_bytes: u64,
}

/// Egress is deny-by-default. Domains are names, not URLs or literal IPs, so
/// private networks cannot be smuggled into an allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum NetworkPolicy {
    DenyAll,
    PublicWeb { allowed_domains: Vec<String> },
}

impl NetworkPolicy {
    pub fn validate(&self) -> Result<(), SandboxError> {
        let Self::PublicWeb { allowed_domains } = self else {
            return Ok(());
        };
        if allowed_domains.is_empty() || allowed_domains.len() > MAX_ALLOWED_DOMAINS {
            return Err(SandboxError::InvalidNetworkPolicy);
        }
        if allowed_domains
            .iter()
            .any(|domain| !is_public_domain(domain))
        {
            return Err(SandboxError::InvalidNetworkPolicy);
        }
        Ok(())
    }
}

/// Server-owned restrictions for each task class. The list stays deliberately
/// small until a real runtime is proven to enforce it.
pub fn profile_limits(profile: SandboxProfile) -> ResourceLimits {
    match profile {
        SandboxProfile::Research => ResourceLimits {
            cpu_millis: 500,
            memory_mib: 512,
            disk_mib: 512,
            pids: 64,
            max_runtime_secs: 5 * 60,
            max_output_bytes: 256 * 1024,
        },
        SandboxProfile::Coding | SandboxProfile::Codex => ResourceLimits {
            cpu_millis: 1500,
            memory_mib: 2048,
            disk_mib: 4096,
            pids: 128,
            max_runtime_secs: 15 * 60,
            max_output_bytes: 512 * 1024,
        },
        SandboxProfile::Browser => ResourceLimits {
            cpu_millis: 1000,
            memory_mib: 2048,
            disk_mib: 2048,
            pids: 128,
            max_runtime_secs: 10 * 60,
            max_output_bytes: 256 * 1024,
        },
        SandboxProfile::DataAnalysis => ResourceLimits {
            cpu_millis: 1000,
            memory_mib: 1024,
            disk_mib: 1024,
            pids: 64,
            max_runtime_secs: 10 * 60,
            max_output_bytes: 256 * 1024,
        },
    }
}

pub fn profile_network_policy(profile: SandboxProfile) -> NetworkPolicy {
    match profile {
        SandboxProfile::DataAnalysis => NetworkPolicy::DenyAll,
        SandboxProfile::Research => NetworkPolicy::PublicWeb {
            allowed_domains: vec!["*.wikipedia.org".into(), "*.arxiv.org".into()],
        },
        SandboxProfile::Coding | SandboxProfile::Codex => NetworkPolicy::PublicWeb {
            allowed_domains: vec![
                "github.com".into(),
                "*.github.com".into(),
                "crates.io".into(),
                "*.crates.io".into(),
                "registry.npmjs.org".into(),
                "pypi.org".into(),
                "files.pythonhosted.org".into(),
            ],
        },
        SandboxProfile::Browser => NetworkPolicy::PublicWeb {
            allowed_domains: vec!["*.google.com".into(), "*.googleusercontent.com".into()],
        },
    }
}

/// A trusted orchestrator-created task. It is not an API DTO and must never be
/// decoded directly from a public client request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxTask {
    pub task_id: Uuid,
    pub profile: SandboxProfile,
    pub command: Vec<String>,
    pub network_policy: NetworkPolicy,
}

impl SandboxTask {
    pub fn new(profile: SandboxProfile, command: Vec<String>) -> Result<Self, SandboxError> {
        if command.is_empty()
            || command.len() > MAX_COMMAND_ARGS
            || command
                .iter()
                .any(|arg| arg.is_empty() || arg.len() > MAX_COMMAND_ARG_BYTES)
        {
            return Err(SandboxError::InvalidTask);
        }
        let network_policy = profile_network_policy(profile);
        network_policy.validate()?;
        Ok(Self {
            task_id: Uuid::now_v7(),
            profile,
            command,
            network_policy,
        })
    }

    /// Code execution always enters through the authoritative policy crate.
    /// This boundary deliberately models the pre-approval decision only: a
    /// boolean cannot stand in for a verified, action-bound device signature.
    pub fn policy_decision(&self, trusted_device: bool) -> jarvis_policy::PolicyDecision {
        jarvis_policy::decide(&jarvis_policy::PolicyContext {
            capability: jarvis_policy::Capability::ExecuteCode,
            risk: jarvis_policy::RiskClass::Mutating,
            trusted_device,
            approved: false,
            reversible: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxHandle {
    pub provider_id: String,
    pub task_id: Uuid,
    pub profile: SandboxProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: String,
    pub bytes: u64,
}

/// Artifact content copied out through the managed output directory. The
/// provider never returns a host path or a sandbox-wide filesystem listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedArtifact {
    pub path: String,
    pub contents: Vec<u8>,
}

/// Task input copied into a disposable sandbox. Inputs are data, never host
/// paths or bind mounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInput {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl TaskInput {
    fn validate(&self) -> Result<(), SandboxError> {
        if self.bytes.len() > MAX_INPUT_BYTES || !is_safe_relative_path(&self.name) {
            return Err(SandboxError::InvalidInput);
        }
        Ok(())
    }
}

/// Opaque reference to a manager-issued, task-scoped secret. The plaintext is
/// deliberately not representable in the provider API or returned result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedSecret {
    pub scope_id: String,
}

/// Bounded, structured execution result. Provider implementations must cap
/// summaries to the selected profile's `max_output_bytes` before returning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_summary: String,
    pub stderr_summary: String,
    pub duration_ms: u64,
}

/// Validate provider-returned artifacts before Core accepts them. No absolute
/// path, traversal, or oversized/too-many outputs crosses this trust boundary.
pub fn validate_artifacts(artifacts: &[Artifact]) -> Result<(), SandboxError> {
    if artifacts.len() > MAX_ARTIFACTS {
        return Err(SandboxError::InvalidArtifact);
    }
    for artifact in artifacts {
        if artifact.bytes > MAX_ARTIFACT_BYTES || !is_safe_relative_path(&artifact.path) {
            return Err(SandboxError::InvalidArtifact);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SandboxError {
    #[error("sandbox unavailable")]
    Unavailable,
    #[error("sandbox task is invalid")]
    InvalidTask,
    #[error("sandbox network policy is invalid")]
    InvalidNetworkPolicy,
    #[error("sandbox artifact is invalid")]
    InvalidArtifact,
    #[error("sandbox input is invalid")]
    InvalidInput,
    #[error("sandbox configuration is invalid")]
    InvalidConfiguration,
    #[error("sandbox provider request failed")]
    ProviderRequestFailed,
    #[error("sandbox provider returned an invalid response")]
    InvalidProviderResponse,
    #[error("sandbox output exceeded its bound")]
    OutputLimitExceeded,
    #[error("sandbox operation is not enabled")]
    Unsupported,
}

/// The narrow boundary Core uses. Implementations must return `Unavailable`,
/// never run the requested command on the Home Node, when their runtime is down
/// or fails a security preflight.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    async fn availability(&self) -> SandboxAvailability;
    async fn create(&self, task: &SandboxTask) -> Result<SandboxHandle, SandboxError>;
    async fn upload(&self, handle: &SandboxHandle, input: TaskInput) -> Result<(), SandboxError>;
    async fn set_network_policy(
        &self,
        handle: &SandboxHandle,
        policy: &NetworkPolicy,
    ) -> Result<(), SandboxError>;
    async fn provide_scoped_secret(
        &self,
        handle: &SandboxHandle,
        secret: ScopedSecret,
    ) -> Result<(), SandboxError>;
    async fn exec(
        &self,
        handle: &SandboxHandle,
        command: &[String],
    ) -> Result<ExecutionResult, SandboxError>;
    async fn collect_artifacts(
        &self,
        handle: &SandboxHandle,
        paths: &[String],
    ) -> Result<Vec<CollectedArtifact>, SandboxError>;
    async fn terminate(&self, handle: SandboxHandle) -> Result<(), SandboxError>;
}

/// Explicit disabled/fail-closed provider for production configurations where
/// sandbox execution has not passed its runtime preflight.
#[derive(Debug, Default)]
pub struct DisabledProvider;

#[async_trait]
impl SandboxProvider for DisabledProvider {
    async fn availability(&self) -> SandboxAvailability {
        SandboxAvailability::Unavailable
    }
    async fn create(&self, _: &SandboxTask) -> Result<SandboxHandle, SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn upload(&self, _: &SandboxHandle, _: TaskInput) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn set_network_policy(
        &self,
        _: &SandboxHandle,
        _: &NetworkPolicy,
    ) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn provide_scoped_secret(
        &self,
        _: &SandboxHandle,
        _: ScopedSecret,
    ) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn exec(&self, _: &SandboxHandle, _: &[String]) -> Result<ExecutionResult, SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn collect_artifacts(
        &self,
        _: &SandboxHandle,
        _: &[String],
    ) -> Result<Vec<CollectedArtifact>, SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn terminate(&self, _: SandboxHandle) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }
}

/// Immutable, manager-owned image references. Images are selected by profile,
/// never supplied by a task, and must be digest pinned.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenSandboxImages {
    pub research: String,
    pub coding: String,
    pub browser: String,
    pub data_analysis: String,
    pub codex: String,
}

impl OpenSandboxImages {
    fn for_profile(&self, profile: SandboxProfile) -> &str {
        match profile {
            SandboxProfile::Research => &self.research,
            SandboxProfile::Coding => &self.coding,
            SandboxProfile::Browser => &self.browser,
            SandboxProfile::DataAnalysis => &self.data_analysis,
            SandboxProfile::Codex => &self.codex,
        }
    }

    fn validate(&self) -> bool {
        [
            &self.research,
            &self.coding,
            &self.browser,
            &self.data_analysis,
            &self.codex,
        ]
        .into_iter()
        .all(|image| is_digest_pinned_image(image))
    }
}

impl fmt::Debug for OpenSandboxImages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenSandboxImages")
            .field("research", &self.research)
            .field("coding", &self.coding)
            .field("browser", &self.browser)
            .field("data_analysis", &self.data_analysis)
            .field("codex", &self.codex)
            .finish()
    }
}

/// Credentials and endpoint are supplied by trusted Home Node configuration;
/// the public API cannot construct this object. `api_key` is intentionally
/// redacted from `Debug` so tracing it cannot disclose the control-plane key.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenSandboxConfig {
    pub endpoint: String,
    pub api_key: String,
    pub images: OpenSandboxImages,
}

impl fmt::Debug for OpenSandboxConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenSandboxConfig")
            .field("endpoint", &self.endpoint)
            .field("api_key", &"[REDACTED]")
            .field("images", &self.images)
            .finish()
    }
}

/// Authenticated loopback-only OpenSandbox adapter. It uses the lifecycle API
/// directly; no sandbox workload port is ever exposed to Jarvis Core or the
/// public API. The deployment patch in ADR-040 additionally makes every
/// runtime-created port loopback-only.
#[derive(Debug)]
pub struct OpenSandboxProvider {
    endpoint: Url,
    api_key: String,
    images: OpenSandboxImages,
    client: Client,
}

impl OpenSandboxProvider {
    pub fn for_home_node(config: OpenSandboxConfig) -> Result<Self, SandboxError> {
        let endpoint = validate_loopback_endpoint(&config.endpoint)?;
        if config.api_key.trim().is_empty() || !config.images.validate() {
            return Err(SandboxError::InvalidConfiguration);
        }
        let client = Client::builder()
            // The control plane is a local trust boundary. In particular, do
            // not let HTTP_PROXY/HTTPS_PROXY turn an authenticated loopback
            // request into a request that discloses its API key to a proxy.
            .no_proxy()
            .redirect(RedirectPolicy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| SandboxError::InvalidConfiguration)?;
        Ok(Self {
            endpoint,
            api_key: config.api_key,
            images: config.images,
            client,
        })
    }

    fn endpoint(&self, path: &str) -> Result<Url, SandboxError> {
        self.endpoint
            .join(path)
            .map_err(|_| SandboxError::InvalidConfiguration)
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.header("OPEN-SANDBOX-API-KEY", &self.api_key)
    }

    fn proxy_endpoint(&self, handle: &SandboxHandle, path: &str) -> Result<Url, SandboxError> {
        if !is_safe_provider_id(&handle.provider_id)
            || !path.starts_with('/')
            || path.contains("..")
        {
            return Err(SandboxError::InvalidProviderResponse);
        }
        self.endpoint(&format!(
            "v1/sandboxes/{}/proxy/{SANDBOX_EXECD_PORT}{}",
            handle.provider_id, path
        ))
    }

    async fn request_success(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, SandboxError> {
        request
            .send()
            .await
            .map_err(|_| SandboxError::Unavailable)?
            .error_for_status()
            .map_err(|_| SandboxError::ProviderRequestFailed)
    }

    async fn terminate_after_failure(&self, handle: &SandboxHandle) {
        // A disposable sandbox with an interrupted protocol/output stream is
        // never reused. Best effort is intentional: the original failure is
        // retained and the lifecycle timeout remains a second cleanup bound.
        let _ = self.terminate(handle.clone()).await;
    }
}

#[async_trait]
impl SandboxProvider for OpenSandboxProvider {
    async fn availability(&self) -> SandboxAvailability {
        let Ok(endpoint) = self.endpoint("health") else {
            return SandboxAvailability::Unavailable;
        };
        match self.client.get(endpoint).send().await {
            Ok(response) if response.status().is_success() => SandboxAvailability::Available,
            _ => SandboxAvailability::Unavailable,
        }
    }
    async fn create(&self, task: &SandboxTask) -> Result<SandboxHandle, SandboxError> {
        task.network_policy.validate()?;
        let endpoint = self.endpoint("v1/sandboxes")?;
        let response = self
            .request_success(
                self.authenticated(self.client.post(endpoint))
                    .json(&opensandbox_create_payload(task, &self.images)),
            )
            .await?;
        let body: Value = response
            .json()
            .await
            .map_err(|_| SandboxError::InvalidProviderResponse)?;
        let provider_id = body
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| is_safe_provider_id(id))
            .ok_or(SandboxError::InvalidProviderResponse)?
            .to_owned();
        Ok(SandboxHandle {
            provider_id,
            task_id: task.task_id,
            profile: task.profile,
        })
    }
    async fn upload(&self, handle: &SandboxHandle, input: TaskInput) -> Result<(), SandboxError> {
        input.validate()?;
        let path = format!("{SANDBOX_INPUT_DIR}/{}", input.name);
        let metadata = json!({ "path": path });
        let form = reqwest::multipart::Form::new()
            .part(
                "metadata",
                reqwest::multipart::Part::text(metadata.to_string())
                    .mime_str("application/json")
                    .map_err(|_| SandboxError::InvalidInput)?,
            )
            .part(
                "file",
                reqwest::multipart::Part::bytes(input.bytes)
                    .file_name("input")
                    .mime_str("application/octet-stream")
                    .map_err(|_| SandboxError::InvalidInput)?,
            );
        let endpoint = self.proxy_endpoint(handle, "/files/upload")?;
        let result = self
            .request_success(
                self.authenticated(self.client.post(endpoint))
                    .multipart(form),
            )
            .await
            .map(|_| ());
        if result.is_err() {
            self.terminate_after_failure(handle).await;
        }
        result
    }
    async fn set_network_policy(
        &self,
        handle: &SandboxHandle,
        policy: &NetworkPolicy,
    ) -> Result<(), SandboxError> {
        // OpenSandbox applies egress policy at sandbox creation. Allowing a
        // later mutation would make the policy race-prone, so only an exact
        // assertion of the already profile-owned policy is accepted here.
        if *policy != profile_network_policy(handle.profile) {
            return Err(SandboxError::InvalidNetworkPolicy);
        }
        Ok(())
    }
    async fn provide_scoped_secret(
        &self,
        _: &SandboxHandle,
        _: ScopedSecret,
    ) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable)
    }
    async fn exec(
        &self,
        handle: &SandboxHandle,
        command: &[String],
    ) -> Result<ExecutionResult, SandboxError> {
        validate_command(command)?;
        let limits = profile_limits(handle.profile);
        let endpoint = self.proxy_endpoint(handle, "/command")?;
        let response = self
            .request_success(
                self.authenticated(self.client.post(endpoint))
                    .timeout(Duration::from_secs(limits.max_runtime_secs + 15))
                    .header("accept", "text/event-stream")
                    .json(&json!({
                        "command": shell_quote_command(command),
                        "cwd": SANDBOX_WORKSPACE,
                        "background": false,
                        "timeout": limits.max_runtime_secs * 1_000,
                    })),
            )
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                self.terminate_after_failure(handle).await;
                return Err(error);
            }
        };
        let stream = consume_sse_response(response, limits.max_output_bytes as usize).await;
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                self.terminate_after_failure(handle).await;
                return Err(error);
            }
        };
        if !stream.completed {
            self.terminate_after_failure(handle).await;
            return Err(SandboxError::InvalidProviderResponse);
        }
        let command_id = stream
            .command_id
            .ok_or(SandboxError::InvalidProviderResponse)?;
        let status_endpoint =
            self.proxy_endpoint(handle, &format!("/command/status/{command_id}"))?;
        let status: Value = self
            .request_success(self.authenticated(self.client.get(status_endpoint)))
            .await?
            .json()
            .await
            .map_err(|_| SandboxError::InvalidProviderResponse)?;
        let exit_code = status.get("exit_code").and_then(Value::as_i64);
        let exit_code = exit_code
            .map(|code| i32::try_from(code).map_err(|_| SandboxError::InvalidProviderResponse))
            .transpose()?;
        let timed_out = status
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.to_ascii_lowercase().contains("timeout"));
        Ok(ExecutionResult {
            exit_code,
            timed_out,
            stdout_summary: stream.stdout,
            stderr_summary: stream.stderr,
            duration_ms: stream.duration_ms,
        })
    }
    async fn collect_artifacts(
        &self,
        handle: &SandboxHandle,
        paths: &[String],
    ) -> Result<Vec<CollectedArtifact>, SandboxError> {
        validate_artifact_paths(paths)?;
        let mut artifacts = Vec::with_capacity(paths.len());
        let mut total_bytes = 0_u64;
        for path in paths {
            let sandbox_path = format!("{SANDBOX_WORKSPACE}/artifacts/{path}");
            let info_endpoint = self.proxy_endpoint(handle, "/files/info")?;
            let info: Value = self
                .request_success(
                    self.authenticated(self.client.get(info_endpoint))
                        .query(&[("path", sandbox_path.as_str())]),
                )
                .await?
                .json()
                .await
                .map_err(|_| SandboxError::InvalidProviderResponse)?;
            let file = info
                .get(&sandbox_path)
                .ok_or(SandboxError::InvalidProviderResponse)?;
            if file.get("type").and_then(Value::as_str) != Some("file")
                || file.get("size").and_then(Value::as_u64) > Some(MAX_ARTIFACT_BYTES)
            {
                return Err(SandboxError::InvalidArtifact);
            }
            let download_endpoint = self.proxy_endpoint(handle, "/files/download")?;
            let response = self
                .request_success(
                    self.authenticated(self.client.get(download_endpoint))
                        .query(&[("path", sandbox_path.as_str())]),
                )
                .await?;
            if response.content_length() > Some(MAX_ARTIFACT_BYTES) {
                return Err(SandboxError::InvalidArtifact);
            }
            let contents = bounded_response_bytes(response, MAX_ARTIFACT_BYTES as usize).await?;
            total_bytes = total_bytes.saturating_add(contents.len() as u64);
            if total_bytes > MAX_TOTAL_ARTIFACT_BYTES {
                return Err(SandboxError::InvalidArtifact);
            }
            artifacts.push(CollectedArtifact {
                path: path.clone(),
                contents,
            });
        }
        Ok(artifacts)
    }
    async fn terminate(&self, handle: SandboxHandle) -> Result<(), SandboxError> {
        let endpoint = self.endpoint(&format!("v1/sandboxes/{}", handle.provider_id))?;
        self.request_success(self.authenticated(self.client.delete(endpoint)))
            .await
            .map(|_| ())
    }
}

fn validate_loopback_endpoint(value: &str) -> Result<Url, SandboxError> {
    let endpoint = Url::parse(value).map_err(|_| SandboxError::InvalidConfiguration)?;
    if endpoint.scheme() != "http"
        || endpoint.port().is_none()
        || endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
        || !matches!(endpoint.host_str(), Some("127.0.0.1") | Some("::1"))
    {
        return Err(SandboxError::InvalidConfiguration);
    }
    Ok(endpoint)
}

fn is_digest_pinned_image(image: &str) -> bool {
    let Some((_, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    image.len() > "@sha256:".len() + 64
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_safe_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn sandbox_profile_name(profile: SandboxProfile) -> &'static str {
    match profile {
        SandboxProfile::Research => "research",
        SandboxProfile::Coding => "coding",
        SandboxProfile::Browser => "browser",
        SandboxProfile::DataAnalysis => "data_analysis",
        SandboxProfile::Codex => "codex",
    }
}

fn opensandbox_network_policy(policy: &NetworkPolicy) -> Value {
    match policy {
        NetworkPolicy::DenyAll => json!({ "defaultAction": "deny", "egress": [] }),
        NetworkPolicy::PublicWeb { allowed_domains } => json!({
            "defaultAction": "deny",
            "egress": allowed_domains.iter().map(|target| json!({
                "action": "allow",
                "target": target,
            })).collect::<Vec<_>>(),
        }),
    }
}

fn opensandbox_create_payload(task: &SandboxTask, images: &OpenSandboxImages) -> Value {
    let limits = profile_limits(task.profile);
    json!({
        "image": { "uri": images.for_profile(task.profile) },
        "entrypoint": ["tail", "-f", "/dev/null"],
        "timeout": limits.max_runtime_secs,
        "resourceLimits": {
            "cpu": format!("{}m", limits.cpu_millis),
            "memory": format!("{}Mi", limits.memory_mib),
        },
        "networkPolicy": opensandbox_network_policy(&task.network_policy),
        "metadata": {
            "jarvis.task_id": task.task_id.to_string(),
            "jarvis.profile": sandbox_profile_name(task.profile),
        },
    })
}

fn validate_command(command: &[String]) -> Result<(), SandboxError> {
    if command.is_empty()
        || command.len() > MAX_COMMAND_ARGS
        || command
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > MAX_COMMAND_ARG_BYTES)
    {
        return Err(SandboxError::InvalidTask);
    }
    Ok(())
}

/// OpenSandbox's execd command API accepts one POSIX shell string. Jarvis does
/// not concatenate unquoted arguments: every trusted-manager argument is
/// single-quoted so an argument cannot alter the surrounding command syntax.
fn shell_quote_command(command: &[String]) -> String {
    command
        .iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Default)]
struct SseExecution {
    command_id: Option<String>,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    completed: bool,
}

async fn consume_sse_response(
    mut response: reqwest::Response,
    max_output_bytes: usize,
) -> Result<SseExecution, SandboxError> {
    // A server/proxy must not be able to force Jarvis to buffer an arbitrary
    // response. The small overhead permits SSE framing and completion metadata
    // in addition to the profile's stdout/stderr allowance.
    let max_wire_bytes = max_output_bytes.saturating_add(64 * 1024);
    let mut wire_bytes = 0_usize;
    let mut pending = String::new();
    let mut execution = SseExecution::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SandboxError::ProviderRequestFailed)?
    {
        wire_bytes = wire_bytes.saturating_add(chunk.len());
        if wire_bytes > max_wire_bytes {
            return Err(SandboxError::OutputLimitExceeded);
        }
        let text =
            std::str::from_utf8(&chunk).map_err(|_| SandboxError::InvalidProviderResponse)?;
        pending.push_str(text);
        while let Some(newline) = pending.find('\n') {
            let line = pending[..newline].trim_end_matches('\r').to_owned();
            pending.drain(..=newline);
            if let Some(data) = line.strip_prefix("data:") {
                consume_sse_event(data.trim_start(), max_output_bytes, &mut execution)?;
            }
        }
    }
    if !pending.trim().is_empty() {
        return Err(SandboxError::InvalidProviderResponse);
    }
    Ok(execution)
}

fn consume_sse_event(
    data: &str,
    max_output_bytes: usize,
    execution: &mut SseExecution,
) -> Result<(), SandboxError> {
    let event: Value =
        serde_json::from_str(data).map_err(|_| SandboxError::InvalidProviderResponse)?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .ok_or(SandboxError::InvalidProviderResponse)?;
    match event_type {
        "init" => {
            let id = event
                .get("text")
                .and_then(Value::as_str)
                .filter(|id| is_safe_provider_id(id))
                .ok_or(SandboxError::InvalidProviderResponse)?;
            execution.command_id = Some(id.to_owned());
        }
        "stdout" => append_bounded(
            &mut execution.stdout,
            event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            max_output_bytes,
        )?,
        "stderr" => append_bounded(
            &mut execution.stderr,
            event
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            max_output_bytes,
        )?,
        "error" => {
            let error = event
                .get("error")
                .and_then(|value| value.get("evalue"))
                .and_then(Value::as_str)
                .unwrap_or("sandbox command failed");
            append_bounded(&mut execution.stderr, error, max_output_bytes)?;
        }
        "execution_complete" => {
            execution.duration_ms = event
                .get("execution_time")
                .and_then(Value::as_u64)
                .ok_or(SandboxError::InvalidProviderResponse)?;
            execution.completed = true;
        }
        "status" | "result" | "execution_count" | "ping" => {}
        _ => return Err(SandboxError::InvalidProviderResponse),
    }
    Ok(())
}

fn append_bounded(
    destination: &mut String,
    value: &str,
    max_output_bytes: usize,
) -> Result<(), SandboxError> {
    if destination.len().saturating_add(value.len()) > max_output_bytes {
        return Err(SandboxError::OutputLimitExceeded);
    }
    destination.push_str(value);
    Ok(())
}

async fn bounded_response_bytes(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, SandboxError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SandboxError::ProviderRequestFailed)?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(SandboxError::InvalidArtifact);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_artifact_paths(paths: &[String]) -> Result<(), SandboxError> {
    if paths.is_empty() || paths.len() > MAX_ARTIFACTS {
        return Err(SandboxError::InvalidArtifact);
    }
    if paths.iter().any(|path| !is_safe_relative_path(path)) {
        return Err(SandboxError::InvalidArtifact);
    }
    if paths.iter().collect::<HashSet<_>>().len() != paths.len() {
        return Err(SandboxError::InvalidArtifact);
    }
    Ok(())
}

fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\\')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !path.contains('\0')
}

fn is_public_domain(domain: &str) -> bool {
    let domain = domain.strip_prefix("*.").unwrap_or(domain);
    !domain.is_empty()
        && domain.len() <= 253
        && domain.contains('.')
        && domain.parse::<IpAddr>().is_err()
        && domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        && !domain
            .split('.')
            .any(|part| part.is_empty() || part.starts_with('-') || part.ends_with('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_bounded_resources_and_network() {
        for profile in [
            SandboxProfile::Research,
            SandboxProfile::Coding,
            SandboxProfile::Browser,
            SandboxProfile::DataAnalysis,
            SandboxProfile::Codex,
        ] {
            let limits = profile_limits(profile);
            assert!(limits.cpu_millis > 0 && limits.memory_mib > 0 && limits.pids > 0);
            assert!(limits.max_runtime_secs <= 15 * 60);
            profile_network_policy(profile).validate().unwrap();
        }
        assert_eq!(
            profile_network_policy(SandboxProfile::DataAnalysis),
            NetworkPolicy::DenyAll
        );
    }

    #[test]
    fn egress_rejects_private_ips_and_non_domains() {
        for value in [
            "127.0.0.1",
            "::1",
            "10.0.0.1",
            "192.168.1.1",
            "localhost",
            "http://example.com",
            "example.com/path",
        ] {
            assert!(
                NetworkPolicy::PublicWeb {
                    allowed_domains: vec![value.into()]
                }
                .validate()
                .is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn artifacts_cannot_escape_or_exhaust_core() {
        for path in [
            "../secret",
            "/etc/shadow",
            "out//file",
            "out/../secret",
            "..\\secret",
        ] {
            assert_eq!(
                validate_artifacts(&[Artifact {
                    path: path.into(),
                    bytes: 1
                }]),
                Err(SandboxError::InvalidArtifact)
            );
        }
        assert_eq!(
            validate_artifacts(&[Artifact {
                path: "result.json".into(),
                bytes: MAX_ARTIFACT_BYTES + 1
            }]),
            Err(SandboxError::InvalidArtifact)
        );
        validate_artifacts(&[Artifact {
            path: "reports/result.json".into(),
            bytes: 1024,
        }])
        .unwrap();
        assert_eq!(
            validate_artifact_paths(&["../secrets.env".into()]),
            Err(SandboxError::InvalidArtifact)
        );
        assert_eq!(
            validate_artifact_paths(&[]),
            Err(SandboxError::InvalidArtifact)
        );
        assert_eq!(
            validate_artifact_paths(&["result.json".into(), "result.json".into()]),
            Err(SandboxError::InvalidArtifact)
        );
        validate_artifact_paths(&["result.json".into(), "reports/chart.png".into()]).unwrap();
    }

    #[test]
    fn execution_uses_authoritative_policy() {
        let task = SandboxTask::new(
            SandboxProfile::DataAnalysis,
            vec!["python".into(), "analysis.py".into()],
        )
        .unwrap();
        assert_eq!(
            task.policy_decision(true),
            jarvis_policy::PolicyDecision::RequireApproval
        );
        assert_eq!(
            task.policy_decision(false),
            jarvis_policy::PolicyDecision::Deny
        );
    }

    #[tokio::test]
    async fn unavailable_runtime_never_falls_back_to_host() {
        let provider = DisabledProvider;
        let task = SandboxTask::new(
            SandboxProfile::DataAnalysis,
            vec!["python".into(), "analysis.py".into()],
        )
        .unwrap();
        assert_eq!(
            provider.availability().await,
            SandboxAvailability::Unavailable
        );
        assert_eq!(provider.create(&task).await, Err(SandboxError::Unavailable));
        assert!(matches!(
            OpenSandboxProvider::for_home_node(OpenSandboxConfig {
                endpoint: "http://192.168.1.2:8090".into(),
                api_key: "control-plane-key".into(),
                images: test_images(),
            }),
            Err(SandboxError::InvalidConfiguration)
        ));
    }

    #[test]
    fn opensandbox_requires_loopback_key_and_digest_pinned_images() {
        let provider = OpenSandboxProvider::for_home_node(OpenSandboxConfig {
            endpoint: "http://127.0.0.1:8090/".into(),
            api_key: "control-plane-key".into(),
            images: test_images(),
        });
        assert!(provider.is_ok());

        for endpoint in [
            "https://127.0.0.1:8090/",
            "http://localhost:8090/",
            "http://127.0.0.1/",
            "http://127.0.0.1:8090/v1/",
        ] {
            assert_eq!(
                OpenSandboxProvider::for_home_node(OpenSandboxConfig {
                    endpoint: endpoint.into(),
                    api_key: "control-plane-key".into(),
                    images: test_images(),
                })
                .unwrap_err(),
                SandboxError::InvalidConfiguration,
                "{endpoint}"
            );
        }
        assert_eq!(
            OpenSandboxProvider::for_home_node(OpenSandboxConfig {
                endpoint: "http://127.0.0.1:8090/".into(),
                api_key: String::new(),
                images: test_images(),
            })
            .unwrap_err(),
            SandboxError::InvalidConfiguration
        );
        let mut noncanonical_images = test_images();
        noncanonical_images.coding = format!("registry.example/coding@sha256:{}", "A".repeat(64));
        assert_eq!(
            OpenSandboxProvider::for_home_node(OpenSandboxConfig {
                endpoint: "http://127.0.0.1:8090/".into(),
                api_key: "control-plane-key".into(),
                images: noncanonical_images,
            })
            .unwrap_err(),
            SandboxError::InvalidConfiguration
        );
    }

    #[test]
    fn opensandbox_payload_is_deny_by_default() {
        assert_eq!(
            opensandbox_network_policy(&NetworkPolicy::DenyAll),
            json!({ "defaultAction": "deny", "egress": [] })
        );
        let payload = opensandbox_network_policy(&NetworkPolicy::PublicWeb {
            allowed_domains: vec!["github.com".into()],
        });
        assert_eq!(payload["defaultAction"], "deny");
        assert_eq!(payload["egress"][0]["target"], "github.com");
    }

    #[test]
    fn lifecycle_request_is_profile_bound_and_never_mounts_hosts() {
        let task = SandboxTask::new(
            SandboxProfile::DataAnalysis,
            vec!["python".into(), "analysis.py".into()],
        )
        .unwrap();
        let request = opensandbox_create_payload(&task, &test_images());
        assert_eq!(request["networkPolicy"]["defaultAction"], "deny");
        assert_eq!(request["metadata"]["jarvis.profile"], "data_analysis");
        assert!(request.get("volumes").is_none());
        assert!(request.get("env").is_none());
        assert!(request.get("image").unwrap()["uri"]
            .as_str()
            .unwrap()
            .contains("@sha256:"));
    }

    #[test]
    fn input_and_shell_arguments_cannot_escape_the_sandbox_protocol() {
        for name in ["../secret", "/etc/shadow", "input\\file", "input//file"] {
            assert_eq!(
                TaskInput {
                    name: name.into(),
                    bytes: vec![],
                }
                .validate(),
                Err(SandboxError::InvalidInput),
                "{name}"
            );
        }
        let command = vec!["printf".into(), "x'; touch /host; echo 'y".into()];
        assert_eq!(
            shell_quote_command(&command),
            "'printf' 'x'\\''; touch /host; echo '\\''y'"
        );
    }

    #[test]
    fn sse_output_is_bounded_and_requires_a_safe_command_id() {
        let mut execution = SseExecution::default();
        consume_sse_event(r#"{"type":"init","text":"command-1"}"#, 10, &mut execution).unwrap();
        consume_sse_event(r#"{"type":"stdout","text":"hello"}"#, 10, &mut execution).unwrap();
        consume_sse_event(
            r#"{"type":"execution_complete","execution_time":12}"#,
            10,
            &mut execution,
        )
        .unwrap();
        assert_eq!(execution.command_id.as_deref(), Some("command-1"));
        assert_eq!(execution.stdout, "hello");
        assert_eq!(execution.duration_ms, 12);
        assert!(execution.completed);
        assert_eq!(
            consume_sse_event(r#"{"type":"stdout","text":"too-long"}"#, 10, &mut execution),
            Err(SandboxError::OutputLimitExceeded)
        );
        assert_eq!(
            consume_sse_event(r#"{"type":"init","text":"../bad"}"#, 10, &mut execution),
            Err(SandboxError::InvalidProviderResponse)
        );
    }

    #[tokio::test]
    async fn network_policy_cannot_be_relaxed_after_create() {
        let provider = OpenSandboxProvider::for_home_node(OpenSandboxConfig {
            endpoint: "http://127.0.0.1:8090/".into(),
            api_key: "control-plane-key".into(),
            images: test_images(),
        })
        .unwrap();
        let handle = SandboxHandle {
            provider_id: "sandbox-1".into(),
            task_id: Uuid::now_v7(),
            profile: SandboxProfile::DataAnalysis,
        };
        assert!(provider
            .set_network_policy(&handle, &NetworkPolicy::DenyAll)
            .await
            .is_ok());
        assert_eq!(
            provider
                .set_network_policy(
                    &handle,
                    &NetworkPolicy::PublicWeb {
                        allowed_domains: vec!["github.com".into()],
                    },
                )
                .await,
            Err(SandboxError::InvalidNetworkPolicy)
        );
    }

    fn test_images() -> OpenSandboxImages {
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        OpenSandboxImages {
            research: format!("registry.example/research@{digest}"),
            coding: format!("registry.example/coding@{digest}"),
            browser: format!("registry.example/browser@{digest}"),
            data_analysis: format!("registry.example/data-analysis@{digest}"),
            codex: format!("registry.example/codex@{digest}"),
        }
    }
}
