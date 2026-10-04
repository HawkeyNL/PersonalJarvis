//! Root-operated, typed administration surface for a Jarvis Home Node.
//!
//! This binary deliberately accepts a small allowlist of operations.  It never
//! evaluates owner input as a shell command and it is not exposed by the API.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, IsTerminal, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, ExitStatus, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        self, disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use fs2::FileExt;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table},
};
use serde::{Deserialize, Serialize};

mod account_activation;
mod admin_helpers;
mod agent_tree;
mod ai_accounts;
mod credential_setup;
mod laya;
mod local_devices;
mod sandbox_preflight;
mod terminal_ui;
mod tui_app;
mod update_center;
mod usage_insights;

use admin_helpers::{
    compatibility_helper, compatibility_helper_output, trusted_admin_helper_command,
    trusted_health_verifier_command, AdminHelper,
};
#[cfg(test)]
use admin_helpers::{
    explicit_helper_subprocess_mode, resolve_admin_helper, resolve_health_verifier,
};
#[cfg(test)]
use agent_tree::parse_safe_agent_manifest;
use agent_tree::{
    active_agent_tree, active_bundle, AgentBundle, AgentTreeAgent, AgentTreeSnapshot,
};
use terminal_ui::*;
use update_center::*;

const RELEASES_ROOT: &str = "/opt/jarvis/releases";
const CURRENT_RELEASE: &str = "/opt/jarvis/current";
const LIBEXEC: &str = "/usr/local/libexec/jarvis";
const SBIN: &str = "/usr/local/sbin";
const CONFIG_LOCK: &str = "/run/jarvis-admin-config.lock";

#[derive(Debug, Parser)]
#[command(name = "jarvis", about = "Jarvis Home Node administration", version)]
struct Cli {
    /// Emit stable JSON for read-only commands.
    #[arg(long, global = true)]
    json: bool,
    /// Stream subprocess diagnostics instead of capturing non-secret output.
    #[arg(long, global = true)]
    verbose: bool,
    /// Print non-secret terminal lifecycle and exit-reason diagnostics after a TUI closes.
    #[arg(long, global = true)]
    tui_trace: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Read-only Codex/OpenSandbox activation checks; never enables execution.
    Sandbox {
        #[command(subcommand)]
        command: sandbox_preflight::SandboxCommand,
    },
    /// Owner-only subscription account management; never exposed through Core HTTP.
    Accounts {
        #[command(subcommand)]
        command: ai_accounts::AccountsCommand,
    },
    /// Host-local owner device administration; never available through HTTP.
    Devices {
        #[command(subcommand)]
        command: local_devices::DeviceCommand,
    },
    /// Locally provision first-device activation; never grants remote self-enrollment.
    Account {
        #[command(subcommand)]
        command: account_activation::AccountCommand,
    },
    Version,
    /// Report safe, non-secret terminal and Crossterm capabilities.
    TerminalDiagnostics,
    Status,
    Health,
    Logs(LogsArgs),
    Update(UpdateArgs),
    /// One-time migration for installations activated by a legacy updater.
    MigrateInstalledTooling,
    Models(ModelsArgs),
    /// Show bounded, non-secret monthly LLM token and cost statistics.
    Usage,
    Credentials(CredentialsArgs),
    Agents(AgentsArgs),
    Services {
        #[command(subcommand)]
        command: ServicesCommand,
    },
    /// Optional local Laya classifier: install, turn on/off and Core mode;
    /// see docs/LAYA_INTENT_ROUTING.md.
    Laya {
        #[command(subcommand)]
        command: laya::LayaCommand,
    },
    /// Deterministic disk housekeeping; see docs/HOUSEKEEPING.md.
    Housekeeping {
        #[command(subcommand)]
        command: HousekeepingCommand,
    },
    #[cfg(feature = "tui-preview")]
    /// Render fixture-only TUI states without administrative access.
    TuiPreview(TuiPreviewArgs),
}

#[cfg(feature = "tui-preview")]
#[derive(Debug, Args)]
struct TuiPreviewArgs {
    #[arg(value_enum, default_value_t = TuiPreviewScenario::Home)]
    scenario: TuiPreviewScenario,
}

#[cfg(feature = "tui-preview")]
#[derive(Clone, Debug, ValueEnum)]
enum TuiPreviewScenario {
    Home,
    HomeDegraded,
    HealthyStatus,
    DegradedStatus,
    Models,
    Costs,
    Credentials,
    Agents,
    UpdateCenter,
    UpdateCenterFailure,
    UpdateCheckInline,
    UpdateRunning,
    UpdateSuccess,
    UpdateFailureRollback,
    Logs,
    NarrowLong,
}

#[derive(Debug, Args)]
struct LogsArgs {
    target: LogTarget,
    #[arg(long, default_value_t = 80, value_parser = clap::value_parser!(u16).range(1..=9999))]
    lines: u16,
    #[arg(long)]
    follow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum LogTarget {
    Core,
    Surrealdb,
    ConfigBroker,
    CodexBroker,
    Opensandbox,
    Updater,
    AgentsUpdater,
}

impl LogTarget {
    fn unit(&self) -> &'static str {
        match self {
            Self::Core => "jarvis-core.service",
            Self::Surrealdb => "jarvis-surrealdb.service",
            Self::ConfigBroker => "jarvis-config-broker.service",
            Self::CodexBroker => "jarvis-codex-broker.service",
            Self::Opensandbox => "jarvis-opensandbox.service",
            Self::Updater => "jarvis-updater.service",
            Self::AgentsUpdater => "jarvis-private-agent-updater.service",
        }
    }
}

#[derive(Debug, Args)]
struct UpdateArgs {
    #[arg(long, conflicts_with_all = ["version", "check", "status", "rollback"])]
    latest: bool,
    #[arg(long, value_parser = parse_release_tag, conflicts_with_all = ["latest", "check", "status", "rollback"])]
    version: Option<String>,
    #[arg(long, conflicts_with_all = ["latest", "version", "status", "rollback"])]
    check: bool,
    #[arg(long, conflicts_with_all = ["latest", "version", "check", "rollback"])]
    status: bool,
    #[arg(long, conflicts_with_all = ["latest", "version", "check", "status"])]
    rollback: bool,
    /// Stage and migrate a schema-changing release; without a tag, the latest
    /// release is used (interactive only). Never used by the timer.
    #[arg(
        long,
        value_name = "vMAJOR.MINOR.PATCH",
        num_args = 0..=1,
        value_parser = parse_release_tag,
        conflicts_with_all = ["latest", "version", "check", "status", "rollback"]
    )]
    migrate: Option<Option<String>>,
    /// Skip the confirmation of --rollback, or of --migrate with an explicit tag.
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}
#[derive(Debug, Subcommand)]
enum ModelsCommand {
    /// Metadata-only refresh of credentialed providers; used by the hourly timer.
    RefreshConfigured,
    Refresh {
        provider: Option<Provider>,
    },
    List {
        provider: Option<Provider>,
    },
    /// Record one exact subscription pair (claude-cli or codex-cli) as
    /// discovered and disabled; subscriptions have no model catalog.
    Register {
        provider: Provider,
        model: ModelId,
    },
    Enable {
        provider: Provider,
        model: ModelId,
    },
    Disable {
        provider: Provider,
        model: ModelId,
    },
    Show {
        provider: Provider,
        model: ModelId,
    },
    Providers {
        provider: Provider,
        model: ModelId,
    },
    SetRoute {
        provider: Provider,
        model: ModelId,
        route: HfRoute,
    },
    /// Per-tier order of discovered models and the paid API switch. Routing
    /// never enables a model; the allowlist still decides.
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
}

#[derive(Debug, Subcommand)]
enum RouteCommand {
    List,
    Show {
        tier: RouteTier,
    },
    /// Ordered `<provider> <model>` pairs, first choice first (at most 9).
    Set {
        tier: RouteTier,
        #[arg(required = true, num_args = 2..=18)]
        entries: Vec<String>,
        /// Allow a paid API after a subscription in this chain.
        #[arg(long)]
        metered_after_subscription: bool,
    },
    /// Return the tier to the built-in order.
    Reset {
        tier: RouteTier,
    },
    /// `off` removes every paid (metered) API from every tier.
    PaidApi {
        state: PaidApiState,
    },
    /// `on` lets explicit Research requests use the provider-hosted web search
    /// of an enabled subscription (claude-cli, codex-cli). Off by default.
    ResearchWebSearch {
        state: OnOff,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RouteTier {
    Cheap,
    Default,
    Hard,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PaidApiState {
    Allowed,
    Off,
}

fn value_name(value: impl ValueEnum) -> String {
    value
        .to_possible_value()
        .expect("route values are never skipped")
        .get_name()
        .to_owned()
}

/// Typed `jarvis models route` arguments for the compatibility helper.
fn route_arguments(command: RouteCommand) -> Result<Vec<String>> {
    let mut arguments = vec!["route".to_owned()];
    match command {
        RouteCommand::List => arguments.push("list".into()),
        RouteCommand::Show { tier } => arguments.extend(["show".into(), value_name(tier)]),
        RouteCommand::Reset { tier } => arguments.extend(["reset".into(), value_name(tier)]),
        RouteCommand::PaidApi { state } => arguments.extend(["paid-api".into(), value_name(state)]),
        RouteCommand::ResearchWebSearch { state } => {
            arguments.extend(["research-web-search".into(), value_name(state)])
        }
        RouteCommand::Set {
            tier,
            entries,
            metered_after_subscription,
        } => {
            if entries.len() % 2 != 0 {
                bail!("give one or more <provider> <model> pairs");
            }
            arguments.extend(["set".into(), value_name(tier)]);
            for pair in entries.chunks(2) {
                let provider = Provider::from_str(&pair[0], false)
                    .map_err(|_| anyhow::anyhow!("unknown provider"))?;
                let model = pair[1].parse::<ModelId>().map_err(anyhow::Error::msg)?;
                arguments.extend([provider.as_str().to_owned(), model.0]);
            }
            if metered_after_subscription {
                arguments.push("--metered-after-subscription".into());
            }
        }
    }
    Ok(arguments)
}

/// Typed `jarvis models register` arguments; the helper checks them again.
fn register_arguments(provider: Provider, model: ModelId) -> Result<Vec<String>> {
    if !matches!(provider, Provider::ClaudeCli | Provider::CodexCli) {
        bail!("register is only for subscription providers (claude-cli, codex-cli)");
    }
    if !jarvis_llm::claude_worker_protocol::valid_worker_model(&model.0) {
        bail!("invalid model: use 1 to 80 of A-Z a-z 0-9 . _ - and do not start with -");
    }
    Ok(vec![
        "register".to_owned(),
        provider.as_str().to_owned(),
        model.0,
    ])
}

#[derive(Debug, Args)]
struct CredentialsArgs {
    #[command(subcommand)]
    command: CredentialsCommand,
}
#[derive(Debug, Subcommand)]
enum CredentialsCommand {
    List,
    Set { provider: CredentialProvider },
    Test { provider: CredentialProvider },
    Remove { provider: CredentialProvider },
}

#[derive(Clone, Debug, ValueEnum)]
enum Provider {
    #[value(name = "anthropic-api")]
    AnthropicApi,
    #[value(name = "openai-api")]
    OpenaiApi,
    #[value(name = "deepseek-api")]
    DeepseekApi,
    #[value(name = "xai-api")]
    XaiApi,
    #[value(name = "zai-api")]
    ZaiApi,
    Ollama,
    #[value(name = "ollama-cloud")]
    OllamaCloud,
    #[value(name = "claude-cli")]
    ClaudeCli,
    #[value(name = "codex-cli")]
    CodexCli,
    Huggingface,
}
impl Provider {
    fn as_str(&self) -> &'static str {
        match self {
            Self::AnthropicApi => "anthropic-api",
            Self::OpenaiApi => "openai-api",
            Self::DeepseekApi => "deepseek-api",
            Self::XaiApi => "xai-api",
            Self::ZaiApi => "zai-api",
            Self::Ollama => "ollama",
            Self::OllamaCloud => "ollama-cloud",
            Self::ClaudeCli => "claude-cli",
            Self::CodexCli => "codex-cli",
            Self::Huggingface => "huggingface",
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum CredentialProvider {
    Anthropic,
    Openai,
    Deepseek,
    Xai,
    Zai,
    #[value(name = "ollama-cloud")]
    OllamaCloud,
    Huggingface,
    Jev,
}
impl CredentialProvider {
    fn as_str(&self) -> &'static str {
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
}

#[derive(Clone, Debug)]
struct ModelId(String);
impl std::str::FromStr for ModelId {
    type Err = String;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.is_empty() || value.len() > 256 || value.contains(['\n', '\r']) {
            return Err("model must be 1..=256 characters without newlines".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug)]
struct HfRoute(String);

impl std::str::FromStr for HfRoute {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if matches!(value, "auto" | "fastest" | "cheapest" | "preferred")
            || (!value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'
                        || byte == b'_'
                }))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err("invalid Hugging Face route".into())
        }
    }
}

#[derive(Debug, Args)]
struct AgentsArgs {
    #[command(subcommand)]
    command: AgentsCommand,
}

#[derive(Debug, Subcommand)]
enum AgentsCommand {
    Status,
    /// Emit the bounded, non-secret active agent registry projection.
    #[command(hide = true)]
    Tree,
    Check,
    Update,
    Rollback {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HousekeepingCommand {
    /// Read-only: what would be removed, sizes, disk free and the last run.
    Status,
    /// A dry run unless --apply; the daily timer runs the --apply form.
    Run {
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ServicesCommand {
    Status,
}

#[derive(Clone, Debug, Serialize)]
struct StatusReport {
    release: Option<String>,
    services: BTreeMap<&'static str, String>,
    updater_enabled: String,
    agent_bundle: Option<AgentBundle>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("jarvis: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Some(Commands::TerminalDiagnostics)) {
        return terminal_diagnostics(cli.json);
    }
    #[cfg(feature = "tui-preview")]
    if let Some(Commands::TuiPreview(args)) = &cli.command {
        if cli.json {
            bail!("fixture TUI preview does not support --json");
        }
        if !io::stdin().is_terminal() || !terminal_supports_rich_output() {
            bail!("fixture TUI preview requires an interactive terminal with rich output enabled");
        }
        return tui_preview(&args.scenario, cli.tui_trace);
    }
    if cli.command.is_none() && cli.json {
        bail!("bare --json requires an explicit read-only Jarvis command");
    }
    require_root()?;
    let presentation = Presentation::new(cli.json, cli.tui_trace);
    let Some(command) = cli.command else {
        if presentation.interactive && io::stdin().is_terminal() {
            return tui_app::run_live(tui_app::AppView::Overview, presentation.tui_trace);
        }
        bail!("non-interactive use requires an explicit Jarvis command");
    };
    match command {
        Commands::Sandbox { command } => sandbox_preflight::run(command, cli.json),
        Commands::Accounts { command } => ai_accounts::run(command, cli.json),
        Commands::Account { command } => account_activation::run(command, cli.json),
        Commands::Devices { command } => local_devices::run(command, cli.json),
        Commands::Version => version(&presentation),
        Commands::TerminalDiagnostics => unreachable!("handled before root-only commands"),
        Commands::Status => status(&presentation),
        Commands::Health => health(&presentation, cli.verbose),
        Commands::Logs(args) => logs(args, &presentation),
        Commands::Update(args) => update(args, &presentation, cli.verbose),
        Commands::MigrateInstalledTooling => migrate_installed_tooling(),
        Commands::Models(args) => models(args, &presentation, cli.verbose),
        Commands::Usage => usage_insights::usage(presentation.json),
        Commands::Credentials(args) => credentials(args, &presentation, cli.verbose),
        Commands::Agents(args) => agents(args, &presentation, cli.verbose),
        Commands::Services {
            command: ServicesCommand::Status,
        } => services(&presentation),
        Commands::Laya { command } => laya::run(command, cli.json),
        Commands::Housekeeping { command } => {
            let mut helper = trusted_admin_helper_command(AdminHelper::Housekeeping)?;
            helper.args(housekeeping_arguments(&command, cli.json));
            run_command(&mut helper, SubprocessMode::InheritedInteractive)
        }
        #[cfg(feature = "tui-preview")]
        Commands::TuiPreview(_) => unreachable!("handled before root-only commands"),
    }
}

/// Only `run --apply` reaches the deleting mode of the fixed helper.
fn housekeeping_arguments(command: &HousekeepingCommand, json: bool) -> Vec<&'static str> {
    let mode = match command {
        HousekeepingCommand::Run { apply: true } => "apply",
        HousekeepingCommand::Status | HousekeepingCommand::Run { apply: false } => "status",
    };
    if json {
        vec![mode, "--json"]
    } else {
        vec![mode]
    }
}

fn require_root() -> Result<()> {
    if libc_geteuid() != 0 {
        bail!("must run as root (use: sudo jarvis ...)");
    }
    Ok(())
}

// Avoid another dependency solely for this platform-specific, security-critical check.
extern "C" {
    fn geteuid() -> u32;
}
fn libc_geteuid() -> u32 {
    unsafe { geteuid() }
}

fn version(presentation: &Presentation) -> Result<()> {
    let (core_version, manifest_cli_version, manifest_app_version) = active_component_versions()?;
    let installed_app_version = fs::read_to_string("/usr/share/jarvis-core-admin/version")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| valid_component_version(value));
    let active_release = active_release()?;
    let report = serde_json::json!({
        "admin_version": env!("CARGO_PKG_VERSION"),
        "active_core": active_release,
        "active_release": active_release,
        "core_version": core_version,
        "cli_version": env!("CARGO_PKG_VERSION"),
        "manifest_cli_version": manifest_cli_version,
        "core_admin_app_version": installed_app_version,
        "manifest_core_admin_app_version": manifest_app_version,
    });
    if presentation.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!(
            "Jarvis Core:      {}",
            report["core_version"].as_str().unwrap_or("unavailable")
        );
        println!(
            "Jarvis admin CLI: {}",
            report["cli_version"].as_str().unwrap_or("unavailable")
        );
        println!(
            "Core Admin App:   {}",
            report["core_admin_app_version"]
                .as_str()
                .unwrap_or("not installed")
        );
        println!(
            "Active release:   {}",
            report["active_release"].as_str().unwrap_or("unavailable")
        );
    }
    Ok(())
}

fn active_component_versions() -> Result<(Option<String>, Option<String>, Option<String>)> {
    let target = fs::canonicalize(CURRENT_RELEASE).ok();
    let Some(target) = target else {
        return Ok((None, None, None));
    };
    if !target.starts_with(RELEASES_ROOT) {
        bail!("active release is outside the managed release root");
    }
    let data = fs::read_to_string(target.join("release.json"))
        .context("read active release component versions")?;
    let manifest = serde_json::from_str::<serde_json::Value>(&data)?;
    let component = |name: &str| {
        manifest
            .get("components")
            .and_then(|components| components.get(name))
            .and_then(serde_json::Value::as_str)
            .filter(|value| valid_component_version(value))
            .map(str::to_owned)
    };
    Ok((component("core"), component("cli"), component("core_admin")))
}

fn valid_component_version(value: &str) -> bool {
    let mut parts = value.split('.');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn status(presentation: &Presentation) -> Result<()> {
    if !presentation.json && presentation.interactive && io::stdin().is_terminal() {
        return tui_app::run_live(tui_app::AppView::Overview, presentation.tui_trace);
    }
    let report = status_report()?;
    if presentation.json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    presentation.intro("Jarvis Home Node");
    println!(
        "  Release          {}",
        report.release.as_deref().unwrap_or("unavailable")
    );
    for (name, state) in &report.services {
        println!("  {name:<16} {state}");
    }
    if let Some(bundle) = &report.agent_bundle {
        println!(
            "  Agents           {} ({} agents)",
            bundle.id, bundle.agent_count
        );
    }
    println!("  Updater          {}", report.updater_enabled);
    presentation.outro("Status collected without reading secrets");
    Ok(())
}

#[cfg(feature = "tui-preview")]
fn tui_preview(scenario: &TuiPreviewScenario, trace_enabled: bool) -> Result<()> {
    let healthy_status = || StatusReport {
        release: Some("v0.0.14-fixture".to_owned()),
        services: BTreeMap::from([
            ("Core", "active".to_owned()),
            ("SurrealDB", "active".to_owned()),
            ("Config broker", "active".to_owned()),
            ("Codex broker", "active".to_owned()),
            ("OpenSandbox", "active".to_owned()),
        ]),
        updater_enabled: "enabled".to_owned(),
        agent_bundle: Some(AgentBundle {
            id: "fixture-bundle-2026-08-29".to_owned(),
            agent_count: 7,
        }),
    };
    match scenario {
        TuiPreviewScenario::Home => {
            tui_app::run_fixture(tui_app::AppView::Overview, trace_enabled, false)
        }
        TuiPreviewScenario::HomeDegraded => {
            tui_app::run_fixture(tui_app::AppView::Overview, trace_enabled, true)
        }
        TuiPreviewScenario::HealthyStatus => status_tui(&healthy_status(), trace_enabled),
        TuiPreviewScenario::DegradedStatus => {
            let mut report = healthy_status();
            report
                .services
                .insert("Core", "activating (degraded fixture)".to_owned());
            report.services.insert("OpenSandbox", "inactive".to_owned());
            status_tui(&report, trace_enabled)
        }
        TuiPreviewScenario::Models => {
            tui_app::run_fixture(tui_app::AppView::Models, trace_enabled, false)
        }
        TuiPreviewScenario::Costs => {
            tui_app::run_fixture(tui_app::AppView::Usage, trace_enabled, false)
        }
        TuiPreviewScenario::Credentials => table_tui(
            "Jarvis Credentials · fixture (status only)",
            vec!["Provider".to_owned(), "Status".to_owned()],
            vec![
                vec!["openai".to_owned(), "configured".to_owned()],
                vec!["anthropic".to_owned(), "not configured".to_owned()],
                vec![
                    "ollama-local".to_owned(),
                    "no credential required".to_owned(),
                ],
            ],
            trace_enabled,
        ),
        TuiPreviewScenario::Agents => table_tui(
            "Jarvis Agents · fixture",
            vec!["Bundle".to_owned(), "Agents".to_owned()],
            vec![vec!["fixture-bundle-2026-08-29".to_owned(), "7".to_owned()]],
            trace_enabled,
        ),
        TuiPreviewScenario::UpdateCenter => {
            tui_app::run_fixture(tui_app::AppView::Update, trace_enabled, false)
        }
        TuiPreviewScenario::UpdateCenterFailure => {
            tui_app::run_fixture(tui_app::AppView::Update, trace_enabled, true)
        }
        TuiPreviewScenario::UpdateCheckInline => {
            println!("Current:  v0.0.15");
            println!("Latest:   v0.0.16");
            println!("Update:   available");
            Ok(())
        }
        TuiPreviewScenario::UpdateRunning => table_tui(
            "Jarvis Update · running fixture",
            vec!["State".to_owned(), "Latest safe event".to_owned()],
            vec![vec![
                "running".to_owned(),
                "Verifying downloaded artifact checksum…".to_owned(),
            ]],
            trace_enabled,
        ),
        TuiPreviewScenario::UpdateSuccess => table_tui(
            "Jarvis Update · success fixture",
            vec!["State".to_owned(), "Result".to_owned()],
            vec![vec![
                "success".to_owned(),
                "Verified fixture release is ready".to_owned(),
            ]],
            trace_enabled,
        ),
        TuiPreviewScenario::UpdateFailureRollback => table_tui(
            "Jarvis Update · rollback fixture",
            vec!["State".to_owned(), "Result".to_owned()],
            vec![
                vec![
                    "failed".to_owned(),
                    "Fixture readiness probe failed".to_owned(),
                ],
                vec![
                    "rolled back".to_owned(),
                    "Previous fixture release restored".to_owned(),
                ],
            ],
            trace_enabled,
        ),
        TuiPreviewScenario::Logs => table_tui(
            "Jarvis Logs · fixture",
            vec!["jarvis-core.service".to_owned()],
            (1..=40)
                .map(|line| {
                    vec![format!(
                        "fixture log line {line:02}: non-secret status event"
                    )]
                })
                .collect(),
            trace_enabled,
        ),
        TuiPreviewScenario::NarrowLong => {
            let mut report = healthy_status();
            report.release = Some(
                "v0.0.14-fixture-with-a-deliberately-long-non-secret-display-value".to_owned(),
            );
            report.services.insert(
                "Codex broker",
                "active with a deliberately long fixture-only state".to_owned(),
            );
            status_tui(&report, trace_enabled)
        }
    }
}

fn status_report() -> Result<StatusReport> {
    let mut services = BTreeMap::new();
    for (label, unit) in [
        ("Core", "jarvis-core.service"),
        ("SurrealDB", "jarvis-surrealdb.service"),
        ("Config broker", "jarvis-config-broker.service"),
        ("Codex broker", "jarvis-codex-broker.service"),
        ("OpenSandbox", "jarvis-opensandbox.service"),
    ] {
        services.insert(label, systemctl_state("is-active", unit));
    }
    Ok(StatusReport {
        release: active_release()?,
        services,
        updater_enabled: systemctl_state("is-enabled", "jarvis-updater.timer"),
        agent_bundle: active_bundle()?,
    })
}

fn active_release() -> Result<Option<String>> {
    let target = fs::canonicalize(CURRENT_RELEASE).ok();
    let Some(target) = target else {
        return Ok(None);
    };
    if !target.starts_with(RELEASES_ROOT) {
        bail!("active release is outside the managed release root");
    }
    let manifest = target.join("release.json");
    let data = fs::read_to_string(&manifest).context("read active release manifest")?;
    let tag = serde_json::from_str::<serde_json::Value>(&data)?
        .get("tag")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    Ok(tag.filter(|tag| valid_release_tag(tag)))
}

fn health(presentation: &Presentation, verbose: bool) -> Result<()> {
    if !presentation.json && presentation.interactive && io::stdin().is_terminal() && !verbose {
        return tui_app::run_live(tui_app::AppView::Health, presentation.tui_trace);
    }
    if presentation.json {
        let mut command = trusted_health_verifier_command()?;
        let output = command
            .stdin(Stdio::null())
            .output()
            .context("start trusted health verifier")?;
        if !output.status.success() {
            io::stderr().write_all(&output.stderr)?;
            return ensure_success(output.status);
        }
        println!("{}", serde_json::json!({"healthy": true}));
        return Ok(());
    }
    presentation.intro("Jarvis Home Node health");
    let mut command = trusted_health_verifier_command()?;
    run_command(&mut command, SubprocessMode::from_verbose(verbose))?;
    if presentation.interactive && io::stdin().is_terminal() {
        let report = status_report()?;
        let mut rows: Vec<Vec<String>> = report
            .services
            .into_iter()
            .map(|(name, state)| vec![name.to_owned(), state])
            .collect();
        rows.push(vec!["Deployment verifier".to_owned(), "passed".to_owned()]);
        rows.push(vec!["Updater".to_owned(), report.updater_enabled]);
        return table_tui(
            "Jarvis Health",
            vec!["Check".to_owned(), "Result".to_owned()],
            rows,
            presentation.tui_trace,
        );
    }
    presentation.outro("Health verification passed");
    Ok(())
}

fn services(presentation: &Presentation) -> Result<()> {
    if !presentation.json && presentation.interactive && io::stdin().is_terminal() {
        tui_app::run_live(tui_app::AppView::Services, presentation.tui_trace)
    } else {
        status(presentation)
    }
}

fn logs(args: LogsArgs, presentation: &Presentation) -> Result<()> {
    let mut command = trusted_command("journalctl");
    command
        .args(["--no-pager", "-u", args.target.unit(), "-n"])
        .arg(args.lines.to_string());
    if args.follow {
        if presentation.json {
            bail!("--json cannot be combined with streaming logs --follow");
        }
        command.arg("-f");
    }
    if presentation.json {
        let output = command.output().context("read allowlisted Jarvis logs")?;
        ensure_success(output.status)?;
        let lines: Vec<_> = String::from_utf8(output.stdout)?
            .lines()
            .map(str::to_owned)
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "unit": args.target.unit(),
                "lines": lines
            }))?
        );
        Ok(())
    } else if presentation.interactive && io::stdin().is_terminal() {
        if args.follow {
            run_process_tui(
                &mut command,
                "Jarvis Logs",
                format!("Following {}…", args.target.unit()),
                presentation.tui_trace,
            )
        } else {
            let output = command.output().context("read allowlisted Jarvis logs")?;
            ensure_success(output.status)?;
            let rows = String::from_utf8(output.stdout)?
                .lines()
                .map(|line| vec![line.to_owned()])
                .collect();
            table_tui(
                "Jarvis Logs",
                vec![args.target.unit().to_owned()],
                rows,
                presentation.tui_trace,
            )
        }
    } else {
        run_command(&mut command, SubprocessMode::InheritedInteractive)
    }
}

fn update(args: UpdateArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    validate_update_confirmation(&args)?;
    let invocation = UpdateInvocation::from_args(&args);
    if matches!(invocation, UpdateInvocation::Center) {
        if presentation.json {
            bail!("--json update requires an explicit read-only --check or --status operation");
        }
        if presentation.interactive && io::stdin().is_terminal() {
            return tui_app::run_live(tui_app::AppView::Update, presentation.tui_trace);
        }
        bail!(
            "non-interactive update requires --check, --status, --latest, --version, or --rollback"
        );
    }

    if presentation.json
        && !matches!(
            invocation,
            UpdateInvocation::Check | UpdateInvocation::Status
        )
    {
        bail!("--json is supported only for non-mutating update --check/--status");
    }
    if let UpdateInvocation::Migrate(target) = invocation {
        return migrate_update(target, args.yes, presentation);
    }
    if matches!(invocation, UpdateInvocation::Rollback) && !args.yes {
        confirm("Rollback Core to the previous verified release?")?;
    }

    let mut command = trusted_updater_command()?;
    match &invocation {
        UpdateInvocation::Check => {
            command.arg("--check");
        }
        UpdateInvocation::Status => {
            command.arg("--status");
        }
        UpdateInvocation::Latest => {
            command.arg("--latest");
        }
        UpdateInvocation::Version(version) => {
            command.args(["--version", version]);
        }
        UpdateInvocation::Rollback => {
            command.arg("--rollback");
        }
        UpdateInvocation::Center | UpdateInvocation::Migrate(_) => unreachable!("handled above"),
    }

    if matches!(
        invocation,
        UpdateInvocation::Check | UpdateInvocation::Status
    ) {
        let output = command.output().context("start trusted updater")?;
        let check_available =
            matches!(invocation, UpdateInvocation::Check) && output.status.code() == Some(2);
        if !output.status.success() && !check_available {
            io::stderr().write_all(&output.stderr)?;
            return ensure_success(output.status);
        }
        if presentation.json {
            let values = parse_key_value_output(&String::from_utf8(output.stdout)?)?;
            let mode = if matches!(invocation, UpdateInvocation::Check) {
                "check"
            } else {
                "status"
            };
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"mode": mode, "values": values}))?
            );
        } else {
            io::stdout().write_all(&output.stdout)?;
            io::stderr().write_all(&output.stderr)?;
            // Keep stdout strictly key/value; the hint goes to stderr.
            let mut summary = UpdateSummary::default();
            if summary
                .merge_helper_output(&String::from_utf8_lossy(&output.stdout))
                .is_ok()
            {
                if let Ok(tag) = migration_target(&summary) {
                    eprintln!("Schema migration required: run sudo jarvis update --migrate {tag}");
                }
            }
            if check_available {
                io::stdout().flush()?;
                io::stderr().flush()?;
                std::process::exit(2);
            }
        }
        return Ok(());
    }

    run_command(&mut command, SubprocessMode::from_verbose(verbose))
}

/// `--yes` skips owner confirmation only where the target is explicit.
fn validate_update_confirmation(args: &UpdateArgs) -> Result<()> {
    if !args.yes {
        return Ok(());
    }
    match &args.migrate {
        Some(Some(_)) => Ok(()),
        Some(None) => bail!("--migrate --yes requires an explicit vMAJOR.MINOR.PATCH tag"),
        None if args.rollback => Ok(()),
        None => bail!("--yes is accepted only with --rollback or --migrate vMAJOR.MINOR.PATCH"),
    }
}

/// Explicit owner-only schema migration: stage with the installed updater,
/// confirm, then migrate with the staged candidate's own updater. The
/// administration config lock is deliberately not held around either step
/// (jarvis-backup takes the updater lock first; holding both would invert).
fn migrate_update(target: Option<String>, yes: bool, presentation: &Presentation) -> Result<()> {
    // Refuse before anything is downloaded or staged.
    require_confirmation_terminal(yes, io::stdin().is_terminal() && io::stdout().is_terminal())?;
    let tag = match target {
        Some(tag) => tag,
        None => {
            if !presentation.interactive || !io::stdin().is_terminal() {
                bail!("non-interactive --migrate requires an explicit vMAJOR.MINOR.PATCH tag");
            }
            let output = trusted_updater_command()?
                .arg("--check")
                .stdin(Stdio::null())
                .output()
                .context("start trusted updater")?;
            if !output.status.success() && output.status.code() != Some(2) {
                io::stderr().write_all(&output.stderr)?;
                ensure_success(output.status)?;
            }
            let mut summary = UpdateSummary::default();
            summary.merge_helper_output(&String::from_utf8(output.stdout)?)?;
            migration_target(&summary)?
        }
    };

    println!("Step 1/2: downloading and verifying {tag}; nothing is stopped or activated");
    run_migration_step(
        trusted_updater_command()?.args(["--stage", &tag]),
        &format!("an update or backup is running; rerun sudo jarvis update --migrate {tag}"),
    )?;
    println!("{}", migration_notice(&tag));
    if !yes {
        confirm_typed(&format!("Type {tag} to start the migration:"), &tag)?;
    }
    let updater = candidate_updater(Path::new(RELEASES_ROOT), &tag, 0)?;
    let mut command = trusted_command(&updater);
    load_updater_environment(&mut command)?;
    command.args(["--migrate-staged", &tag]);
    ignore_interrupts()?;
    println!("Step 2/2: migrating to {tag}");
    println!(
        "Migration in progress - do not interrupt; services will restart automatically; follow with sudo jarvis logs core"
    );
    run_migration_step(
        &mut command,
        &format!("an update or backup is running; {tag} stays staged; rerun sudo jarvis update --migrate {tag}"),
    )
}

fn require_confirmation_terminal(yes: bool, terminal: bool) -> Result<()> {
    if !yes && !terminal {
        bail!("refusing non-interactive migration; pass --migrate vMAJOR.MINOR.PATCH --yes after reviewing the target");
    }
    Ok(())
}

/// Once the candidate updater may stop services, Ctrl-C or a closed terminal
/// must not kill this CLI and hide the outcome of a migration that keeps
/// running. Ignored dispositions are inherited by the updater as well.
fn ignore_interrupts() -> Result<()> {
    for signal in [libc::SIGINT, libc::SIGHUP] {
        // SAFETY: installs the async-signal-safe SIG_IGN disposition only.
        if unsafe { libc::signal(signal, libc::SIG_IGN) } == libc::SIG_ERR {
            bail!("could not protect the migration from interruption");
        }
    }
    Ok(())
}

fn run_migration_step(command: &mut ProcessCommand, busy: &str) -> Result<()> {
    let status = command
        .stdin(Stdio::null())
        .status()
        .context("start trusted updater")?;
    if status.code() == Some(75) {
        bail!("{busy}");
    }
    ensure_success(status)
}

fn migration_target(summary: &UpdateSummary) -> Result<String> {
    match (summary.schema.as_deref(), summary.latest.as_deref()) {
        (Some("migration required"), Some(latest)) if valid_release_tag(latest) => {
            Ok(latest.to_owned())
        }
        (schema, latest) => bail!(
            "latest release {} offers no supported schema migration (schema: {}); use sudo jarvis update --latest for routine updates",
            latest.unwrap_or("unavailable"),
            schema.unwrap_or("unknown")
        ),
    }
}

fn migration_notice(tag: &str) -> String {
    format!(
        "{tag} changes the database schema. Migrating will:\n\
         - stop Jarvis Core and SurrealDB; connected clients disconnect until Core is ready again;\n\
         - write a cold database snapshot to /var/backups/jarvis-migrations;\n\
         - activate {tag}, which migrates the database on first start;\n\
         - on failure, restore the snapshot and restart the previous release.\n\
         After a successful migration, a binary-only rollback across the schema change is refused."
    )
}

/// The candidate updater runs as root, so it must be the exact staged file:
/// canonical below the release root, and a regular file in a directory that
/// only `owner_uid` (root in production) can modify.
fn candidate_updater(releases_root: &Path, tag: &str, owner_uid: u32) -> Result<PathBuf> {
    if !valid_release_tag(tag) {
        bail!("invalid release tag");
    }
    let release = releases_root.join(tag);
    let updater = release.join("update-core-release");
    let canonical =
        fs::canonicalize(&updater).context("staged candidate updater is unavailable")?;
    if canonical != updater {
        bail!("staged candidate updater does not resolve inside the release root");
    }
    let root = releases_root.to_path_buf();
    for (path, file) in [(&root, false), (&release, false), (&updater, true)] {
        let metadata = fs::symlink_metadata(path).context("inspect staged candidate updater")?;
        let kind_ok = if file {
            metadata.file_type().is_file() && metadata.mode() & 0o100 != 0
        } else {
            metadata.file_type().is_dir()
        };
        if !kind_ok || metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0 {
            bail!("staged candidate updater is not a root-owned, non-writable regular executable");
        }
    }
    Ok(canonical)
}

fn run_process_tui(
    command: &mut ProcessCommand,
    title: &str,
    initial: String,
    trace_enabled: bool,
) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start trusted updater")?;
    let (sender, receiver) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        forward_update_lines(stdout, sender.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward_update_lines(stderr, sender.clone());
    }
    drop(sender);
    let mut trace = TuiTrace::new(trace_enabled);
    let mut first_frame = true;
    let terminal_result = ratatui::run(|terminal| -> io::Result<(ExitStatus, TuiExitReason)> {
        trace.record("application closure entered; child started");
        let mut messages = VecDeque::from([initial]);
        let spinner = ["◐", "◓", "◑", "◒"];
        let mut tick = 0usize;
        loop {
            while let Ok(message) = receiver.try_recv() {
                if messages.len() == 12 {
                    messages.pop_front();
                }
                messages.push_back(message);
            }
            let draw = terminal.draw(|frame| {
                let block = Block::default()
                    .title(format!(
                        " {title} {} · Esc/Ctrl-C to stop ",
                        spinner[tick % spinner.len()]
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan));
                frame.render_widget(
                    Paragraph::new(messages.iter().cloned().collect::<Vec<_>>().join("\n"))
                        .block(block),
                    frame.area(),
                );
            });
            trace.io("terminal.draw", draw.map(|_| ()))?;
            if first_frame {
                trace.record("first frame drawn");
                first_frame = false;
            }
            if let Some(status) = trace.io("child.try_wait", child.try_wait())? {
                trace.record(format!("child completed with {status}"));
                return Ok((status, TuiExitReason::ProcessCompleted));
            }
            let ready = trace.io(
                "event.poll",
                event::poll(std::time::Duration::from_millis(100)),
            )?;
            if ready {
                let event = trace.io("event.read", event::read())?;
                trace.record_event(&event);
                if let Some(reason @ (TuiExitReason::Escape | TuiExitReason::CtrlC)) =
                    close_exit_reason(&event)
                {
                    trace.record(format!("owner cancellation requested by {reason}"));
                    child.kill()?;
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "operation cancelled by owner",
                    ));
                }
            }
            tick = tick.wrapping_add(1);
        }
    });
    let reason = terminal_result.as_ref().ok().map(|(_, reason)| *reason);
    trace.finish(title, &terminal_result, reason, false);
    let status = match terminal_result {
        Ok((status, _)) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("interactive terminal operation failed");
        }
    };
    ensure_success(status)
}

fn forward_update_lines<R: Read + Send + 'static>(stream: R, sender: mpsc::Sender<String>) {
    thread::spawn(move || {
        for line in io::BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
}

/// Complete the only unavoidable legacy boundary: a v0.0.10 updater can
/// activate a verified release but did not replace its own tools.  This command
/// is run directly from `/opt/jarvis/current/jarvis`, which is already part of
/// that verified release.  Future updates perform this atomically themselves.
fn migrate_installed_tooling() -> Result<()> {
    let release = fs::canonicalize(CURRENT_RELEASE).context("resolve active release")?;
    if !release.starts_with(RELEASES_ROOT) {
        bail!("active release is outside the managed release root");
    }
    let admin = release.join("jarvis");
    let updater = release.join("update-core-release");
    for path in [&admin, &updater] {
        let metadata = fs::symlink_metadata(path).context("inspect versioned tooling")?;
        if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o111 == 0 {
            bail!("versioned tooling is unsafe or not executable");
        }
    }
    fs::create_dir_all(LIBEXEC).context("create privileged helper directory")?;
    // Validate/create trusted configuration before changing either executable.
    // A configuration failure therefore cannot leave a partially migrated CLI.
    let updater_config_created = ensure_updater_config()?;
    if let Err(error) = install_tooling_pair(
        &admin,
        Path::new("/usr/local/sbin/jarvis"),
        &updater,
        &Path::new(LIBEXEC).join("update-core-release"),
    ) {
        rollback_new_updater_config(updater_config_created, Path::new("/etc/jarvis/updater.env"))?;
        return Err(error);
    }
    println!("jarvis: installed tooling migrated from verified active release");
    Ok(())
}

fn rollback_new_updater_config(created: bool, path: &Path) -> Result<()> {
    if created {
        fs::remove_file(path).context("roll back newly created updater configuration")?;
    }
    Ok(())
}

fn stage_executable(source: &Path, destination: &Path) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .context("tooling destination has no parent")?;
    let temporary = parent.join(format!(
        ".{}.new",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("jarvis")
    ));
    if temporary.exists() {
        fs::remove_file(&temporary).context("remove stale tooling stage")?;
    }
    fs::copy(source, &temporary).context("stage versioned tooling")?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    Ok(temporary)
}

fn backup_tool(destination: &Path) -> Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect installed tooling"),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("installed tooling destination is unsafe");
    }
    let backup = destination.with_file_name(format!(
        ".{}.previous",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("jarvis")
    ));
    if backup.exists() {
        fs::remove_file(&backup).context("remove stale tooling backup")?;
    }
    fs::copy(destination, &backup).context("backup installed tooling")?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o755))?;
    Ok(Some(backup))
}

fn restore_tool(destination: &Path, backup: Option<&Path>) -> Result<()> {
    if let Some(backup) = backup {
        fs::rename(backup, destination).context("restore installed tooling")?;
    } else if destination.exists() {
        fs::remove_file(destination).context("remove newly installed tooling")?;
    }
    Ok(())
}

fn install_tooling_pair(
    admin_source: &Path,
    admin_destination: &Path,
    updater_source: &Path,
    updater_destination: &Path,
) -> Result<()> {
    let admin_stage = stage_executable(admin_source, admin_destination)?;
    let updater_stage = stage_executable(updater_source, updater_destination)?;
    let admin_backup = backup_tool(admin_destination)?;
    let updater_backup = backup_tool(updater_destination)?;

    if let Err(error) = fs::rename(&updater_stage, updater_destination) {
        let _ = fs::remove_file(&admin_stage);
        let _ = fs::remove_file(&updater_stage);
        if let Some(backup) = admin_backup.as_deref() {
            let _ = fs::remove_file(backup);
        }
        if let Some(backup) = updater_backup.as_deref() {
            let _ = fs::remove_file(backup);
        }
        bail!("activate versioned updater: {error}");
    }
    if let Err(error) = fs::rename(&admin_stage, admin_destination) {
        restore_tool(updater_destination, updater_backup.as_deref())?;
        let _ = fs::remove_file(&admin_stage);
        if let Some(backup) = admin_backup.as_deref() {
            let _ = fs::remove_file(backup);
        }
        bail!("activate versioned admin CLI: {error}; updater restored");
    }
    if let Some(backup) = admin_backup {
        let _ = fs::remove_file(backup);
    }
    if let Some(backup) = updater_backup {
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn ensure_updater_config() -> Result<bool> {
    let path = Path::new("/etc/jarvis/updater.env");
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || metadata.uid() != 0
                || metadata.gid() != 0
                || metadata.permissions().mode() & 0o077 != 0
            {
                bail!("existing updater configuration permissions are unsafe");
            }
            parse_updater_config(&fs::read_to_string(path).context("read updater configuration")?)?;
            return Ok(false);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect updater configuration"),
    }
    fs::create_dir_all("/etc/jarvis").context("create updater config directory")?;
    let temporary = Path::new("/etc/jarvis/.updater.env.new");
    fs::write(
        temporary,
        "JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis\nJARVIS_UPDATE_CHANNEL=stable\n",
    )?;
    fs::set_permissions(temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(temporary, path).context("activate updater configuration")?;
    Ok(true)
}

#[derive(Debug, Deserialize, Serialize)]
struct ModelPolicy {
    version: u8,
    models: Vec<ModelRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ModelRecord {
    provider: String,
    model: String,
    enabled: bool,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<String>,
}

fn read_model_policy() -> Result<ModelPolicy> {
    let path = admin_helpers::resolve_model_policy_path(
        Path::new("/opt/jarvis/current"),
        Path::new("/opt/jarvis/releases"),
        Path::new("/usr/local/sbin"),
        0,
        0,
    )?;
    let metadata = fs::symlink_metadata(&path).context("inspect model policy")?;
    let config_directory = fs::symlink_metadata(path.parent().context("policy has no directory")?)
        .context("inspect policy directory")?;
    if !config_directory.is_dir()
        || config_directory.uid() != 0
        || config_directory.permissions().mode() & 0o777 != 0o750
        || !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != config_directory.gid()
        || metadata.permissions().mode() & 0o777 != 0o640
    {
        bail!("model policy permissions are unsafe");
    }
    let policy: ModelPolicy =
        serde_json::from_slice(&fs::read(path).context("read model policy")?)?;
    if policy.version != 1 {
        bail!("unsupported model policy version");
    }
    Ok(policy)
}

/// Read `routing.json` like Core does at startup: no links, root-owned, not
/// group/world-writable, bounded. `Ok(None)` means absent (built-in order).
/// Errors are Core's stable reason codes.
fn read_routing_file(path: &Path) -> std::result::Result<Option<Vec<u8>>, &'static str> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Err("routing_unsafe"),
        Err(_) => return Err("routing_unreadable"),
    };
    let metadata = file.metadata().map_err(|_| "routing_unreadable")?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err("routing_unsafe");
    }
    let mut raw = Vec::new();
    file.take(jarvis_llm::ROUTING_MAX_BYTES as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| "routing_unreadable")?;
    if raw.len() > jarvis_llm::ROUTING_MAX_BYTES {
        return Err("routing_too_large");
    }
    Ok(Some(raw))
}

/// `{"routing", "routing_unavailable_reason"}` for an unusable or absent file
/// as well as a valid one. Core fails closed on an unusable file.
fn routing_report_from(
    file: std::result::Result<Option<Vec<u8>>, &'static str>,
) -> serde_json::Value {
    let routing = file.and_then(|raw| {
        raw.map(|raw| jarvis_llm::ModelRouting::parse(&raw).map_err(|_| "routing_invalid"))
            .transpose()
    });
    match routing {
        Ok(routing) => serde_json::json!({"routing": routing, "routing_unavailable_reason": null}),
        Err(reason) => serde_json::json!({"routing": null, "routing_unavailable_reason": reason}),
    }
}

fn routing_report() -> Result<serde_json::Value> {
    let policy = admin_helpers::resolve_model_policy_path(
        Path::new("/opt/jarvis/current"),
        Path::new("/opt/jarvis/releases"),
        Path::new("/usr/local/sbin"),
        0,
        0,
    )?;
    if policy.file_name() != Some(OsStr::new("policy.json")) {
        bail!("the active release has no model routing");
    }
    Ok(routing_report_from(read_routing_file(
        &policy.with_file_name("routing.json"),
    )))
}

fn models(args: ModelsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    if presentation.json
        && !matches!(
            &args.command,
            ModelsCommand::List { .. }
                | ModelsCommand::Show { .. }
                | ModelsCommand::Providers { .. }
                | ModelsCommand::Route {
                    command: RouteCommand::List
                }
        )
    {
        bail!("--json is supported only for read-only models list/show");
    }
    if presentation.json && matches!(&args.command, ModelsCommand::Route { .. }) {
        println!("{}", routing_report()?);
        return Ok(());
    }
    if let ModelsCommand::List { provider } = &args.command {
        let mut policy = read_model_policy()?;
        if let Some(provider) = provider {
            policy
                .models
                .retain(|model| model.provider == provider.as_str());
        }
        let priced = usage_insights::priced_model_policy(policy)?;
        if presentation.json {
            println!("{}", serde_json::to_string(&priced)?);
        } else if presentation.interactive && io::stdin().is_terminal() {
            let rows = priced
                .models
                .into_iter()
                .map(|model| {
                    vec![
                        model.provider,
                        model.model,
                        if model.enabled { "yes" } else { "no" }.to_owned(),
                        usage_insights::display_model_price(
                            model.input_per_million_usd,
                            model.price_status,
                        ),
                        usage_insights::display_model_price(
                            model.cache_read_per_million_usd,
                            model.price_status,
                        ),
                        usage_insights::display_model_price(
                            model.output_per_million_usd,
                            model.price_status,
                        ),
                        model.price_status.to_owned(),
                        model.route.unwrap_or_else(|| "—".into()),
                        model.source,
                    ]
                })
                .collect();
            return table_tui(
                "Jarvis Models",
                [
                    "Provider",
                    "Model",
                    "Enabled",
                    "Input $/1M",
                    "Cached $/1M",
                    "Output $/1M",
                    "Pricing",
                    "HF Route",
                    "Source",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                rows,
                presentation.tui_trace,
            );
        } else {
            println!(
                "{:<16} {:<36} {:<8} {:<12} {:<12} {:<12} {:<9} {:<12} SOURCE",
                "PROVIDER",
                "MODEL",
                "ENABLED",
                "INPUT $/1M",
                "CACHED $/1M",
                "OUTPUT $/1M",
                "PRICING",
                "HF ROUTE"
            );
            for model in priced.models {
                println!(
                    "{:<16} {:<36} {:<8} {:<12} {:<12} {:<12} {:<9} {:<12} {}",
                    model.provider,
                    model.model,
                    if model.enabled { "yes" } else { "no" },
                    usage_insights::display_model_price(
                        model.input_per_million_usd,
                        model.price_status,
                    ),
                    usage_insights::display_model_price(
                        model.cache_read_per_million_usd,
                        model.price_status,
                    ),
                    usage_insights::display_model_price(
                        model.output_per_million_usd,
                        model.price_status,
                    ),
                    model.price_status,
                    model.route.unwrap_or_else(|| "—".into()),
                    model.source
                );
            }
        }
        return Ok(());
    }
    let arguments: Vec<String> = match args.command {
        ModelsCommand::RefreshConfigured => vec!["refresh-configured".to_owned()],
        ModelsCommand::Refresh { provider } => vec!["refresh".to_owned()]
            .into_iter()
            .chain(provider.map(|value| value.as_str().to_owned()))
            .collect(),
        ModelsCommand::List { provider } => vec!["list".to_owned()]
            .into_iter()
            .chain(provider.map(|value| value.as_str().to_owned()))
            .collect(),
        ModelsCommand::Register { provider, model } => register_arguments(provider, model)?,
        ModelsCommand::Enable { provider, model } => {
            vec!["enable".to_owned(), provider.as_str().to_owned(), model.0]
        }
        ModelsCommand::Disable { provider, model } => {
            vec!["disable".to_owned(), provider.as_str().to_owned(), model.0]
        }
        ModelsCommand::Show { provider, model } => {
            vec!["show".to_owned(), provider.as_str().to_owned(), model.0]
        }
        ModelsCommand::Providers { provider, model } => {
            return model_providers(provider, model, presentation);
        }
        ModelsCommand::SetRoute {
            provider,
            model,
            route,
        } => {
            if !matches!(provider, Provider::Huggingface) {
                bail!("routes are supported only for huggingface");
            }
            vec![
                "set-route".to_owned(),
                provider.as_str().to_owned(),
                model.0,
                route.0,
            ]
        }
        ModelsCommand::Route { command } => route_arguments(command)?,
    };
    compatibility_helper(AdminHelper::Models, arguments, verbose)
}

#[derive(Debug, Deserialize, Serialize)]
struct HfProvidersResponse {
    model: String,
    routes: Vec<String>,
    providers: Vec<HfProviderRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct HfProviderRecord {
    provider: String,
    status: String,
    context_length: Option<u64>,
    input_per_million_usd: Option<f64>,
    output_per_million_usd: Option<f64>,
    supports_tools: Option<bool>,
    supports_structured_output: Option<bool>,
    first_token_latency_ms: Option<f64>,
    throughput: Option<f64>,
}

fn model_providers(provider: Provider, model: ModelId, presentation: &Presentation) -> Result<()> {
    if !matches!(provider, Provider::Huggingface) {
        bail!("inference provider discovery is supported only for huggingface");
    }
    let output = compatibility_helper_output(
        AdminHelper::Models,
        vec!["providers".into(), provider.as_str().into(), model.0],
    )?;
    let response: HfProvidersResponse =
        serde_json::from_slice(&output).context("parse trusted Hugging Face provider catalog")?;
    if presentation.json {
        println!("{}", serde_json::to_string(&response)?);
        return Ok(());
    }
    println!("Model: {}", response.model);
    println!("Available routes: {}", response.routes.join(", "));
    println!(
        "{:<16} {:<10} {:<12} {:<12} {:<10} {:<10} {:<7} STRUCTURED",
        "ROUTE", "STATUS", "INPUT/M", "OUTPUT/M", "TTFT", "TOK/S", "TOOLS"
    );
    for item in response.providers {
        println!(
            "{:<16} {:<10} {:<12} {:<12} {:<10} {:<10} {:<7} {}",
            item.provider,
            item.status,
            item.input_per_million_usd
                .map_or_else(|| "—".into(), |v| format!("${v:.4}")),
            item.output_per_million_usd
                .map_or_else(|| "—".into(), |v| format!("${v:.4}")),
            item.first_token_latency_ms
                .map_or_else(|| "—".into(), |v| format!("{v:.0}ms")),
            item.throughput
                .map_or_else(|| "—".into(), |v| format!("{v:.1}")),
            item.supports_tools
                .map_or("—", |v| if v { "yes" } else { "no" }),
            item.supports_structured_output
                .map_or("—", |v| if v { "yes" } else { "no" }),
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct CredentialStatus {
    provider: &'static str,
    configured: bool,
}

fn credential_statuses() -> Vec<CredentialStatus> {
    let expected_group = fs::symlink_metadata("/etc/jarvis/secrets")
        .ok()
        .map(|metadata| metadata.gid());
    [
        "anthropic",
        "openai",
        "deepseek",
        "xai",
        "zai",
        "ollama-cloud",
        "huggingface",
        "jev",
    ]
    .into_iter()
    .map(|provider| {
        let path = Path::new("/etc/jarvis/secrets").join(format!("{provider}.env"));
        let configured = fs::symlink_metadata(path).is_ok_and(|metadata| {
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == 0
                && Some(metadata.gid()) == expected_group
                && metadata.permissions().mode() & 0o777 == 0o640
        });
        CredentialStatus {
            provider,
            configured,
        }
    })
    .collect()
}

fn credentials(args: CredentialsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    if presentation.json && !matches!(&args.command, CredentialsCommand::List) {
        bail!("--json is supported only for read-only credentials list");
    }
    if matches!(&args.command, CredentialsCommand::List) {
        let statuses = credential_statuses();
        if presentation.json {
            println!("{}", serde_json::to_string(&statuses)?);
        } else if presentation.interactive && io::stdin().is_terminal() {
            let mut rows: Vec<Vec<String>> = statuses
                .into_iter()
                .map(|status| {
                    vec![
                        status.provider.to_owned(),
                        if status.configured {
                            "configured"
                        } else {
                            "not-configured"
                        }
                        .to_owned(),
                    ]
                })
                .collect();
            rows.push(vec![
                "ollama-local".to_owned(),
                "no credential required".to_owned(),
            ]);
            return table_tui(
                "Jarvis Credentials",
                vec!["Provider".to_owned(), "Status".to_owned()],
                rows,
                presentation.tui_trace,
            );
        } else {
            println!("{:<16} STATUS", "PROVIDER");
            for status in statuses {
                println!(
                    "{:<16} {}",
                    status.provider,
                    if status.configured {
                        "configured"
                    } else {
                        "not-configured"
                    }
                );
            }
            println!("{:<16} no credential required", "ollama-local");
        }
        return Ok(());
    }
    // Compatibility boundary: the helper owns only protected file mechanics and
    // reads secrets directly from /dev/tty. Rust deliberately inherits that
    // controlling TTY; it never captures, receives, or logs a credential.
    let arguments = match args.command {
        CredentialsCommand::List => vec!["list".to_owned()],
        CredentialsCommand::Set { provider } => {
            return credential_setup::set(&provider, verbose);
        }
        CredentialsCommand::Test { provider } => {
            vec!["test".to_owned(), provider.as_str().to_owned()]
        }
        CredentialsCommand::Remove { provider } => {
            vec!["remove".to_owned(), provider.as_str().to_owned()]
        }
    };
    compatibility_helper(AdminHelper::Credentials, arguments, verbose)
}

fn agents(args: AgentsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    if presentation.json && !matches!(&args.command, AgentsCommand::Status | AgentsCommand::Tree) {
        bail!("--json is supported only for read-only agents status/tree");
    }
    match args.command {
        AgentsCommand::Status => {
            let bundle = active_bundle()?.context("no active private agent bundle")?;
            if presentation.json {
                println!("{}", serde_json::to_string(&bundle)?);
            } else if presentation.interactive && io::stdin().is_terminal() {
                return table_tui(
                    "Jarvis Agents",
                    vec!["Bundle".to_owned(), "Agents".to_owned()],
                    vec![vec![bundle.id, bundle.agent_count.to_string()]],
                    presentation.tui_trace,
                );
            } else {
                println!(
                    "Agent bundle: {} ({} agents)",
                    bundle.id, bundle.agent_count
                );
            }
            Ok(())
        }
        AgentsCommand::Tree => {
            let tree = active_agent_tree()?.context("no active private agent bundle")?;
            if presentation.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "version": 1,
                        "bundle_id": tree.bundle_id,
                        "agents": tree.agents,
                    })
                );
            } else {
                println!("Agent bundle: {}", tree.bundle_id);
                for agent in tree.agents {
                    println!(
                        "{} / {} ({})",
                        agent.group.as_deref().unwrap_or("Ungrouped"),
                        agent.name,
                        agent.model_policy.as_deref().unwrap_or("no model policy")
                    );
                }
            }
            Ok(())
        }
        AgentsCommand::Check => {
            let mut command = trusted_command(Path::new(LIBEXEC).join("private-agent-poll"));
            command.arg("--check");
            if presentation.interactive && io::stdin().is_terminal() && !verbose {
                run_process_tui(
                    &mut command,
                    "Jarvis Agents",
                    "Checking private agent source and active bundle…".to_owned(),
                    presentation.tui_trace,
                )
            } else {
                run_command(
                    &mut command,
                    if verbose {
                        SubprocessMode::Streamed
                    } else {
                        SubprocessMode::Captured
                    },
                )
            }
        }
        AgentsCommand::Update => {
            let mut command = trusted_command(Path::new(LIBEXEC).join("private-agent-poll"));
            if presentation.interactive && io::stdin().is_terminal() && !verbose {
                run_process_tui(
                    &mut command,
                    "Jarvis Agents",
                    "Validating and activating private agent update…".to_owned(),
                    presentation.tui_trace,
                )
            } else {
                run_command(&mut command, SubprocessMode::from_verbose(verbose))
            }
        }
        AgentsCommand::Rollback { yes } => {
            if !yes {
                confirm("Activate the previous verified private agent bundle?")?;
            }
            bail!("agent rollback is not available until the Rust transactional activator is installed")
        }
    }
}

fn confirm(prompt: &str) -> Result<()> {
    if !confirmation_answer(&read_confirmation(&format!("{prompt} [y/N]"))?) {
        bail!("unchanged");
    }
    Ok(())
}

/// Require the owner to type the exact release tag before a schema change.
fn confirm_typed(prompt: &str, expected: &str) -> Result<()> {
    if !typed_confirmation_matches(&read_confirmation(prompt)?, expected) {
        bail!("unchanged; the typed text did not match {expected}");
    }
    Ok(())
}

fn read_confirmation(prompt: &str) -> Result<String> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("refusing non-interactive mutation; pass --yes after reviewing the target");
    }
    // Confirmation is deliberately a tiny, fixed TTY interaction.  No owner
    // input becomes a command, and credentials use their own hidden /dev/tty
    // reader in the compatibility helper.
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open controlling terminal for confirmation")?;
    write!(tty, "{prompt} ")?;
    tty.flush()?;
    let mut answer = String::new();
    io::BufReader::new(&tty)
        .read_line(&mut answer)
        .context("read confirmation")?;
    Ok(answer)
}

fn confirmation_answer(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
}

fn typed_confirmation_matches(answer: &str, expected: &str) -> bool {
    answer.trim_end_matches(['\n', '\r']) == expected
}

fn mutation_lock(path: impl AsRef<Path>) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path.as_ref())
        .context("open administration lock")?;
    file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!("another conflicting Jarvis administration operation is running")
    })?;
    Ok(file)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubprocessMode {
    InheritedInteractive,
    Captured,
    Streamed,
}
impl SubprocessMode {
    fn from_verbose(verbose: bool) -> Self {
        if verbose {
            Self::Streamed
        } else {
            Self::InheritedInteractive
        }
    }
}

fn run_command(command: &mut ProcessCommand, mode: SubprocessMode) -> Result<()> {
    match mode {
        SubprocessMode::InheritedInteractive | SubprocessMode::Streamed => {
            let status = command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .context("start trusted helper")?;
            ensure_success(status)
        }
        SubprocessMode::Captured => {
            let output = command
                .stdin(Stdio::null())
                .output()
                .context("start trusted helper")?;
            io::stdout().write_all(&output.stdout)?;
            io::stderr().write_all(&output.stderr)?;
            ensure_success(output.status)
        }
    }
}

fn ensure_success(status: ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("trusted helper exited with {status}")
    }
}

fn systemctl_state(action: &str, unit: &str) -> String {
    trusted_command("systemctl")
        .args([action, unit])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .unwrap_or_else(|| "inactive".to_owned())
}

/// A child never inherits the invoking administrator's environment.  This
/// prevents an arbitrary `LD_*`, proxy, credential, or provider variable from
/// changing a privileged helper's behaviour.  Helpers receive only their
/// normal root-owned configuration files and the minimum execution context.
fn trusted_command(program: impl AsRef<OsStr>) -> ProcessCommand {
    let mut command = ProcessCommand::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LANG", "C.UTF-8");
    command
}

/// The updater's configuration is root-owned and its values are not secrets:
/// the optional netrc is passed as a *path*, which the helper validates before
/// opening.  We deliberately do not source shell syntax or forward any other
/// inherited variables.
fn load_updater_environment(command: &mut ProcessCommand) -> Result<()> {
    let path = Path::new("/etc/jarvis/updater.env");
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect updater configuration"),
    };
    if metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("updater configuration permissions are unsafe");
    }
    let config =
        parse_updater_config(&fs::read_to_string(path).context("read updater configuration")?)?;
    // The versioned helper independently validates the same root-owned file.
    // These are the sole compatibility values forwarded to an older helper;
    // no caller environment crosses the root boundary.
    command.env("JARVIS_UPDATE_REPOSITORY", config.repository);
    if let Some(netrc) = config.github_curl_netrc {
        command.env("JARVIS_GITHUB_CURL_NETRC", netrc);
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct TrustedUpdaterConfig {
    repository: String,
    github_curl_netrc: Option<PathBuf>,
}

fn parse_updater_config(contents: &str) -> Result<TrustedUpdaterConfig> {
    let mut repository = None;
    let mut netrc = None;
    for line in contents.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .context("updater configuration is malformed")?;
        match key {
            "JARVIS_UPDATE_REPOSITORY" => {
                if repository.replace(value.to_owned()).is_some() || !valid_repository(value) {
                    bail!("updater repository is invalid or duplicated");
                }
            }
            "JARVIS_UPDATE_CHANNEL" if value == "stable" => {}
            "JARVIS_GITHUB_CURL_NETRC" => {
                let candidate = PathBuf::from(value);
                if !candidate.is_absolute() || netrc.replace(candidate).is_some() {
                    bail!("updater netrc path is invalid or duplicated");
                }
            }
            _ => bail!("updater configuration contains an unsupported key"),
        }
    }
    Ok(TrustedUpdaterConfig {
        repository: repository.context("updater repository is missing")?,
        github_curl_netrc: netrc,
    })
}

fn valid_repository(value: &str) -> bool {
    let mut segments = value.split('/');
    let valid_segment = |segment: Option<&str>| {
        segment.is_some_and(|segment| {
            !segment.is_empty()
                && segment.len() <= 100
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    };
    valid_segment(segments.next()) && valid_segment(segments.next()) && segments.next().is_none()
}

fn parse_key_value_output(output: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line
            .split_once(':')
            .context("trusted helper returned malformed structured output")?;
        let key = key.trim().to_ascii_lowercase().replace(' ', "_");
        if key.is_empty() || values.insert(key, value.trim().to_owned()).is_some() {
            bail!("trusted helper returned duplicate or empty field");
        }
    }
    Ok(values)
}

fn parse_release_tag(input: &str) -> Result<String, String> {
    if valid_release_tag(input) {
        Ok(input.to_owned())
    } else {
        Err("must be vMAJOR.MINOR.PATCH".to_owned())
    }
}
fn valid_release_tag(tag: &str) -> bool {
    let mut parts = tag.strip_prefix('v').unwrap_or_default().split('.');
    parts.clone().count() == 3
        && parts.all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod main_tests;
