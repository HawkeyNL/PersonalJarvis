use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wait_timeout::ChildExt;

use crate::devices;
use crate::logs::{parse_lines, sanitize, LogRecord};
use crate::session::{BrokerRequest, SessionManager};

const ADMIN: &str = "/usr/local/sbin/jarvis";
const CORE_ADMIN_BINARY: &str = "/usr/bin/jarvis-core-admin";
const CORE_ADMIN_VERSION: &str = "/usr/share/jarvis-core-admin/version";
const PKEXEC: &str = "/usr/bin/pkexec";
const PTYXIS: &str = "/usr/bin/ptyxis";
const GNOME_TERMINAL: &str = "/usr/bin/gnome-terminal";
const OUTPUT_LIMIT: usize = 1_048_576;
// Deliberately unbounded in practice: after pkexec the child runs as root, so
// this unprivileged app cannot kill it (EPERM) and a timeout would only
// misreport a migration that keeps running. The updater bounds its own steps.
const MIGRATE_WAIT: Duration = Duration::from_secs(u32::MAX as u64);

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
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateMutation {
    Latest,
    InstallVersion { version: String },
    Rollback,
    Migrate { version: String },
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
    Huggingface,
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
            Self::Huggingface => "huggingface",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ModelMutation {
    Refresh {
        #[serde(default)]
        provider: Option<ModelProvider>,
    },
    Enable {
        provider: ModelProvider,
        model: String,
    },
    Disable {
        provider: ModelProvider,
        model: String,
    },
    SetRoute {
        provider: ModelProvider,
        model: String,
        route: String,
    },
    /// Record an exact subscription pair as discovered (disabled).
    Register {
        provider: ModelProvider,
        model: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteTier {
    Cheap,
    Default,
    Hard,
}

impl RouteTier {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Default => "default",
            Self::Hard => "hard",
        }
    }
}

/// Providers a routed chain may name (`jarvis_llm::ROUTING_PROVIDERS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteProvider {
    AnthropicApi,
    OpenaiApi,
    DeepseekApi,
    XaiApi,
    ZaiApi,
    Ollama,
    OllamaCloud,
    Huggingface,
    ClaudeCli,
    CodexCli,
}

impl RouteProvider {
    fn cli_name(self) -> &'static str {
        match self {
            Self::AnthropicApi => "anthropic-api",
            Self::OpenaiApi => "openai-api",
            Self::DeepseekApi => "deepseek-api",
            Self::XaiApi => "xai-api",
            Self::ZaiApi => "zai-api",
            Self::Ollama => "ollama",
            Self::OllamaCloud => "ollama-cloud",
            Self::Huggingface => "huggingface",
            Self::ClaudeCli => "claude-cli",
            Self::CodexCli => "codex-cli",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteEntry {
    pub provider: RouteProvider,
    pub model: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaidApi {
    Allowed,
    Off,
}

impl PaidApi {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TierRoute {
    pub chain: Vec<RouteEntry>,
    pub metered_after_subscription: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRouting {
    pub version: u32,
    pub paid_api: PaidApi,
    pub tiers: BTreeMap<RouteTier, TierRoute>,
}

/// `jarvis --json models route list`: the stored document, or why Core
/// cannot use it (Core then runs the built-in order without paid APIs).
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingReport {
    pub routing: Option<ModelRouting>,
    pub routing_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteMutation {
    Set {
        tier: RouteTier,
        chain: Vec<RouteEntry>,
        metered_after_subscription: bool,
    },
    Reset {
        tier: RouteTier,
    },
    PaidApi {
        state: PaidApi,
    },
}

const MAX_ROUTE_CHAIN: usize = 9;

/// Fixed `jarvis models register …` argv. Subscriptions have no model
/// catalog, so the owner records the exact pair; it stays disabled.
fn register_arguments(provider: ModelProvider, model: String) -> AdminResult<Vec<String>> {
    if !matches!(provider, ModelProvider::ClaudeCli | ModelProvider::CodexCli) {
        return Err("only claude-cli and codex-cli models can be registered".to_owned());
    }
    // Same rule as the subscription worker request; never option-like.
    if model.is_empty()
        || model.len() > 80
        || model.starts_with('-')
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("model identifier contains unsupported characters".to_owned());
    }
    Ok(vec![
        "models".to_owned(),
        "register".to_owned(),
        provider.cli_name().to_owned(),
        model,
    ])
}

/// Fixed `jarvis models route …` argv. The trusted helper validates the full
/// document again and requires every pair to be discovered.
fn route_mutation_arguments(request: RouteMutation) -> AdminResult<Vec<String>> {
    let mut args = vec!["models".to_owned(), "route".to_owned()];
    match request {
        RouteMutation::Set {
            tier,
            chain,
            metered_after_subscription,
        } => {
            if chain.is_empty() || chain.len() > MAX_ROUTE_CHAIN {
                return Err("a tier chain has 1 to 9 entries".to_owned());
            }
            args.extend(["set".to_owned(), tier.cli_name().to_owned()]);
            for (index, entry) in chain.iter().enumerate() {
                validate_model(&entry.model)?;
                // Never let a model id look like an option to the CLI.
                if entry.model.starts_with('-') {
                    return Err("model identifier contains unsupported characters".to_owned());
                }
                if chain[..index].contains(entry) {
                    return Err("a tier chain cannot repeat a model".to_owned());
                }
                args.extend([entry.provider.cli_name().to_owned(), entry.model.clone()]);
            }
            if metered_after_subscription {
                args.push("--metered-after-subscription".to_owned());
            }
        }
        RouteMutation::Reset { tier } => {
            args.extend(["reset".to_owned(), tier.cli_name().to_owned()]);
        }
        RouteMutation::PaidApi { state } => {
            args.extend(["paid-api".to_owned(), state.cli_name().to_owned()]);
        }
    }
    Ok(args)
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
    #[serde(default)]
    pub route: Option<String>,
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
    #[serde(default)]
    pub pricing_notes: String,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AiAccountRecord {
    pub provider: String,
    pub worker: String,
    pub state: String,
    pub billing: String,
    pub runtime: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AiAccountProvider {
    Claude,
    Codex,
    #[serde(rename = "codex-chat")]
    CodexChat,
}

impl AiAccountProvider {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::CodexChat => "codex-chat",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AiAccountAction {
    Connect,
    Test,
    Reconnect,
    Disconnect,
}

impl AiAccountAction {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Test => "test",
            Self::Reconnect => "reconnect",
            Self::Disconnect => "disconnect",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialProvider {
    Anthropic,
    Openai,
    Deepseek,
    Xai,
    Zai,
    OllamaCloud,
    Huggingface,
    Jev,
}

impl CredentialProvider {
    fn cli_name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Deepseek => "deepseek",
            Self::Xai => "xai",
            Self::Zai => "zai",
            Self::OllamaCloud => "ollama-cloud",
            Self::Huggingface => "huggingface",
            Self::Jev => "jev",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Openai => "OpenAI",
            Self::Deepseek => "DeepSeek",
            Self::Xai => "xAI",
            Self::Zai => "Z.ai",
            Self::OllamaCloud => "Ollama Cloud",
            Self::Huggingface => "Hugging Face",
            Self::Jev => "TypeSafe Jev",
        }
    }

    fn from_os(value: &OsStr) -> Option<Self> {
        match value.to_str()? {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "deepseek" => Some(Self::Deepseek),
            "xai" => Some(Self::Xai),
            "zai" => Some(Self::Zai),
            "ollama-cloud" => Some(Self::OllamaCloud),
            "huggingface" => Some(Self::Huggingface),
            "jev" => Some(Self::Jev),
            _ => None,
        }
    }
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
    if let UpdateMutation::Migrate { version } = &request {
        return migrate(session, version);
    }
    operation(
        session.run(BrokerRequest::UpdateMutation { request })?,
        "Core operation completed",
    )
}

/// A schema migration never uses the retained broker grant: like device
/// management it starts a fresh PolicyKit authentication (`auth_admin`, never
/// `_keep`), even while this application's session is unlocked.
fn migrate(session: &SessionManager, version: &str) -> AdminResult<OperationResult> {
    root_guard()?;
    session.require_active()?;
    let args = migrate_arguments(version)?;
    verify_root_executable(ADMIN)?;
    devices::verify_installed_policy()?;
    operation(
        run_checked_command(PKEXEC, &args, MIGRATE_WAIT)?,
        "Core schema migration completed",
    )
}

fn migrate_arguments(version: &str) -> AdminResult<Vec<&str>> {
    validate_version(version)?;
    Ok(vec![ADMIN, "update", "--migrate", version, "--yes"])
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

#[derive(Debug, Deserialize, Serialize)]
pub struct HfProvidersResponse {
    pub model: String,
    pub routes: Vec<String>,
    pub providers: Vec<HfProviderRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HfProviderRecord {
    pub provider: String,
    pub status: String,
    pub context_length: Option<u64>,
    pub input_per_million_usd: Option<f64>,
    pub output_per_million_usd: Option<f64>,
    pub supports_tools: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub first_token_latency_ms: Option<f64>,
    pub throughput: Option<f64>,
}

pub fn model_providers(
    session: &SessionManager,
    model: String,
) -> AdminResult<HfProvidersResponse> {
    validate_model(&model)?;
    parse_json(&session.run(BrokerRequest::ModelProviders { model })?.stdout)
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

pub fn model_routes(session: &SessionManager) -> AdminResult<RoutingReport> {
    parse_json(&session.run(BrokerRequest::ModelRoutes)?.stdout)
}

pub fn model_route_mutation(
    session: &SessionManager,
    request: RouteMutation,
) -> AdminResult<OperationResult> {
    operation(
        session.run(BrokerRequest::ModelRouteMutation { request })?,
        "Model routing updated",
    )
}

pub fn credentials(session: &SessionManager) -> AdminResult<Vec<CredentialRecord>> {
    parse_json(&session.run(BrokerRequest::Credentials)?.stdout)
}

pub fn ai_accounts(session: &SessionManager) -> AdminResult<Vec<AiAccountRecord>> {
    validate_ai_accounts(parse_json(&session.run(BrokerRequest::Accounts)?.stdout)?)
}

/// Exactly one row per known login, each with its fixed worker identity.
fn validate_ai_accounts(rows: Vec<AiAccountRecord>) -> AdminResult<Vec<AiAccountRecord>> {
    let providers: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.provider.as_str()).collect();
    if rows.len() != 3
        || providers.len() != rows.len()
        || rows.iter().any(|row| {
            !matches!(
                (row.provider.as_str(), row.worker.as_str()),
                ("claude", "jarvis-claude") | ("codex", "jarvis-codex") | ("codex-chat", "jarvis-codex-chat")
            )
                || !matches!(
                    row.state.as_str(),
                    "connected"
                        | "logged_out"
                        | "runtime_missing"
                        | "wrong_auth_mode"
                        | "incompatible_runtime"
                        | "unhealthy"
                )
                || !matches!(row.billing.as_str(), "subscription" | "unverified" | "overage_unverified")
                || !matches!(
                    row.runtime.as_str(),
                    "inactive" | "socket_ready" | "active" | "unavailable"
                )
        })
    {
        return Err("AI account status contained unexpected metadata".to_owned());
    }
    Ok(rows)
}

pub fn ai_account_action(
    session: &SessionManager,
    provider: AiAccountProvider,
    action: AiAccountAction,
) -> AdminResult<OperationResult> {
    session.require_active()?;
    let entry_binary = credential_entry_binary()?;
    let mut args = vec![
        entry_binary.into_os_string(),
        OsString::from("--account-entry"),
        OsString::from(action.cli_name()),
        OsString::from(provider.cli_name()),
    ];
    let (program, mut terminal_args) = if Path::new(PTYXIS).exists() {
        verify_root_executable(PTYXIS)?;
        (PTYXIS, vec![OsString::from("--title=Jarvis AI account"), OsString::from("--")])
    } else if Path::new(GNOME_TERMINAL).exists() {
        verify_root_executable(GNOME_TERMINAL)?;
        (GNOME_TERMINAL, vec![OsString::from("--wait"), OsString::from("--title=Jarvis AI account"), OsString::from("--")])
    } else {
        return Err("no supported GNOME account terminal is installed".to_owned());
    };
    terminal_args.append(&mut args);
    let mut command = Command::new(program);
    command.args(terminal_args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    configure_desktop_environment(&mut command);
    if !command.status().map_err(|_| "could not open trusted account terminal".to_owned())?.success() {
        return Err("AI account operation was cancelled or did not complete".to_owned());
    }
    Ok(OperationResult {
        success: true,
        summary: format!("{} account operation finished", provider.cli_name()),
        detail: "The trusted terminal closed; refresh the status to verify the result.".to_owned(),
    })
}

pub fn ai_account_entry(action: &OsStr, provider: &OsStr) -> AdminResult<()> {
    root_guard()?;
    let action = match action.to_str() {
        Some("connect") => AiAccountAction::Connect,
        Some("test") => AiAccountAction::Test,
        Some("reconnect") => AiAccountAction::Reconnect,
        Some("disconnect") => AiAccountAction::Disconnect,
        _ => return Err("unsupported AI account operation".to_owned()),
    };
    let provider = match provider.to_str() {
        Some("claude") => AiAccountProvider::Claude,
        Some("codex") => AiAccountProvider::Codex,
        Some("codex-chat") => AiAccountProvider::CodexChat,
        _ => return Err("unsupported AI account provider".to_owned()),
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return Err("AI account operation requires a controlling terminal".to_owned());
    }
    verify_root_executable(PKEXEC)?;
    verify_root_executable(ADMIN)?;
    println!("Jarvis AI account · {} · {}", provider.cli_name(), action.cli_name());
    println!("Provider authentication stays inside the dedicated worker identity.");
    let status = Command::new(PKEXEC)
        .arg(ADMIN)
        .arg("accounts")
        .arg(action.cli_name())
        .arg(provider.cli_name())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C.UTF-8")
        .status()
        .map_err(|_| "could not start trusted AI account operation".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err("AI account operation did not complete".to_owned())
    }
}

pub fn credential_set(
    session: &SessionManager,
    provider: CredentialProvider,
) -> AdminResult<OperationResult> {
    session.require_active()?;
    launch_credential_terminal(provider)?;
    Ok(OperationResult {
        success: true,
        summary: format!("{} credential setup finished", provider.label()),
        detail: "The trusted terminal closed. Credential status has been refreshed.".to_owned(),
    })
}

pub fn credential_entry(provider: &OsStr) -> AdminResult<()> {
    root_guard()?;
    let provider = CredentialProvider::from_os(provider)
        .ok_or_else(|| "unsupported credential provider".to_owned())?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return Err("credential entry requires an interactive controlling terminal".to_owned());
    }
    verify_root_executable(PKEXEC)?;
    verify_root_executable(ADMIN)?;

    println!("Jarvis credential setup · {}", provider.label());
    println!("The secret is read invisibly by the trusted Jarvis credential helper.");
    let status = Command::new(PKEXEC)
        .arg(ADMIN)
        .arg("credentials")
        .arg("set")
        .arg(provider.cli_name())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C.UTF-8")
        .status()
        .map_err(|_| "could not start the trusted credential operation".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err("credential setup was cancelled or did not complete".to_owned())
    }
}

fn launch_credential_terminal(provider: CredentialProvider) -> AdminResult<()> {
    let entry_binary = credential_entry_binary()?;
    let (program, arguments) = credential_terminal_command(provider, &entry_binary)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_desktop_environment(&mut command);
    let status = command
        .status()
        .map_err(|_| "could not open the protected credential terminal".to_owned())?;
    if status.success() {
        Ok(())
    } else {
        Err("credential setup was cancelled or did not complete".to_owned())
    }
}

fn configure_desktop_environment(command: &mut Command) {
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8");
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn credential_terminal_command(
    provider: CredentialProvider,
    entry_binary: &Path,
) -> AdminResult<(&'static str, Vec<OsString>)> {
    let common = credential_entry_arguments(provider, entry_binary);
    if Path::new(PTYXIS).exists() {
        verify_root_executable(PTYXIS)?;
        return Ok((
            PTYXIS,
            vec![
                OsString::from("--title=Jarvis credential setup"),
                OsString::from("--"),
            ]
            .into_iter()
            .chain(common)
            .collect(),
        ));
    }
    if Path::new(GNOME_TERMINAL).exists() {
        verify_root_executable(GNOME_TERMINAL)?;
        return Ok((
            GNOME_TERMINAL,
            vec![
                OsString::from("--wait"),
                OsString::from("--title=Jarvis credential setup"),
                OsString::from("--"),
            ]
            .into_iter()
            .chain(common)
            .collect(),
        ));
    }
    Err("no supported GNOME credential terminal is installed".to_owned())
}

fn credential_entry_binary() -> AdminResult<PathBuf> {
    let path = std::env::current_exe()
        .map_err(|_| "could not resolve the active Core Admin executable".to_owned())?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|_| "active Core Admin executable is unavailable".to_owned())?;
    let owner = metadata.uid();
    let current_user = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || (owner != 0 && owner != current_user)
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err("active Core Admin executable has unsafe ownership or permissions".to_owned());
    }
    Ok(path)
}

fn credential_entry_arguments(provider: CredentialProvider, entry_binary: &Path) -> Vec<OsString> {
    vec![
        entry_binary.as_os_str().to_owned(),
        OsString::from("--credential-entry"),
        OsString::from(provider.cli_name()),
    ]
}

pub fn wait_for_credential_terminal() {
    print!("\nPress Enter to close this trusted terminal…");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
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
    run_checked_command(program, args, timeout)
}

pub(crate) fn run_checked_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> AdminResult<ProgramOutput> {
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
        BrokerRequest::Devices { pending } => (
            ADMIN,
            vec![
                "--json".into(),
                "devices".into(),
                if pending { "pending" } else { "list" }.into(),
            ],
            Duration::from_secs(20),
        ),
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
                UpdateMutation::Migrate { .. } => {
                    return Err(
                        "schema migration requires fresh administrator authentication".to_owned(),
                    )
                }
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
            vec!["--json".to_owned(), "agents".to_owned(), "tree".to_owned()],
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
        BrokerRequest::ModelProviders { model } => {
            validate_model(&model)?;
            (
                ADMIN,
                vec![
                    "--json".to_owned(),
                    "models".to_owned(),
                    "providers".to_owned(),
                    "huggingface".to_owned(),
                    model,
                ],
                Duration::from_secs(120),
            )
        }
        BrokerRequest::Usage => (
            ADMIN,
            vec!["--json".to_owned(), "usage".to_owned()],
            Duration::from_secs(120),
        ),
        BrokerRequest::ModelMutation { request } => {
            let args = match request {
                ModelMutation::Refresh { provider } => {
                    vec!["models".to_owned(), "refresh".to_owned()]
                        .into_iter()
                        .chain(provider.map(|provider| provider.cli_name().to_owned()))
                        .collect()
                }
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
                ModelMutation::SetRoute {
                    provider,
                    model,
                    route,
                } => {
                    validate_model(&model)?;
                    validate_hf_route(&route)?;
                    if !matches!(provider, ModelProvider::Huggingface) {
                        return Err("routes are supported only for huggingface".to_owned());
                    }
                    vec![
                        "models".to_owned(),
                        "set-route".to_owned(),
                        provider.cli_name().to_owned(),
                        model,
                        route,
                    ]
                }
                ModelMutation::Register { provider, model } => register_arguments(provider, model)?,
            };
            (ADMIN, args, Duration::from_secs(900))
        }
        BrokerRequest::ModelRoutes => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "models".to_owned(),
                "route".to_owned(),
                "list".to_owned(),
            ],
            Duration::from_secs(120),
        ),
        BrokerRequest::ModelRouteMutation { request } => (
            ADMIN,
            route_mutation_arguments(request)?,
            Duration::from_secs(900),
        ),
        BrokerRequest::Credentials => (
            ADMIN,
            vec![
                "--json".to_owned(),
                "credentials".to_owned(),
                "list".to_owned(),
            ],
            Duration::from_secs(120),
        ),
        BrokerRequest::Accounts => (
            ADMIN,
            vec!["--json".to_owned(), "accounts".to_owned(), "list".to_owned()],
            Duration::from_secs(30),
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
        || metadata.permissions().mode() & 0o111 == 0
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

fn validate_hf_route(value: &str) -> AdminResult<()> {
    if matches!(value, "auto" | "fastest" | "cheapest" | "preferred")
        || (!value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            }))
    {
        Ok(())
    } else {
        Err("invalid Hugging Face route".to_owned())
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
        assert!(validate_hf_route("groq").is_ok());
        assert!(validate_hf_route("https://evil").is_err());
    }

    #[test]
    fn migrate_maps_to_fixed_arguments_and_never_runs_on_the_session_grant() {
        let request: UpdateMutation =
            serde_json::from_str(r#"{"action":"migrate","version":"v1.2.3"}"#).unwrap();
        let UpdateMutation::Migrate { version } = &request else {
            panic!("expected migrate request");
        };
        assert_eq!(
            migrate_arguments(version).unwrap(),
            vec![ADMIN, "update", "--migrate", "v1.2.3", "--yes"]
        );
        for version in ["v1.2", "latest", "v1.2.3 --latest", "--help"] {
            assert!(migrate_arguments(version).is_err(), "{version}");
        }
        assert!(run_broker_request(BrokerRequest::UpdateMutation { request }).is_err());
        for payload in [
            r#"{"action":"shell","command":"id"}"#,
            r#"{"action":"migrate"}"#,
            r#"{"action":"migrate","version":"v1.2.3","password":"x"}"#,
            r#"{"action":"migrate_staged","version":"v1.2.3"}"#,
        ] {
            assert!(
                serde_json::from_str::<UpdateMutation>(payload).is_err(),
                "{payload}"
            );
        }
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

        let stale: SafeManifest =
            serde_json::from_str(r#"{"version":1,"bundle_id":"bundle-old","agents":[]}"#).unwrap();
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
        assert_eq!(policy.models[0].route, None);
    }

    #[test]
    fn model_route_mutation_is_a_typed_frontend_request() {
        let refresh: ModelMutation = serde_json::from_str(r#"{"action":"refresh"}"#).unwrap();
        assert!(matches!(refresh, ModelMutation::Refresh { provider: None }));
        let request: ModelMutation = serde_json::from_str(
            r#"{"action":"set_route","provider":"huggingface","model":"openai/gpt-oss-20b","route":"groq"}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            ModelMutation::SetRoute { provider: ModelProvider::Huggingface, route, .. } if route == "groq"
        ));
    }

    #[test]
    fn register_is_a_typed_subscription_only_request() {
        let register = |payload: &str| -> AdminResult<Vec<String>> {
            match serde_json::from_str(payload).map_err(|e| e.to_string())? {
                ModelMutation::Register { provider, model } => register_arguments(provider, model),
                _ => Err("not a register request".to_owned()),
            }
        };
        assert_eq!(
            register(r#"{"action":"register","provider":"codex-cli","model":"gpt-6-luna"}"#)
                .unwrap(),
            ["models", "register", "codex-cli", "gpt-6-luna"]
        );
        assert_eq!(
            register(r#"{"action":"register","provider":"claude-cli","model":"claude-opus-5"}"#)
                .unwrap(),
            ["models", "register", "claude-cli", "claude-opus-5"]
        );
        let long = "m".repeat(81);
        for (provider, model) in [
            ("openai-api", "gpt-6-luna"),
            ("ollama-local", "llama3.2"),
            ("codex-cli", "-c"),
            ("codex-cli", "org/model"),
            ("codex-cli", "a b"),
            ("codex-cli", ""),
            ("codex-cli", long.as_str()),
        ] {
            let payload =
                serde_json::json!({"action": "register", "provider": provider, "model": model});
            assert!(
                register(&payload.to_string()).is_err(),
                "{provider} {model}"
            );
        }
    }

    fn route_arguments(payload: &str) -> AdminResult<Vec<String>> {
        let request: RouteMutation = serde_json::from_str(payload).map_err(|e| e.to_string())?;
        route_mutation_arguments(request)
    }

    #[test]
    fn route_mutations_map_to_fixed_argv() {
        assert_eq!(
            route_arguments(
                r#"{"action":"set","tier":"cheap","chain":[{"provider":"zai-api","model":"glm-5.3-flash"},{"provider":"claude-cli","model":"claude-haiku-4-5"}],"metered_after_subscription":true}"#
            )
            .unwrap(),
            [
                "models", "route", "set", "cheap", "zai-api", "glm-5.3-flash", "claude-cli",
                "claude-haiku-4-5", "--metered-after-subscription",
            ]
        );
        assert_eq!(
            route_arguments(
                r#"{"action":"set","tier":"hard","chain":[{"provider":"codex-cli","model":"gpt-6-luna"}],"metered_after_subscription":false}"#
            )
            .unwrap(),
            ["models", "route", "set", "hard", "codex-cli", "gpt-6-luna"]
        );
        assert_eq!(
            route_arguments(r#"{"action":"reset","tier":"hard"}"#).unwrap(),
            ["models", "route", "reset", "hard"]
        );
        assert_eq!(
            route_arguments(r#"{"action":"paid_api","state":"off"}"#).unwrap(),
            ["models", "route", "paid-api", "off"]
        );
    }

    #[test]
    fn route_mutations_reject_unsafe_or_unknown_input() {
        let entry = r#"{"provider":"ollama","model":"llama3.2"}"#;
        let chain = |count| {
            (0..count)
                .map(|index| format!(r#"{{"provider":"ollama","model":"m{index}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        };
        let ten = chain(10);
        for payload in [
            r#"{"action":"set","tier":"turbo","chain":[{"provider":"ollama","model":"a"}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"jev","model":"a"}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama-local","model":"a"}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[],"metered_after_subscription":false}"#.to_owned(),
            format!(r#"{{"action":"set","tier":"cheap","chain":[{ten}],"metered_after_subscription":false}}"#),
            format!(r#"{{"action":"set","tier":"cheap","chain":[{entry},{entry}],"metered_after_subscription":false}}"#),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama","model":"a b"}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama","model":"a\nb"}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama","model":"--metered-after-subscription"}],"metered_after_subscription":false}"#.to_owned(),
            format!(r#"{{"action":"set","tier":"cheap","chain":[{{"provider":"ollama","model":"{}"}}],"metered_after_subscription":false}}"#, "a".repeat(257)),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama","model":"a","enabled":true}],"metered_after_subscription":false}"#.to_owned(),
            r#"{"action":"set","tier":"cheap","chain":[{"provider":"ollama","model":"a"}]}"#.to_owned(),
            r#"{"action":"paid_api","state":"maybe"}"#.to_owned(),
            r#"{"action":"reset","tier":"hard","shell":"id"}"#.to_owned(),
            r#"{"action":"enable","tier":"hard"}"#.to_owned(),
        ] {
            assert!(route_arguments(&payload).is_err(), "{payload}");
        }
        let nine = chain(9);
        assert!(route_arguments(&format!(
            r#"{{"action":"set","tier":"default","chain":[{nine}],"metered_after_subscription":false}}"#
        ))
        .is_ok());
    }

    #[test]
    fn routing_report_parses_the_cli_json() {
        let report: RoutingReport = serde_json::from_str(
            r#"{"routing":{"version":1,"paid_api":"off","tiers":{"cheap":{"chain":[{"provider":"claude-cli","model":"claude-haiku-4-5"}],"metered_after_subscription":false}}},"routing_unavailable_reason":null}"#,
        )
        .unwrap();
        let routing = report.routing.unwrap();
        assert!(matches!(routing.paid_api, PaidApi::Off));
        assert_eq!(
            routing.tiers[&RouteTier::Cheap].chain[0].provider,
            RouteProvider::ClaudeCli
        );
        let unusable: RoutingReport = serde_json::from_str(
            r#"{"routing":null,"routing_unavailable_reason":"routing_invalid"}"#,
        )
        .unwrap();
        assert_eq!(
            unusable.routing_unavailable_reason.as_deref(),
            Some("routing_invalid")
        );
        assert!(serde_json::from_str::<RoutingReport>(
            r#"{"routing":null,"routing_unavailable_reason":null,"raw":"x"}"#
        )
        .is_err());
    }

    #[test]
    fn credential_entry_is_typed_and_contains_no_secret_argument() {
        let provider: CredentialProvider = serde_json::from_str(r#""huggingface""#).unwrap();
        assert!(serde_json::from_str::<CredentialProvider>(r#""arbitrary""#).is_err());
        assert!(CredentialProvider::from_os(OsStr::new("openai")).is_some());
        assert!(CredentialProvider::from_os(OsStr::new("openai;sh")).is_none());
        assert_eq!(
            credential_entry_arguments(provider, Path::new(CORE_ADMIN_BINARY)),
            vec![
                OsString::from(CORE_ADMIN_BINARY),
                OsString::from("--credential-entry"),
                OsString::from("huggingface"),
            ]
        );
        let jev: CredentialProvider = serde_json::from_str(r#""jev""#).unwrap();
        assert_eq!(jev.cli_name(), "jev");
        assert_eq!(
            credential_entry_arguments(jev, Path::new(CORE_ADMIN_BINARY)),
            vec![
                OsString::from(CORE_ADMIN_BINARY),
                OsString::from("--credential-entry"),
                OsString::from("jev"),
            ]
        );
    }

    #[test]
    fn usage_report_contains_aggregates_only() {
        let report: UsageReport = serde_json::from_str(
            r#"{"period":"current_calendar_month","generated_at_unix":1,"budget_eur":50.0,"spent_eur":1.0,"remaining_eur":49.0,"over_budget":false,"reserved_eur":0.0,"remaining_hard_eur":49.0,"above_soft_budget":false,"requests":2,"input_tokens":10,"output_tokens":5,"cache_read_tokens":3,"cache_write_tokens":0,"total_tokens":18,"by_backend":[],"by_model":[],"daily":[],"pricing":{"source":"fixture","updated_at":"2026-09-01"}}"#,
        )
        .unwrap();
        assert_eq!(report.total_tokens, 18);
    }

    #[test]
    fn ai_account_status_rejects_provider_credentials_in_frontend_payload() {
        let safe = r#"{"provider":"claude","worker":"jarvis-claude","state":"connected","billing":"overage_unverified","runtime":"inactive"}"#;
        let row: AiAccountRecord = serde_json::from_str(safe).unwrap();
        assert_eq!(row.billing, "overage_unverified");
        let with_token = safe.replace("\"runtime\"", "\"access_token\":\"canary-secret\",\"runtime\"");
        assert!(serde_json::from_str::<AiAccountRecord>(&with_token).is_err());
    }

    #[test]
    fn ai_accounts_need_one_row_per_login_with_its_own_worker() {
        let row = |provider: &str, worker: &str| AiAccountRecord {
            provider: provider.to_owned(),
            worker: worker.to_owned(),
            state: "logged_out".to_owned(),
            billing: "unverified".to_owned(),
            runtime: "inactive".to_owned(),
        };
        let claude = row("claude", "jarvis-claude");
        let codex = row("codex", "jarvis-codex");
        let chat = row("codex-chat", "jarvis-codex-chat");
        assert!(validate_ai_accounts(vec![claude.clone(), codex.clone(), chat.clone()]).is_ok());
        assert!(validate_ai_accounts(vec![claude.clone(), codex.clone()]).is_err());
        assert!(validate_ai_accounts(vec![claude.clone(), codex.clone(), codex.clone()]).is_err());
        // A Claude CLI outside the reviewed contract is a state, not an error.
        let incompatible = AiAccountRecord {
            state: "incompatible_runtime".to_owned(),
            ..claude.clone()
        };
        assert!(validate_ai_accounts(vec![incompatible, codex.clone(), chat.clone()]).is_ok());
        let unknown = AiAccountRecord {
            state: "surprise".to_owned(),
            ..claude.clone()
        };
        assert!(validate_ai_accounts(vec![unknown, codex.clone(), chat.clone()]).is_err());
        // The chat worker never shares the coding login's identity.
        assert!(validate_ai_accounts(vec![claude, codex, row("codex-chat", "jarvis-codex")]).is_err());
        assert_eq!(
            serde_json::to_string(&AiAccountProvider::CodexChat).unwrap(),
            "\"codex-chat\""
        );
    }
}
