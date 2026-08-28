//! Root-operated, typed administration surface for a Jarvis Home Node.
//!
//! This binary deliberately accepts a small allowlist of operations.  It never
//! evaluates owner input as a shell command and it is not exposed by the API.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitStatus, Stdio},
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use fs2::FileExt;
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Row, Table},
};
use serde::Serialize;

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
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Version,
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
    require_root()?;
    let presentation = Presentation::new(cli.json);
    match cli.command {
        Commands::Version => version(&presentation),
        Commands::Status => status(&presentation),
        Commands::Health => health(&presentation, cli.verbose),
        Commands::Logs(args) => logs(args),
        Commands::Update(args) => update(args, cli.verbose),
        Commands::MigrateInstalledTooling => migrate_installed_tooling(),
        Commands::Models(args) => models(args, cli.verbose),
        Commands::Credentials(args) => credentials(args, cli.verbose),
        Commands::Agents(args) => agents(args, &presentation, cli.verbose),
        Commands::Services {
            command: ServicesCommand::Status,
        } => services(&presentation),
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
}
impl Presentation {
    fn new(json: bool) -> Self {
        Self {
            json,
            interactive: !json && terminal_supports_rich_output(),
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
        return status_tui(&report);
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

fn status_tui(report: &StatusReport) -> Result<()> {
    // `ratatui::run` installs restoration/panic handling around the alternate
    // screen.  Business state is computed before this call, so no privileged
    // operation is coupled to terminal widgets or input events.
    ratatui::run(|terminal| -> io::Result<()> {
        loop {
            terminal.draw(|frame| render_status_dashboard(frame, report))?;
            if event::poll(std::time::Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press
                        && (matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL)))
                    {
                        return Ok(());
                    }
                }
            }
        }
    })
    .map_err(Into::into)
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

fn status_report() -> Result<StatusReport> {
    let mut services = BTreeMap::new();
    for (label, unit) in [
        ("Core", "jarvis-core.service"),
        ("SurrealDB", "jarvis-surrealdb.service"),
        ("Config broker", "jarvis-config-broker.service"),
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
    presentation.intro("Jarvis Home Node health");
    run_program(
        Path::new(LIBEXEC).join("verify-home-node"),
        std::iter::empty::<&str>(),
        SubprocessMode::from_verbose(verbose),
    )?;
    presentation.outro("Health verification passed");
    Ok(())
}

fn services(presentation: &Presentation) -> Result<()> {
    status(presentation)
}

fn logs(args: LogsArgs) -> Result<()> {
    let mut command = trusted_command("journalctl");
    command
        .args(["--no-pager", "-u", args.target.unit(), "-n"])
        .arg(args.lines.to_string());
    if args.follow {
        command.arg("-f");
    }
    run_command(&mut command, SubprocessMode::InheritedInteractive)
}

fn update(args: UpdateArgs, verbose: bool) -> Result<()> {
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
    run_command(&mut command, SubprocessMode::from_verbose(verbose))
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
    atomic_install(&admin, Path::new("/usr/local/sbin/jarvis"))?;
    fs::create_dir_all(LIBEXEC).context("create privileged helper directory")?;
    atomic_install(&updater, &Path::new(LIBEXEC).join("update-core-release"))?;
    ensure_updater_config()?;
    println!("jarvis: installed tooling migrated from verified active release");
    Ok(())
}

fn atomic_install(source: &Path, destination: &Path) -> Result<()> {
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
    fs::copy(source, &temporary).context("stage versioned tooling")?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    fs::rename(&temporary, destination).context("activate versioned tooling")?;
    Ok(())
}

fn ensure_updater_config() -> Result<()> {
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
            return Ok(());
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
    Ok(())
}

fn models(args: ModelsArgs, verbose: bool) -> Result<()> {
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
    compatibility_helper("jarvis-models", arguments, verbose)
}

fn credentials(args: CredentialsArgs, verbose: bool) -> Result<()> {
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
    compatibility_helper("jarvis-credentials", arguments, verbose)
}

fn compatibility_helper(name: &str, args: Vec<String>, verbose: bool) -> Result<()> {
    let allowed = matches!(name, "jarvis-models" | "jarvis-credentials");
    if !allowed {
        bail!("unsupported internal helper");
    }
    let lock = mutation_lock(CONFIG_LOCK)?;
    let _lock = lock;
    let mut command = trusted_command(Path::new(SBIN).join(name));
    command.args(args);
    run_command(&mut command, SubprocessMode::from_verbose(verbose))
}

fn agents(args: AgentsArgs, presentation: &Presentation, verbose: bool) -> Result<()> {
    match args.command {
        AgentsCommand::Status => {
            let bundle = active_bundle()?.context("no active private agent bundle")?;
            if presentation.json {
                println!("{}", serde_json::to_string(&bundle)?);
            } else {
                println!(
                    "Agent bundle: {} ({} agents)",
                    bundle.id, bundle.agent_count
                );
            }
            Ok(())
        }
        AgentsCommand::Check => run_program(
            Path::new(LIBEXEC).join("private-agent-poll"),
            ["--check"],
            if verbose {
                SubprocessMode::Streamed
            } else {
                SubprocessMode::Captured
            },
        ),
        AgentsCommand::Update => {
            let _lock = mutation_lock("/run/jarvis-private-agent-update.lock")?;
            run_program(
                Path::new(LIBEXEC).join("private-agent-poll"),
                std::iter::empty::<&str>(),
                SubprocessMode::from_verbose(verbose),
            )
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
    use std::io::BufRead;
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
    fn atomic_install_replaces_only_completed_file() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        fs::write(&source, "verified tooling").unwrap();
        fs::write(&destination, "old tooling").unwrap();
        atomic_install(&source, &destination).unwrap();
        assert_eq!(fs::read_to_string(destination).unwrap(), "verified tooling");
        assert_eq!(
            fs::metadata(directory.path().join(".destination.new"))
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::NotFound)
        );
        assert_eq!(
            fs::metadata(directory.path().join("destination"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}
