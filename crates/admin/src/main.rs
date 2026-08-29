//! Root-operated, typed administration surface for a Jarvis Home Node.
//!
//! This binary deliberately accepts a small allowlist of operations.  It never
//! evaluates owner input as a shell command and it is not exposed by the API.

use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, IsTerminal, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitStatus, Stdio},
    sync::mpsc,
    thread,
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
    command: Commands,
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
    #[arg(value_enum, default_value_t = TuiPreviewScenario::HealthyStatus)]
    scenario: TuiPreviewScenario,
}

#[cfg(feature = "tui-preview")]
#[derive(Clone, Debug, ValueEnum)]
enum TuiPreviewScenario {
    HealthyStatus,
    DegradedStatus,
    Models,
    Credentials,
    Agents,
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

#[derive(Clone, Debug, ValueEnum)]
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

#[derive(Debug, Serialize)]
struct StatusReport {
    release: Option<String>,
    services: BTreeMap<&'static str, String>,
    updater_enabled: String,
    agent_bundle: Option<AgentBundle>,
}

#[derive(Debug, Serialize)]
struct AgentBundle {
    id: String,
    agent_count: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("jarvis: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Commands::TerminalDiagnostics) {
        return terminal_diagnostics(cli.json);
    }
    #[cfg(feature = "tui-preview")]
    if let Commands::TuiPreview(args) = &cli.command {
        if cli.json {
            bail!("fixture TUI preview does not support --json");
        }
        if !io::stdin().is_terminal() || !terminal_supports_rich_output() {
            bail!("fixture TUI preview requires an interactive terminal with rich output enabled");
        }
        return tui_preview(&args.scenario, cli.tui_trace);
    }
    require_root()?;
    let presentation = Presentation::new(cli.json, cli.tui_trace);
    match cli.command {
        Commands::Version => version(&presentation),
        Commands::TerminalDiagnostics => unreachable!("handled before root-only commands"),
        Commands::Status => status(&presentation),
        Commands::Health => health(&presentation, cli.verbose),
        Commands::Logs(args) => logs(args, &presentation),
        Commands::Update(args) => update(args, &presentation, cli.verbose),
        Commands::MigrateInstalledTooling => migrate_installed_tooling(),
        Commands::Models(args) => models(args, &presentation, cli.verbose),
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

struct Presentation {
    json: bool,
    interactive: bool,
    tui_trace: bool,
}
impl Presentation {
    fn new(json: bool, tui_trace: bool) -> Self {
        Self {
            json,
            interactive: !json && terminal_supports_rich_output(),
            tui_trace,
        }
    }
    fn intro(&self, text: &str) {
        if !self.json {
            println!("{text}");
        }
    }
    fn outro(&self, text: &str) {
        if !self.json {
            println!("{text}");
        }
    }
}

fn terminal_supports_rich_output() -> bool {
    terminal_supports_rich_output_for(
        io::stdout().is_terminal(),
        std::env::var_os("NO_COLOR").is_none(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn terminal_supports_rich_output_for(
    stdout_is_tty: bool,
    color_allowed: bool,
    term: Option<&str>,
) -> bool {
    stdout_is_tty && color_allowed && term != Some("dumb")
}

fn terminal_diagnostics(json: bool) -> Result<()> {
    let stdin_is_tty = io::stdin().is_terminal();
    let stdout_is_tty = io::stdout().is_terminal();
    let stderr_is_tty = io::stderr().is_terminal();
    let term = std::env::var("TERM").ok();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let dimensions = terminal::size().ok();
    let rich_output = terminal_supports_rich_output_for(
        stdout_is_tty && stdin_is_tty,
        !no_color,
        term.as_deref(),
    );
    let mut raw_mode = "not attempted (stdin/stdout are not TTYs)".to_owned();
    let mut alternate_screen = raw_mode.clone();
    let mut event_polling = raw_mode.clone();
    let mut restoration = "not needed".to_owned();
    let mut backend_error = None;

    if json {
        raw_mode = "not attempted (--json never initializes a terminal)".to_owned();
        alternate_screen = raw_mode.clone();
        event_polling = raw_mode.clone();
    } else if stdin_is_tty && stdout_is_tty {
        let mut raw_enabled = false;
        let mut alternate_attempted = false;
        match enable_raw_mode() {
            Ok(()) => {
                raw_enabled = true;
                raw_mode = "available".to_owned();
                alternate_attempted = true;
                match execute!(io::stdout(), EnterAlternateScreen) {
                    Ok(()) => {
                        alternate_screen = "available (control sequence accepted)".to_owned();
                        match event::poll(std::time::Duration::ZERO) {
                            Ok(_) => event_polling = "available".to_owned(),
                            Err(error) => {
                                event_polling = "unavailable".to_owned();
                                backend_error = Some(format!("event polling: {error}"));
                            }
                        }
                    }
                    Err(error) => {
                        alternate_screen = "unavailable".to_owned();
                        backend_error = Some(format!("alternate-screen initialization: {error}"));
                    }
                }
            }
            Err(error) => {
                raw_mode = "unavailable".to_owned();
                backend_error = Some(format!("raw-mode initialization: {error}"));
            }
        }

        let mut restore_errors = Vec::new();
        if alternate_attempted {
            if let Err(error) = execute!(io::stdout(), LeaveAlternateScreen) {
                restore_errors.push(format!("leave alternate screen: {error}"));
            }
        }
        if raw_enabled {
            if let Err(error) = disable_raw_mode() {
                restore_errors.push(format!("disable raw mode: {error}"));
            }
        }
        restoration = if restore_errors.is_empty() {
            "successful".to_owned()
        } else {
            let error = restore_errors.join("; ");
            backend_error.get_or_insert_with(|| format!("terminal restoration: {error}"));
            "failed".to_owned()
        };
    }

    let report = serde_json::json!({
        "stdin_is_tty": stdin_is_tty,
        "stdout_is_tty": stdout_is_tty,
        "stderr_is_tty": stderr_is_tty,
        "term": term,
        "dimensions": dimensions.map(|(width, height)| format!("{width}x{height}")),
        "no_color": no_color,
        "rich_output": rich_output,
        "raw_mode": raw_mode,
        "alternate_screen": alternate_screen,
        "event_polling": event_polling,
        "restoration": restoration,
        "running_under_sudo": std::env::var_os("SUDO_UID").is_some()
            || std::env::var_os("SUDO_USER").is_some(),
        "backend_error": backend_error,
    });
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("Jarvis terminal diagnostics (no secrets or configuration values)");
        for (label, key) in [
            ("stdin is TTY", "stdin_is_tty"),
            ("stdout is TTY", "stdout_is_tty"),
            ("stderr is TTY", "stderr_is_tty"),
            ("TERM", "term"),
            ("dimensions", "dimensions"),
            ("NO_COLOR", "no_color"),
            ("rich output", "rich_output"),
            ("raw mode", "raw_mode"),
            ("alternate screen", "alternate_screen"),
            ("event polling", "event_polling"),
            ("restoration", "restoration"),
            ("running under sudo", "running_under_sudo"),
            ("backend error", "backend_error"),
        ] {
            let value = report[key]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| report[key].to_string());
            println!("  {label:<19} {value}");
        }
    }
    Ok(())
}

fn version(presentation: &Presentation) -> Result<()> {
    let report = serde_json::json!({"admin_version": env!("CARGO_PKG_VERSION"), "active_core": active_release()?});
    if presentation.json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("Jarvis admin CLI: {}", env!("CARGO_PKG_VERSION"));
        println!(
            "Active Core:      {}",
            report["active_core"].as_str().unwrap_or("unavailable")
        );
    }
    Ok(())
}

fn status(presentation: &Presentation) -> Result<()> {
    let report = status_report()?;
    if presentation.json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    if presentation.interactive && io::stdin().is_terminal() {
        return status_tui(&report, presentation.tui_trace);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiExitReason {
    Quit,
    Escape,
    CtrlC,
    ProcessCompleted,
}

impl std::fmt::Display for TuiExitReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Quit => "q key",
            Self::Escape => "Escape key",
            Self::CtrlC => "Ctrl-C",
            Self::ProcessCompleted => "child process completed",
        })
    }
}

fn close_exit_reason(event: &Event) -> Option<TuiExitReason> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Char('q') => Some(TuiExitReason::Quit),
        KeyCode::Esc => Some(TuiExitReason::Escape),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(TuiExitReason::CtrlC)
        }
        _ => None,
    }
}

struct TuiTrace {
    enabled: bool,
    started: std::time::Instant,
    entries: VecDeque<String>,
    failure: Option<String>,
}

impl TuiTrace {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: std::time::Instant::now(),
            entries: VecDeque::new(),
            failure: None,
        }
    }

    fn record(&mut self, entry: impl Into<String>) {
        if !self.enabled {
            return;
        }
        if self.entries.len() == 16 {
            self.entries.pop_front();
        }
        self.entries.push_back(entry.into());
    }

    fn io<T>(&mut self, stage: &str, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result {
            self.failure = Some(format!("{stage}: {error}"));
        }
        result
    }

    fn record_event(&mut self, event: &Event) {
        let description = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(reason) = close_exit_reason(event) {
                    format!("event.read: documented close key ({reason})")
                } else {
                    "event.read: other key press".to_owned()
                }
            }
            Event::Key(_) => "event.read: non-press key event".to_owned(),
            Event::Resize(width, height) => format!("event.read: resize {width}x{height}"),
            Event::Mouse(_) => "event.read: mouse event".to_owned(),
            Event::FocusGained => "event.read: focus gained".to_owned(),
            Event::FocusLost => "event.read: focus lost".to_owned(),
            Event::Paste(_) => "event.read: paste event (contents omitted)".to_owned(),
        };
        self.record(description);
    }

    fn finish<T>(
        &self,
        view: &str,
        result: &io::Result<T>,
        reason: Option<TuiExitReason>,
        persistent: bool,
    ) {
        let elapsed = self.started.elapsed();
        let rapid_success =
            persistent && result.is_ok() && elapsed < std::time::Duration::from_millis(750);
        if !self.enabled && !rapid_success {
            return;
        }
        if rapid_success && !self.enabled {
            eprintln!(
                "Jarvis interactive view closed after {:.2}s.",
                elapsed.as_secs_f64()
            );
            if let Some(reason) = reason {
                eprintln!("Exit reason: {reason}");
            } else {
                eprintln!("Exit reason: missing (unexpected successful return)");
            }
            eprintln!("Run `jarvis terminal-diagnostics` and retry with `--tui-trace`.");
            return;
        }
        eprintln!("Jarvis TUI trace ({view}; no input contents recorded)");
        eprintln!("  lifecycle: ratatui::run returned after terminal restoration");
        for entry in &self.entries {
            eprintln!("  {entry}");
        }
        if let Some(reason) = reason {
            eprintln!("  exit reason: {reason}");
        } else if let Some(failure) = &self.failure {
            eprintln!("  failure stage: {failure}");
        } else if let Err(error) = result {
            eprintln!("  error: {error}");
        } else {
            eprintln!("  exit reason: missing (unexpected successful return)");
        }
    }
}

fn status_tui(report: &StatusReport, trace_enabled: bool) -> Result<()> {
    // `ratatui::run` installs restoration/panic handling around the alternate
    // screen.  Business state is computed before this call, so no privileged
    // operation is coupled to terminal widgets or input events.
    let mut trace = TuiTrace::new(trace_enabled);
    let mut first_frame = true;
    let result = ratatui::run(|terminal| -> io::Result<TuiExitReason> {
        trace.record("application closure entered");
        loop {
            let draw = terminal
                .draw(|frame| render_status_dashboard(frame, report))
                .map(|_| ());
            trace.io("terminal.draw", draw)?;
            if first_frame {
                trace.record("first frame drawn");
                first_frame = false;
            }
            let ready = trace.io(
                "event.poll",
                event::poll(std::time::Duration::from_millis(250)),
            )?;
            if ready {
                let event = trace.io("event.read", event::read())?;
                trace.record_event(&event);
                if let Some(reason) = close_exit_reason(&event) {
                    return Ok(reason);
                }
            }
        }
    });
    let reason = result.as_ref().ok().copied();
    trace.finish("status", &result, reason, true);
    result.map(|_| ()).map_err(Into::into)
}

fn render_status_dashboard(frame: &mut ratatui::Frame, report: &StatusReport) {
    let area = frame.area();
    let outer = Block::default()
        .title(" Jarvis Home Node · q / Esc to close ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    if inner.width < 44 || inner.height < 10 {
        render_compact_status(frame, inner, report);
        return;
    }
    let rows = report.services.iter().map(|(name, state)| {
        let healthy = state == "active";
        Row::new(vec![
            name.to_string(),
            state.to_string(),
            if healthy { "✓" } else { "!" }.to_owned(),
        ])
        .style(Style::default().fg(if healthy { Color::Green } else { Color::Yellow }))
    });
    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Release ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                report.release.as_deref().unwrap_or("unavailable"),
                Style::default().fg(Color::Cyan),
            ),
        ])),
        sections[0],
    );
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(45),
                Constraint::Percentage(40),
                Constraint::Length(3),
            ],
        )
        .header(Row::new(["Service", "Status", ""]).style(Style::default().fg(Color::DarkGray)))
        .block(Block::default().borders(Borders::TOP)),
        sections[1],
    );
    let agents = report
        .agent_bundle
        .as_ref()
        .map_or("unavailable".to_owned(), |bundle| {
            format!("{} agents · {}", bundle.agent_count, bundle.id)
        });
    frame.render_widget(
        Paragraph::new(format!(
            "Agents: {agents}    Updater: {}",
            report.updater_enabled
        )),
        sections[2],
    );
}

fn render_compact_status(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    report: &StatusReport,
) {
    let mut lines = vec![Line::from(format!(
        "Release: {}",
        report.release.as_deref().unwrap_or("unavailable")
    ))];
    lines.extend(
        report
            .services
            .iter()
            .map(|(name, state)| Line::from(format!("{name}: {state}"))),
    );
    lines.push(Line::from(format!("Updater: {}", report.updater_enabled)));
    frame.render_widget(Paragraph::new(lines), area);
}

fn table_tui(
    title: &str,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    trace_enabled: bool,
) -> Result<()> {
    let mut trace = TuiTrace::new(trace_enabled);
    let mut first_frame = true;
    let result = ratatui::run(|terminal| -> io::Result<TuiExitReason> {
        trace.record("application closure entered");
        loop {
            let draw = terminal.draw(|frame| {
                let area = frame.area();
                let column_count = headers.len().max(1) as u16;
                let widths = vec![Constraint::Ratio(1, column_count.into()); headers.len()];
                let block = Block::default()
                    .title(format!(" {title} · q / Esc to close "))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan));
                let table_rows = rows.iter().map(|row| Row::new(row.clone()));
                frame.render_widget(
                    Table::new(table_rows, widths)
                        .header(Row::new(headers.clone()).style(Style::default().fg(Color::Cyan)))
                        .block(block),
                    area,
                );
            });
            trace.io("terminal.draw", draw.map(|_| ()))?;
            if first_frame {
                trace.record("first frame drawn");
                first_frame = false;
            }
            let ready = trace.io(
                "event.poll",
                event::poll(std::time::Duration::from_millis(250)),
            )?;
            if ready {
                let event = trace.io("event.read", event::read())?;
                trace.record_event(&event);
                if let Some(reason) = close_exit_reason(&event) {
                    return Ok(reason);
                }
            }
        }
    });
    let reason = result.as_ref().ok().copied();
    trace.finish(title, &result, reason, true);
    result.map(|_| ()).map_err(Into::into)
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

fn active_bundle() -> Result<Option<AgentBundle>> {
    let target = fs::canonicalize("/var/lib/jarvis/agents/current").ok();
    let Some(target) = target else {
        return Ok(None);
    };
    if !target.starts_with("/var/lib/jarvis/agents/releases") {
        bail!("active agent bundle is outside the managed release root");
    }
    let data =
        fs::read_to_string(target.join("manifest.json")).context("read active agent manifest")?;
    let count = serde_json::from_str::<serde_json::Value>(&data)?
        .get("agents")
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len);
    Ok(Some(AgentBundle {
        id: target
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("unknown")
            .to_owned(),
        agent_count: count,
    }))
}

fn health(presentation: &Presentation, verbose: bool) -> Result<()> {
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
    status(presentation)
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
    // The narrowly scoped updater helper owns the same lock and release
    // transaction until its migration is complete.  Taking it here would
    // deadlock the child; no second update policy is introduced.
    let mut command = trusted_command(Path::new(LIBEXEC).join("update-core-release"));
    load_updater_environment(&mut command)?;
    if args.check {
        command.arg("--check");
    } else if args.status {
        command.arg("--status");
    } else if args.rollback {
        if !args.yes {
            confirm("Rollback Core to the previous verified release?")?;
        }
        command.arg("--rollback");
    } else if let Some(version) = args.version {
        command.args(["--version", &version]);
    } else {
        command.arg("--latest");
    }
    if presentation.json {
        if !(args.check || args.status) {
            bail!("--json is supported only for non-mutating update --check/--status");
        }
        let mode = if args.check { "check" } else { "status" };
        let output = command.output().context("start trusted updater")?;
        if !output.status.success() && !(args.check && output.status.code() == Some(2)) {
            io::stderr().write_all(&output.stderr)?;
            return ensure_success(output.status);
        }
        let values = parse_key_value_output(&String::from_utf8(output.stdout)?)?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"mode": mode, "values": values}))?
        );
        Ok(())
    } else if presentation.interactive && io::stdin().is_terminal() && !verbose {
        run_process_tui(
            &mut command,
            "Jarvis Update",
            "Starting verified update transaction…".to_owned(),
            presentation.tui_trace,
        )
    } else {
        run_command(&mut command, SubprocessMode::from_verbose(verbose))
    }
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

#[derive(Debug, Deserialize, Serialize)]
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
            println!("{}", serde_json::to_string(&policy)?);
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
    compatibility_helper("jarvis-models", arguments, presentation, verbose, true)
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
    let tui_safe = matches!(&args.command, CredentialsCommand::Test { .. });
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
    compatibility_helper(
        "jarvis-credentials",
        arguments,
        presentation,
        verbose,
        tui_safe,
    )
}

fn compatibility_helper(
    name: &str,
    args: Vec<String>,
    presentation: &Presentation,
    verbose: bool,
    tui_safe: bool,
) -> Result<()> {
    let allowed = matches!(name, "jarvis-models" | "jarvis-credentials");
    if !allowed {
        bail!("unsupported internal helper");
    }
    let lock = mutation_lock(CONFIG_LOCK)?;
    let _lock = lock;
    let mut command = trusted_command(Path::new(SBIN).join(name));
    command.args(args);
    if tui_safe && presentation.interactive && io::stdin().is_terminal() && !verbose {
        run_process_tui(
            &mut command,
            "Jarvis Administration",
            "Executing typed protected operation…".to_owned(),
            presentation.tui_trace,
        )
    } else {
        run_command(&mut command, SubprocessMode::from_verbose(verbose))
    }
}

fn agents(args: AgentsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    if presentation.json && !matches!(&args.command, AgentsCommand::Status) {
        bail!("--json is supported only for read-only agents status");
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
            let _lock = mutation_lock("/run/jarvis-private-agent-update.lock")?;
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

#[derive(Clone, Copy)]
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
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn release_tags_are_strict() {
        assert!(valid_release_tag("v0.0.11"));
        assert!(!valid_release_tag("v1.2"));
        assert!(!valid_release_tag("v1.2.3;id"));
    }
    #[test]
    fn clap_rejects_unknown_root_command() {
        assert!(Cli::try_parse_from(["jarvis", "shell"]).is_err());
    }
    #[test]
    fn clap_bounds_log_lines() {
        assert!(Cli::try_parse_from(["jarvis", "logs", "core", "--lines", "0"]).is_err());
    }
    #[test]
    fn update_modes_cannot_be_combined() {
        assert!(Cli::try_parse_from(["jarvis", "update", "--latest", "--check"]).is_err());
    }
    #[test]
    fn log_target_is_allowlisted() {
        assert!(Cli::try_parse_from(["jarvis", "logs", "arbitrary.service"]).is_err());
    }
    #[test]
    fn no_color_disables_interactive_rendering() {
        assert!(!terminal_supports_rich_output_for(
            true,
            false,
            Some("xterm-256color")
        ));
        assert!(!terminal_supports_rich_output_for(true, true, Some("dumb")));
        assert!(!terminal_supports_rich_output_for(
            false,
            true,
            Some("xterm-256color")
        ));
        assert!(terminal_supports_rich_output_for(
            true,
            true,
            Some("xterm-256color")
        ));
        assert!(!Presentation::new(true, true).interactive);
    }

    #[test]
    fn tui_exit_reason_requires_documented_pressed_keys() {
        use crossterm::event::{KeyEvent, KeyEventState};

        let pressed = |code, modifiers| {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            })
        };
        assert_eq!(
            close_exit_reason(&pressed(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(TuiExitReason::Quit)
        );
        assert_eq!(
            close_exit_reason(&pressed(KeyCode::Esc, KeyModifiers::NONE)),
            Some(TuiExitReason::Escape)
        );
        assert_eq!(
            close_exit_reason(&pressed(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(TuiExitReason::CtrlC)
        );
        assert_eq!(
            close_exit_reason(&pressed(KeyCode::Char('c'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            close_exit_reason(&Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Release,
                state: KeyEventState::NONE,
            })),
            None
        );
    }

    #[test]
    fn tui_trace_never_records_pasted_contents() {
        let mut trace = TuiTrace::new(true);
        trace.record_event(&Event::Paste("fixture-secret-must-not-appear".to_owned()));
        let rendered = trace.entries.into_iter().collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("contents omitted"));
        assert!(!rendered.contains("fixture-secret-must-not-appear"));
    }

    #[test]
    fn typed_model_input_rejects_newline_injection() {
        assert!(Cli::try_parse_from(["jarvis", "models", "enable", "openai-api", "x\ny"]).is_err());
    }

    #[test]
    fn mutation_lock_is_exclusive() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("admin.lock");
        let _first = mutation_lock(&path).unwrap();
        assert!(mutation_lock(&path).is_err());
    }

    #[test]
    fn child_environment_is_minimal() {
        let command = trusted_command("true");
        let environment: BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        assert_eq!(environment.get(OsStr::new("HOME")), Some(&"/root".into()));
        assert!(!environment.contains_key(OsStr::new("JARVIS_LLM_OPENAI_API_KEY")));
    }

    #[test]
    fn repository_allowlist_rejects_shell_syntax() {
        assert!(valid_repository("HawkeyNL/PersonalJarvis"));
        assert!(!valid_repository("HawkeyNL/PersonalJarvis;id"));
        assert!(!valid_repository("../../etc/passwd"));
    }

    #[test]
    fn trusted_updater_config_is_strict_and_never_shell_parsed() {
        let config = parse_updater_config(
            "JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis\nJARVIS_UPDATE_CHANNEL=stable\n",
        )
        .unwrap();
        assert_eq!(config.repository, "HawkeyNL/PersonalJarvis");
        assert!(parse_updater_config("JARVIS_UPDATE_REPOSITORY=bad;id\n").is_err());
        assert!(parse_updater_config("JARVIS_UPDATE_REPOSITORY=a/b\nUNKNOWN=x\n").is_err());
        assert!(parse_updater_config(
            "JARVIS_UPDATE_REPOSITORY=a/b\nJARVIS_UPDATE_REPOSITORY=c/d\n"
        )
        .is_err());
        assert!(parse_updater_config(
            "JARVIS_UPDATE_REPOSITORY=a/b\nJARVIS_GITHUB_CURL_NETRC=relative\n"
        )
        .is_err());
    }

    #[test]
    fn confirmation_is_explicit_and_non_secret() {
        assert!(confirmation_answer("yes\n"));
        assert!(confirmation_answer("Y"));
        assert!(!confirmation_answer(""));
        assert!(!confirmation_answer("yes; id"));
    }

    #[test]
    fn dashboard_renders_compactly_on_narrow_terminals() {
        use ratatui::{backend::TestBackend, Terminal};

        let report = StatusReport {
            release: Some("v0.0.13".to_owned()),
            services: BTreeMap::from([("Core", "active".to_owned())]),
            updater_enabled: "enabled".to_owned(),
            agent_bundle: None,
        };
        let backend = TestBackend::new(32, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_status_dashboard(frame, &report))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Release:"));
        assert!(rendered.contains("Core:"));
    }

    #[test]
    fn dashboard_includes_codex_broker_state() {
        use ratatui::{backend::TestBackend, Terminal};

        let report = StatusReport {
            release: Some("v0.0.14".to_owned()),
            services: BTreeMap::from([
                ("Core", "active".to_owned()),
                ("Codex broker", "active".to_owned()),
            ]),
            updater_enabled: "enabled".to_owned(),
            agent_bundle: None,
        };
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_status_dashboard(frame, &report))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Codex broker"));
    }

    #[test]
    fn updater_plain_status_has_strict_json_conversion() {
        let values = parse_key_value_output(
            "Current:  v0.0.13\nPrevious: v0.0.12\nLatest: v0.0.14\nUpdater: enabled\n",
        )
        .unwrap();
        assert_eq!(values.get("current").map(String::as_str), Some("v0.0.13"));
        assert_eq!(values.get("previous").map(String::as_str), Some("v0.0.12"));
        assert!(parse_key_value_output("not structured\n").is_err());
        assert!(parse_key_value_output("Current: one\nCurrent: two\n").is_err());
    }

    #[test]
    fn model_policy_json_round_trips_without_credentials() {
        let policy: ModelPolicy = serde_json::from_str(
            r#"{"version":1,"models":[{"provider":"openai-api","model":"gpt-test","enabled":false,"source":"fixture"}]}"#,
        )
        .unwrap();
        assert_eq!(policy.models.len(), 1);
        let output = serde_json::to_string(&policy).unwrap();
        assert!(!output.contains("credential"));
        assert!(!output.contains("api_key"));
    }

    #[test]
    fn tooling_pair_replaces_both_completed_files() {
        let directory = tempfile::tempdir().unwrap();
        let admin_source = directory.path().join("admin-source");
        let updater_source = directory.path().join("updater-source");
        let admin_destination = directory.path().join("jarvis");
        let updater_destination = directory.path().join("updater");
        fs::write(&admin_source, "verified admin").unwrap();
        fs::write(&updater_source, "verified updater").unwrap();
        fs::write(&admin_destination, "old admin").unwrap();
        fs::write(&updater_destination, "old updater").unwrap();
        install_tooling_pair(
            &admin_source,
            &admin_destination,
            &updater_source,
            &updater_destination,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&admin_destination).unwrap(),
            "verified admin"
        );
        assert_eq!(
            fs::read_to_string(&updater_destination).unwrap(),
            "verified updater"
        );
        assert_eq!(
            fs::metadata(admin_destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn tooling_pair_preflight_failure_keeps_both_installed_tools() {
        let directory = tempfile::tempdir().unwrap();
        let admin_source = directory.path().join("admin-source");
        let updater_source = directory.path().join("updater-source");
        let admin_destination = directory.path().join("jarvis");
        let updater_destination = directory.path().join("updater");
        fs::write(&admin_source, "verified admin").unwrap();
        fs::write(&updater_source, "verified updater").unwrap();
        fs::create_dir(&admin_destination).unwrap();
        fs::write(&updater_destination, "old updater").unwrap();
        assert!(install_tooling_pair(
            &admin_source,
            &admin_destination,
            &updater_source,
            &updater_destination,
        )
        .is_err());
        assert!(admin_destination.is_dir());
        assert_eq!(
            fs::read_to_string(updater_destination).unwrap(),
            "old updater"
        );
    }

    #[test]
    fn failed_legacy_migration_rolls_back_only_new_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let created = directory.path().join("created.env");
        let existing = directory.path().join("existing.env");
        fs::write(&created, "new").unwrap();
        fs::write(&existing, "existing").unwrap();
        rollback_new_updater_config(true, &created).unwrap();
        rollback_new_updater_config(false, &existing).unwrap();
        assert!(!created.exists());
        assert_eq!(fs::read_to_string(existing).unwrap(), "existing");
    }
}
