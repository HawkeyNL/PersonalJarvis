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

mod admin_helpers;
mod agent_tree;
mod terminal_ui;
mod tui_app;
mod update_center;
mod usage_insights;

use admin_helpers::{compatibility_helper, trusted_admin_helper_command, AdminHelper};
#[cfg(test)]
use admin_helpers::{explicit_helper_subprocess_mode, resolve_admin_helper};
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
    #[arg(long, requires = "rollback")]
    yes: bool,
}

#[derive(Debug, Args)]
struct ModelsArgs {
    #[command(subcommand)]
    command: ModelsCommand,
}
#[derive(Debug, Subcommand)]
enum ModelsCommand {
    Refresh { provider: Option<Provider> },
    List { provider: Option<Provider> },
    Enable { provider: Provider, model: ModelId },
    Disable { provider: Provider, model: ModelId },
    Show { provider: Provider, model: ModelId },
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
        #[cfg(feature = "tui-preview")]
        Commands::TuiPreview(_) => unreachable!("handled before root-only commands"),
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
        TuiPreviewScenario::Models => table_tui(
            "Jarvis Models · fixture",
            ["Provider", "Model", "Enabled", "Source"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            vec![
                vec!["openai-api", "gpt-fixture", "yes", "catalog fixture"],
                vec!["anthropic-api", "claude-fixture", "no", "policy fixture"],
                vec!["ollama", "local-fixture:latest", "yes", "local fixture"],
            ]
            .into_iter()
            .map(|row| row.into_iter().map(str::to_owned).collect())
            .collect(),
            trace_enabled,
        ),
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
        let mut command = trusted_command(Path::new(LIBEXEC).join("verify-home-node"));
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
    run_program(
        Path::new(LIBEXEC).join("verify-home-node"),
        std::iter::empty::<&str>(),
        SubprocessMode::from_verbose(verbose),
    )?;
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
        UpdateInvocation::Center => unreachable!("handled above"),
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
}

fn read_model_policy() -> Result<ModelPolicy> {
    let path = Path::new("/etc/jarvis/model-policy.json");
    let metadata = fs::symlink_metadata(path).context("inspect model policy")?;
    let config_directory =
        fs::symlink_metadata("/etc/jarvis").context("inspect config directory")?;
    if metadata.file_type().is_symlink()
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

fn models(args: ModelsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    if presentation.json
        && !matches!(
            &args.command,
            ModelsCommand::List { .. } | ModelsCommand::Show { .. }
        )
    {
        bail!("--json is supported only for read-only models list/show");
    }
    if let ModelsCommand::List { provider } = &args.command {
        let mut policy = read_model_policy()?;
        if let Some(provider) = provider {
            policy
                .models
                .retain(|model| model.provider == provider.as_str());
        }
        if presentation.json {
            println!("{}", usage_insights::priced_model_policy_json(policy)?);
        } else if presentation.interactive && io::stdin().is_terminal() {
            let rows = policy
                .models
                .into_iter()
                .map(|model| {
                    vec![
                        model.provider,
                        model.model,
                        if model.enabled { "yes" } else { "no" }.to_owned(),
                        model.source,
                    ]
                })
                .collect();
            return table_tui(
                "Jarvis Models",
                ["Provider", "Model", "Enabled", "Source"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                rows,
                presentation.tui_trace,
            );
        } else {
            println!("{:<16} {:<36} {:<8} SOURCE", "PROVIDER", "MODEL", "ENABLED");
            for model in policy.models {
                println!(
                    "{:<16} {:<36} {:<8} {}",
                    model.provider,
                    model.model,
                    if model.enabled { "yes" } else { "no" },
                    model.source
                );
            }
        }
        return Ok(());
    }
    let arguments: Vec<String> = match args.command {
        ModelsCommand::Refresh { provider } => vec!["refresh".to_owned()]
            .into_iter()
            .chain(provider.map(|value| value.as_str().to_owned()))
            .collect(),
        ModelsCommand::List { provider } => vec!["list".to_owned()]
            .into_iter()
            .chain(provider.map(|value| value.as_str().to_owned()))
            .collect(),
        ModelsCommand::Enable { provider, model } => {
            vec!["enable".to_owned(), provider.as_str().to_owned(), model.0]
        }
        ModelsCommand::Disable { provider, model } => {
            vec!["disable".to_owned(), provider.as_str().to_owned(), model.0]
        }
        ModelsCommand::Show { provider, model } => {
            vec!["show".to_owned(), provider.as_str().to_owned(), model.0]
        }
    };
    compatibility_helper(AdminHelper::Models, arguments, verbose)
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
            vec!["set".to_owned(), provider.as_str().to_owned()]
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
    write!(tty, "{prompt} [y/N] ")?;
    tty.flush()?;
    let mut answer = String::new();
    io::BufReader::new(&tty)
        .read_line(&mut answer)
        .context("read confirmation")?;
    if !confirmation_answer(&answer) {
        bail!("unchanged");
    }
    Ok(())
}

fn confirmation_answer(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
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

fn run_program<I, S>(program: PathBuf, args: I, mode: SubprocessMode) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = trusted_command(program);
    command.args(args);
    run_command(&mut command, mode)
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
