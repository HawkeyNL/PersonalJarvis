use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::logs::{parse_lines, sanitize, LogRecord};
use crate::session::{BrokerRequest, SessionManager};

const ADMIN: &str = "/usr/local/sbin/jarvis";
const CORE_ADMIN_BINARY: &str = "/usr/bin/jarvis-core-admin";
const CORE_ADMIN_VERSION: &str = "/usr/share/jarvis-core-admin/version";
const OUTPUT_LIMIT: usize = 1_048_576;

type AdminResult<T> = Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogService {
    Core,
    Surrealdb,
    ConfigBroker,
    CodexBroker,
    Opensandbox,
    Updater,
    AgentsUpdater,
}

impl LogService {
    fn cli_name(&self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Surrealdb => "surrealdb",
            Self::ConfigBroker => "config-broker",
            Self::CodexBroker => "codex-broker",
            Self::Opensandbox => "opensandbox",
            Self::Updater => "updater",
            Self::AgentsUpdater => "agents-updater",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LogQuery {
    pub service: LogService,
    pub lines: u16,
}

#[derive(Debug, Serialize)]
pub struct LogResponse {
    pub unit: String,
    pub records: Vec<LogRecord>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeStatus {
    pub running_version: String,
    pub installed_version: Option<String>,
    pub restart_required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UpdateMutation {
    Latest,
    InstallVersion { version: String },
    Rollback,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelProvider {
    AnthropicApi,
    OpenaiApi,
    DeepseekApi,
    XaiApi,
    ZaiApi,
    OllamaCloud,
    OllamaLocal,
    ClaudeCli,
    CodexCli,
}

impl ModelProvider {
    fn cli_name(&self) -> &'static str {
        match self {
            Self::AnthropicApi => "anthropic-api",
            Self::OpenaiApi => "openai-api",
            Self::DeepseekApi => "deepseek-api",
            Self::XaiApi => "xai-api",
            Self::ZaiApi => "zai-api",
            Self::OllamaCloud => "ollama-cloud",
            Self::OllamaLocal => "ollama-local",
            Self::ClaudeCli => "claude-cli",
            Self::CodexCli => "codex-cli",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ModelMutation {
    Refresh,
    Enable {
        provider: ModelProvider,
        model: String,
    },
    Disable {
        provider: ModelProvider,
        model: String,
    },
}

#[derive(Debug, Serialize)]
pub struct OperationResult {
    pub success: bool,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AgentBundle {
    pub id: String,
    pub agent_count: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct StatusReport {
    pub release: Option<String>,
    pub services: BTreeMap<String, String>,
    pub updater_enabled: String,
    pub agent_bundle: Option<AgentBundle>,
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub status: StatusReport,
    pub update: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub checks: BTreeMap<String, String>,
    pub verification: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceRecord {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ModelRecord {
    pub provider: String,
    pub model: String,
    pub enabled: bool,
    pub source: String,
    #[serde(default = "unknown_price_status")]
    pub price_status: String,
    #[serde(default)]
    pub input_per_million_usd: Option<f64>,
    #[serde(default)]
    pub cache_read_per_million_usd: Option<f64>,
    #[serde(default)]
    pub output_per_million_usd: Option<f64>,
    #[serde(default)]
    pub pricing_source: String,
    #[serde(default)]
    pub pricing_updated_at: String,
}

fn unknown_price_status() -> String {
    "unknown".to_owned()
}

#[derive(Debug, Deserialize)]
struct ModelPolicy {
    models: Vec<ModelRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageReport {
    pub period: String,
    pub generated_at_unix: u64,
    pub budget_eur: f64,
    pub spent_eur: f64,
    pub remaining_eur: f64,
    pub over_budget: bool,
    pub reserved_eur: f64,
    pub remaining_hard_eur: f64,
    pub above_soft_budget: bool,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub by_backend: Vec<UsageRow>,
    pub by_model: Vec<UsageRow>,
    pub daily: Vec<DailyUsageRow>,
    pub pricing: PricingSummary,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsageRow {
    pub backend: String,
    pub model: Option<String>,
    pub spent_eur: f64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DailyUsageRow {
    pub day: String,
    pub spent_eur: f64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PricingSummary {
    pub source: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CredentialRecord {
    pub provider: String,
    pub configured: bool,
}

#[derive(Debug, Deserialize)]
struct UpdateEnvelope {
    values: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawLogResponse {
    unit: String,
    lines: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SafeManifest {
    version: u32,
    bundle_id: String,
    agents: Vec<SafeManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct SafeManifestEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    model_policy: Option<String>,
    #[serde(default)]
    profile_lines: Option<u32>,
    #[serde(default)]
    source_updated_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub group: String,
    pub model_policy: Option<String>,
    pub profile_lines: Option<u32>,
    pub source_updated_at: Option<String>,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub struct AgentsResponse {
    pub bundle: AgentBundle,
    pub manifest_bundle: Option<String>,
    pub agents: Vec<AgentRecord>,
}

#[derive(Debug, Serialize)]
pub struct SystemResponse {
    pub values: Vec<(String, String)>,
}

pub fn root_guard() -> AdminResult<()> {
    if unsafe { libc::geteuid() } == 0 {
        Err(
            "Jarvis Core Administration must run as the normal desktop user, never as root"
                .to_owned(),
        )
    } else {
        Ok(())
    }
}

pub fn overview(session: &SessionManager) -> AdminResult<OverviewResponse> {
    let status = status(session)?;
    let update = update_values(session, false).ok();
    Ok(OverviewResponse { status, update })
}

pub fn health(session: &SessionManager, run_verification: bool) -> AdminResult<HealthResponse> {
    let status = status(session)?;
    let verification = if run_verification {
        let output = session.run(BrokerRequest::Health)?;
        let value: serde_json::Value = parse_json(&output.stdout)?;
        Some(
            if value.get("healthy").and_then(serde_json::Value::as_bool) == Some(true) {
                "passed".to_owned()
            } else {
                "failed".to_owned()
            },
        )
    } else {
        None
    };
    let mut checks = status.services;
    checks.insert("Updater".to_owned(), status.updater_enabled);
    Ok(HealthResponse {
        checks,
        verification,
    })
}

pub fn services(session: &SessionManager) -> AdminResult<Vec<ServiceRecord>> {
    Ok(status(session)?
        .services
        .into_iter()
        .map(|(name, state)| ServiceRecord { name, state })
        .collect())
}

pub fn update_status(
    session: &SessionManager,
    check: bool,
) -> AdminResult<BTreeMap<String, String>> {
    update_values(session, check)
}

pub fn update_mutation(
    session: &SessionManager,
    request: UpdateMutation,
) -> AdminResult<OperationResult> {
    operation(
        session.run(BrokerRequest::UpdateMutation { request })?,
        "Core operation completed",
    )
}

pub fn agents(session: &SessionManager) -> AdminResult<AgentsResponse> {
    let bundle: AgentBundle = parse_json(&session.run(BrokerRequest::AgentsStatus)?.stdout)?;
    let output = session.run(BrokerRequest::AgentTree)?;
    let manifest: SafeManifest = parse_json(&output.stdout)?;
    let (manifest_bundle, agents) = safe_agent_records(&bundle, manifest)?;
    Ok(AgentsResponse {
        bundle,
        manifest_bundle: Some(manifest_bundle),
        agents,
    })
}

fn safe_agent_records(
    bundle: &AgentBundle,
    manifest: SafeManifest,
) -> AdminResult<(String, Vec<AgentRecord>)> {
    if manifest.version != 1 || manifest.agents.len() > 512 || !safe_id(&manifest.bundle_id) {
        return Err("active agent manifest metadata is invalid".to_owned());
    }
    if manifest.bundle_id != bundle.id {
        return Err("active agent manifest does not match the active bundle".to_owned());
    }
    let agents = manifest
        .agents
        .into_iter()
        .map(|entry| {
            if !safe_id(&entry.id)
                || entry
                    .name
                    .as_deref()
                    .is_some_and(|value| !safe_label(value))
                || entry
                    .group
                    .as_deref()
                    .is_some_and(|value| !safe_label(value))
                || entry
                    .model_policy
                    .as_deref()
                    .is_some_and(|value| !safe_label(value))
                || entry
                    .profile_lines
                    .is_some_and(|value| value == 0 || value > 100_000)
                || entry
                    .source_updated_at
                    .as_deref()
                    .is_some_and(|value| !safe_source_timestamp(value))
            {
                return Err("active agent manifest contains unsafe display metadata".to_owned());
            }
            Ok(AgentRecord {
                name: entry.name.unwrap_or_else(|| entry.id.clone()),
                id: entry.id,
                group: entry.group.unwrap_or_else(|| "Ungrouped".to_owned()),
                model_policy: entry.model_policy,
                profile_lines: entry.profile_lines,
                source_updated_at: entry.source_updated_at,
                state: "active".to_owned(),
            })
        })
        .collect::<AdminResult<Vec<_>>>()?;
    Ok((manifest.bundle_id, agents))
}

pub fn agent_action(session: &SessionManager, update: bool) -> AdminResult<OperationResult> {
    operation(
        session.run(BrokerRequest::AgentAction { update })?,
        if update {
            "Agent update completed"
        } else {
            "Agent check completed"
        },
    )
}

pub fn models(session: &SessionManager) -> AdminResult<Vec<ModelRecord>> {
    Ok(parse_json::<ModelPolicy>(&session.run(BrokerRequest::Models)?.stdout)?.models)
}

pub fn usage(session: &SessionManager) -> AdminResult<UsageReport> {
    parse_json(&session.run(BrokerRequest::Usage)?.stdout)
}

pub fn model_mutation(
    session: &SessionManager,
    request: ModelMutation,
) -> AdminResult<OperationResult> {
    operation(
        session.run(BrokerRequest::ModelMutation { request })?,
        "Model policy updated",
    )
}

pub fn credentials(session: &SessionManager) -> AdminResult<Vec<CredentialRecord>> {
    parse_json(&session.run(BrokerRequest::Credentials)?.stdout)
}

pub fn logs(session: &SessionManager, query: LogQuery) -> AdminResult<LogResponse> {
    if !(1..=2_000).contains(&query.lines) {
        return Err("log line count must be between 1 and 2000".to_owned());
    }
    let response: RawLogResponse = parse_json(&session.run(BrokerRequest::Logs { query })?.stdout)?;
    Ok(LogResponse {
        unit: sanitize(&response.unit, 128),
        records: parse_lines(&response.lines),
    })
}

pub fn system() -> AdminResult<SystemResponse> {
    let release = read_fixed("/opt/jarvis/current/release.json", 64 * 1024)
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
    let provenance = read_fixed("/opt/jarvis/current/build-provenance.json", 64 * 1024)
        .ok()
        .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
    let installed_app_version = read_fixed(CORE_ADMIN_VERSION, 128)
        .ok()
        .map(|value| sanitize(value.trim(), 64))
        .unwrap_or_else(|| "not installed".to_owned());
    let os = read_fixed("/etc/os-release", 64 * 1024).unwrap_or_default();
    let os_name = os
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_owned())
        .unwrap_or_else(|| "Ubuntu Linux".to_owned());
    let values = vec![
        ("Active release".to_owned(), json_field(&release, "tag")),
        (
            "Core component".to_owned(),
            json_nested_field(&release, "components", "core"),
        ),
        (
            "Admin CLI component".to_owned(),
            json_nested_field(&release, "components", "cli"),
        ),
        ("Installed Core Admin App".to_owned(), installed_app_version),
        (
            "Release revision".to_owned(),
            json_field(&release, "revision"),
        ),
        ("Build Rust".to_owned(), json_field(&provenance, "rustc")),
        ("Build target".to_owned(), json_field(&provenance, "target")),
        ("Operating system".to_owned(), os_name),
        (
            "Kernel".to_owned(),
            local_command("/usr/bin/uname", &["-r"]),
        ),
        (
            "Architecture".to_owned(),
            local_command("/usr/bin/uname", &["-m"]),
        ),
        (
            "Hostname".to_owned(),
            local_command("/usr/bin/hostname", &[]),
        ),
        (
            "Running Core Admin App".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        ),
    ];
    Ok(SystemResponse { values })
}

pub fn runtime_status() -> RuntimeStatus {
    let running_version = env!("CARGO_PKG_VERSION").to_owned();
    let installed_version = trusted_installed_version();
    let version_changed = installed_version
        .as_deref()
        .is_some_and(|installed| installed != running_version);
    let executable_replaced = active_executable_was_replaced().unwrap_or(false);
    RuntimeStatus {
        running_version,
        installed_version,
        restart_required: restart_required(
            cfg!(feature = "custom-protocol"),
            version_changed,
            executable_replaced,
        ),
    }
}

fn status(session: &SessionManager) -> AdminResult<StatusReport> {
    parse_json(&session.run(BrokerRequest::Status)?.stdout)
}

fn update_values(session: &SessionManager, check: bool) -> AdminResult<BTreeMap<String, String>> {
    Ok(
        parse_json::<UpdateEnvelope>(&session.run(BrokerRequest::UpdateStatus { check })?.stdout)?
            .values,
    )
}

fn parse_json<T: DeserializeOwned>(value: &str) -> AdminResult<T> {
    serde_json::from_str(value)
        .map_err(|_| "trusted admin boundary returned invalid structured output".to_owned())
}

pub(crate) struct ProgramOutput {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

fn run_direct(program: &str, args: &[&str], timeout: Duration) -> AdminResult<ProgramOutput> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("trusted administration broker is not privileged".to_owned());
    }
    verify_root_executable(program)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C.UTF-8");
    let mut child = command
        .spawn()
        .map_err(|_| "could not start trusted administration operation".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "missing protected stdout channel".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "missing protected stderr channel".to_owned())?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let status = child
        .wait_timeout(timeout)
        .map_err(|_| "could not monitor trusted admin operation".to_owned())?;
    let timed_out = status.is_none();
    let status = match status {
        Some(status) => Some(status),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "protected stdout reader failed".to_owned())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "protected stderr reader failed".to_owned())??;
    if timed_out {
        return Err("trusted admin operation timed out".to_owned());
    }
    let status = status.ok_or_else(|| "trusted admin operation timed out".to_owned())?;
    let output = ProgramOutput {
        success: status.success(),
        stdout: safe_text(&stdout),
        stderr: safe_text(&stderr),
    };
    if output.success {
        Ok(output)
    } else {
        Err(if output.stderr.trim().is_empty() {
            "authorization was cancelled or the trusted operation failed".to_owned()
        } else {
            output.stderr.clone()
        })
    }
}

pub(crate) fn run_broker_request(request: BrokerRequest) -> AdminResult<ProgramOutput> {
    let (program, args, timeout) = match request {
        BrokerRequest::Status => (
            ADMIN,
            vec!["--json".to_owned(), "status".to_owned()],
            Duration::from_secs(120),
        ),
        BrokerRequest::Health => (
            ADMIN,
            vec!["--json".to_owned(), "health".to_owned()],
            Duration::from_secs(180),
        ),
        BrokerRequest::UpdateStatus { check } => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "update".to_owned(),
                if check { "--check" } else { "--status" }.to_owned(),
            ],
            Duration::from_secs(180),
        ),
        BrokerRequest::UpdateMutation { request } => {
            let args = match request {
                UpdateMutation::Latest => vec!["update".to_owned(), "--latest".to_owned()],
                UpdateMutation::InstallVersion { version } => {
                    validate_version(&version)?;
                    vec!["update".to_owned(), "--version".to_owned(), version]
                }
                UpdateMutation::Rollback => vec![
                    "update".to_owned(),
                    "--rollback".to_owned(),
                    "--yes".to_owned(),
                ],
            };
            (ADMIN, args, Duration::from_secs(1_800))
        }
        BrokerRequest::AgentsStatus => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "agents".to_owned(),
                "status".to_owned(),
            ],
            Duration::from_secs(120),
        ),
        BrokerRequest::AgentTree => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "agents".to_owned(),
                "tree".to_owned(),
            ],
            Duration::from_secs(120),
        ),
        BrokerRequest::AgentAction { update } => (
            ADMIN,
            vec![
                "agents".to_owned(),
                if update { "update" } else { "check" }.to_owned(),
            ],
            Duration::from_secs(900),
        ),
        BrokerRequest::Models => (
            ADMIN,
            vec!["--json".to_owned(), "models".to_owned(), "list".to_owned()],
            Duration::from_secs(120),
        ),
        BrokerRequest::Usage => (
            ADMIN,
            vec!["--json".to_owned(), "usage".to_owned()],
            Duration::from_secs(120),
        ),
        BrokerRequest::ModelMutation { request } => {
            let args = match request {
                ModelMutation::Refresh => vec!["models".to_owned(), "refresh".to_owned()],
                ModelMutation::Enable { provider, model } => {
                    validate_model(&model)?;
                    vec![
                        "models".to_owned(),
                        "enable".to_owned(),
                        provider.cli_name().to_owned(),
                        model,
                    ]
                }
                ModelMutation::Disable { provider, model } => {
                    validate_model(&model)?;
                    vec![
                        "models".to_owned(),
                        "disable".to_owned(),
                        provider.cli_name().to_owned(),
                        model,
                    ]
                }
            };
            (ADMIN, args, Duration::from_secs(900))
        }
        BrokerRequest::Credentials => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "credentials".to_owned(),
                "list".to_owned(),
            ],
            Duration::from_secs(120),
        ),
        BrokerRequest::Logs { query } => {
            if !(1..=2_000).contains(&query.lines) {
                return Err("log line count must be between 1 and 2000".to_owned());
            }
            (
                ADMIN,
                vec![
                    "--json".to_owned(),
                    "logs".to_owned(),
                    query.service.cli_name().to_owned(),
                    "--lines".to_owned(),
                    query.lines.to_string(),
                ],
                Duration::from_secs(120),
            )
        }
        BrokerRequest::Touch | BrokerRequest::Shutdown => {
            return Err("invalid privileged operation".to_owned())
        }
    };
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_direct(program, &borrowed, timeout)
}

fn read_bounded(mut reader: impl Read) -> AdminResult<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| "could not read trusted operation output".to_owned())?;
        if count == 0 {
            break;
        }
        if result.len() < OUTPUT_LIMIT {
            let remaining = OUTPUT_LIMIT - result.len();
            result.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
    Ok(result)
}

pub(crate) fn verify_root_executable(path: &str) -> AdminResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "trusted administration executable is unavailable".to_owned())?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(
            "trusted administration executable has unsafe ownership or permissions".to_owned(),
        );
    }
    Ok(())
}

fn operation(output: ProgramOutput, summary: &str) -> AdminResult<OperationResult> {
    let detail = output
        .stderr
        .lines()
        .chain(output.stdout.lines())
        .filter(|line| !line.trim().is_empty())
        .take(40)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(OperationResult {
        success: true,
        summary: summary.to_owned(),
        detail: if detail.is_empty() {
            "The trusted operation completed successfully.".to_owned()
        } else {
            detail
        },
    })
}

pub(crate) fn safe_text(bytes: &[u8]) -> String {
    sanitize(&String::from_utf8_lossy(bytes), OUTPUT_LIMIT)
}

pub(crate) fn sanitize_error(value: &str) -> String {
    sanitize(value, 4096)
}

fn validate_version(value: &str) -> AdminResult<()> {
    let mut parts = value
        .strip_prefix('v')
        .ok_or_else(|| "version must use vMAJOR.MINOR.PATCH".to_owned())?
        .split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none();
    if valid {
        Ok(())
    } else {
        Err("version must use vMAJOR.MINOR.PATCH".to_owned())
    }
}

fn validate_model(value: &str) -> AdminResult<()> {
    if !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
        })
    {
        Ok(())
    } else {
        Err("model identifier contains unsupported characters".to_owned())
    }
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn safe_label(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= 80
        && value.chars().all(|character| !character.is_control())
}

fn safe_source_timestamp(value: &str) -> bool {
    (20..=40).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'T' | b'Z' | b'+' | b'.')
        })
}

fn read_fixed(path: &str, limit: u64) -> AdminResult<String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "system metadata is unavailable".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        return Err("system metadata path is unsafe".to_owned());
    }
    fs::read_to_string(path).map_err(|_| "system metadata could not be read".to_owned())
}

fn trusted_installed_version() -> Option<String> {
    let metadata = fs::symlink_metadata(CORE_ADMIN_VERSION).ok()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.len() > 128
    {
        return None;
    }
    let version = fs::read_to_string(CORE_ADMIN_VERSION).ok()?;
    let version = version.trim();
    valid_component_version(version).then(|| version.to_owned())
}

fn active_executable_was_replaced() -> Option<bool> {
    let installed = fs::symlink_metadata(CORE_ADMIN_BINARY).ok()?;
    if installed.file_type().is_symlink()
        || !installed.is_file()
        || installed.uid() != 0
        || installed.permissions().mode() & 0o022 != 0
    {
        return None;
    }
    let running = fs::metadata("/proc/self/exe").ok()?;
    Some(installed.dev() != running.dev() || installed.ino() != running.ino())
}

fn valid_component_version(value: &str) -> bool {
    let mut parts = value.split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn restart_required(production: bool, version_changed: bool, executable_replaced: bool) -> bool {
    production && (version_changed || executable_replaced)
}

fn local_command(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .ok()
        .filter(|output| output.status.success() && output.stdout.len() < 4096)
        .map(|output| sanitize(String::from_utf8_lossy(&output.stdout).trim(), 256))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn json_field(value: &Option<serde_json::Value>, key: &str) -> String {
    value
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(|value| sanitize(value, 256))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn json_nested_field(value: &Option<serde_json::Value>, parent: &str, key: &str) -> String {
    value
        .as_ref()
        .and_then(|value| value.get(parent))
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(|value| sanitize(value, 256))
        .unwrap_or_else(|| "unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_inputs_are_strict() {
        assert!(validate_version("v1.2.3").is_ok());
        assert!(validate_version("latest;sh").is_err());
        assert!(validate_model("gpt-5.1-mini").is_ok());
        assert!(validate_model("model\n--flag").is_err());
    }

    #[test]
    fn restart_detection_is_enabled_only_for_replaced_production_clients() {
        assert!(valid_component_version("0.1.2"));
        assert!(!valid_component_version("v0.1.2"));
        assert!(restart_required(true, true, false));
        assert!(restart_required(true, false, true));
        assert!(!restart_required(true, false, false));
        assert!(!restart_required(false, true, true));
    }

    #[test]
    fn manifest_schema_retains_only_safe_projection() {
        let manifest: SafeManifest = serde_json::from_str(r#"{"version":1,"bundle_id":"bundle-test","agents":[{"id":"research","name":"Research","group":"Development","model_policy":"research","profile_lines":142,"source_updated_at":"2026-08-29T14:32:00+02:00","instructions":"never retain"}]}"#).unwrap();
        assert_eq!(manifest.agents[0].name.as_deref(), Some("Research"));
        assert_eq!(manifest.agents[0].profile_lines, Some(142));
        assert!(safe_source_timestamp(
            manifest.agents[0].source_updated_at.as_deref().unwrap()
        ));
        assert!(!format!("{manifest:?}").contains("never retain"));
    }

    #[test]
    fn agent_records_require_the_active_bundle_and_preserve_safe_tree_metadata() {
        let bundle = AgentBundle {
            id: "bundle-test".to_owned(),
            agent_count: 1,
        };
        let manifest: SafeManifest = serde_json::from_str(r#"{"version":1,"bundle_id":"bundle-test","agents":[{"id":"research","name":"Research","group":"Development","model_policy":"research","profile_lines":142,"source_updated_at":"2026-08-29T14:32:00+02:00"}]}"#).unwrap();
        let (manifest_bundle, records) = safe_agent_records(&bundle, manifest).unwrap();
        assert_eq!(manifest_bundle, bundle.id);
        assert_eq!(records[0].group, "Development");
        assert_eq!(records[0].profile_lines, Some(142));

        let stale: SafeManifest = serde_json::from_str(
            r#"{"version":1,"bundle_id":"bundle-old","agents":[]}"#,
        )
        .unwrap();
        assert!(safe_agent_records(&bundle, stale).is_err());
    }

    #[test]
    fn legacy_model_json_remains_readable_with_unknown_pricing() {
        let policy: ModelPolicy = serde_json::from_str(
            r#"{"models":[{"provider":"ollama-cloud","model":"future-model","enabled":false,"source":"provider_api"}]}"#,
        )
        .unwrap();
        assert_eq!(policy.models[0].price_status, "unknown");
        assert_eq!(policy.models[0].input_per_million_usd, None);
    }

    #[test]
    fn usage_report_contains_aggregates_only() {
        let report: UsageReport = serde_json::from_str(
            r#"{"period":"current_calendar_month","generated_at_unix":1,"budget_eur":50.0,"spent_eur":1.0,"remaining_eur":49.0,"over_budget":false,"reserved_eur":0.0,"remaining_hard_eur":49.0,"above_soft_budget":false,"requests":2,"input_tokens":10,"output_tokens":5,"cache_read_tokens":3,"cache_write_tokens":0,"total_tokens":18,"by_backend":[],"by_model":[],"daily":[],"pricing":{"source":"fixture","updated_at":"2026-09-01"}}"#,
        )
        .unwrap();
        assert_eq!(report.total_tokens, 18);
    }
}
