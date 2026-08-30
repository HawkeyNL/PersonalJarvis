//! Single-lifecycle owner console for the Jarvis Home Node.
//!
//! This module owns presentation state only. Every privileged operation still
//! crosses an existing typed helper boundary from `main.rs`.

use super::*;

const VIEWS: [AppView; 9] = [
    AppView::Overview,
    AppView::Update,
    AppView::Health,
    AppView::Services,
    AppView::Agents,
    AppView::Models,
    AppView::Credentials,
    AppView::Logs,
    AppView::System,
];
const LOG_TARGETS: [LogTarget; 7] = [
    LogTarget::Core,
    LogTarget::Surrealdb,
    LogTarget::ConfigBroker,
    LogTarget::CodexBroker,
    LogTarget::Opensandbox,
    LogTarget::Updater,
    LogTarget::AgentsUpdater,
];
const SERVICE_UNITS: [(&str, &str); 7] = [
    ("Core", "jarvis-core.service"),
    ("SurrealDB", "jarvis-surrealdb.service"),
    ("Config broker", "jarvis-config-broker.service"),
    ("Codex broker", "jarvis-codex-broker.service"),
    ("OpenSandbox", "jarvis-opensandbox.service"),
    ("Updater timer", "jarvis-updater.timer"),
    ("Agent updater", "jarvis-private-agent-updater.service"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AppView {
    Overview,
    Update,
    Health,
    Services,
    Agents,
    Models,
    Credentials,
    Logs,
    System,
}

impl AppView {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Update => "Update",
            Self::Health => "Health",
            Self::Services => "Services",
            Self::Agents => "Agents",
            Self::Models => "Models",
            Self::Credentials => "Credentials",
            Self::Logs => "Logs",
            Self::System => "System",
        }
    }
}

#[derive(Clone)]
struct CredentialView {
    provider: String,
    status: String,
}

#[derive(Clone)]
struct ServiceView {
    label: String,
    unit: String,
    active: String,
    enabled: String,
    since: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogEntry {
    timestamp: Option<String>,
    level: Option<String>,
    message: String,
    target: Option<String>,
}

enum Snapshot {
    Status(std::result::Result<StatusReport, String>),
    Agents(std::result::Result<Option<AgentTreeSnapshot>, String>),
    Models(std::result::Result<Vec<ModelRecord>, String>),
    Credentials(Vec<CredentialView>),
    Services(Vec<ServiceView>),
    System(Vec<(String, String)>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentTreeRow {
    Group {
        name: String,
        expanded: bool,
        count: usize,
    },
    Agent {
        group: String,
        agent: AgentTreeAgent,
    },
}

impl AgentTreeRow {
    fn key(&self) -> String {
        match self {
            Self::Group { name, .. } => format!("group:{name}"),
            Self::Agent { agent, .. } => format!("agent:{}", agent.id),
        }
    }
}

#[derive(Clone, Debug)]
enum AppOperation {
    HealthVerification,
    AgentCheck,
    AgentUpdate,
    ModelRefresh,
    ModelEnable { provider: String, model: String },
    ModelDisable { provider: String, model: String },
    LogRefresh,
    LogFollow,
}

impl AppOperation {
    fn title(&self) -> String {
        match self {
            Self::HealthVerification => "Running full Home Node verification".to_owned(),
            Self::AgentCheck => "Checking private agent bundle".to_owned(),
            Self::AgentUpdate => "Updating verified private agent bundle".to_owned(),
            Self::ModelRefresh => "Refreshing model catalog".to_owned(),
            Self::ModelEnable { provider, model } => format!("Enabling {provider}/{model}"),
            Self::ModelDisable { provider, model } => format!("Disabling {provider}/{model}"),
            Self::LogRefresh => "Loading recent logs".to_owned(),
            Self::LogFollow => "Following bounded service logs".to_owned(),
        }
    }

    fn mutating(&self) -> bool {
        matches!(
            self,
            Self::AgentUpdate
                | Self::ModelRefresh
                | Self::ModelEnable { .. }
                | Self::ModelDisable { .. }
        )
    }
}

struct AppChild {
    child: Child,
    pending: Arc<Mutex<VecDeque<ChildStream>>>,
    readers: Vec<thread::JoinHandle<()>>,
    operation: AppOperation,
    stdout: VecDeque<String>,
    stderr: VecDeque<String>,
    _lock: Option<File>,
}

impl AppChild {
    fn spawn(
        mut command: ProcessCommand,
        operation: AppOperation,
        lock: Option<File>,
    ) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("start {}", operation.title()))?;
        let pending = Arc::new(Mutex::new(VecDeque::new()));
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(forward_child_lines(stdout, Arc::clone(&pending), false));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(forward_child_lines(stderr, Arc::clone(&pending), true));
        }
        Ok(Self {
            child,
            pending,
            readers,
            operation,
            stdout: VecDeque::new(),
            stderr: VecDeque::new(),
            _lock: lock,
        })
    }

    fn drain(&mut self, progress: &mut VecDeque<String>) -> Vec<String> {
        let drained = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.drain(..).collect::<Vec<_>>()
        };
        let mut stdout_lines = Vec::new();
        for message in drained {
            let (line, destination) = match message {
                ChildStream::Stdout(line) => {
                    stdout_lines.push(sanitize_terminal_line(&line));
                    (line, &mut self.stdout)
                }
                ChildStream::Stderr(line) => (line, &mut self.stderr),
            };
            push_bounded(destination, line.clone(), 256);
            push_bounded(progress, sanitize_terminal_line(&line), 18);
        }
        stdout_lines
    }

    fn try_status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn finish(mut self, status: ExitStatus, progress: &mut VecDeque<String>) -> AppOutcome {
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let _ = self.drain(progress);
        AppOutcome {
            operation: self.operation.clone(),
            status,
            stdout: std::mem::take(&mut self.stdout)
                .into_iter()
                .collect::<Vec<_>>()
                .join("\n"),
            stderr: std::mem::take(&mut self.stderr)
                .into_iter()
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

impl Drop for AppChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct AppOutcome {
    operation: AppOperation,
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

enum Modal {
    Help,
    Result {
        success: bool,
        title: String,
        detail: String,
    },
    ConfirmModelDisable {
        provider: String,
        model: String,
        selected: usize,
    },
    ConfirmAgentRollback {
        selected: usize,
    },
}

struct JarvisApp {
    root_view: AppView,
    view: AppView,
    navigation: usize,
    selected: usize,
    status: Option<StatusReport>,
    models: Vec<ModelRecord>,
    model_filter: Option<String>,
    credentials: Vec<CredentialView>,
    services: Vec<ServiceView>,
    agent_tree: Option<AgentTreeSnapshot>,
    agent_expanded: BTreeSet<String>,
    agent_expansion_initialized: bool,
    agent_selected: usize,
    agent_scroll: usize,
    agent_page_height: usize,
    logs: VecDeque<LogEntry>,
    log_target: usize,
    /// Zero-based offset into the currently wrapped visual rows.
    log_scroll: usize,
    log_visual_rows: usize,
    log_page_height: usize,
    log_tail: bool,
    log_fixture_follow: bool,
    system: Vec<(String, String)>,
    update: Option<UpdateCenter>,
    modal: Option<Modal>,
    loading: Vec<AppView>,
    error: Option<String>,
    banner: Option<String>,
    active: Option<AppChild>,
    progress: VecDeque<String>,
    health_result: Option<String>,
    sender: mpsc::Sender<Snapshot>,
    receiver: mpsc::Receiver<Snapshot>,
    fixture: bool,
    fixture_failure: bool,
    fixture_completion: Option<Instant>,
    tick: usize,
}

impl JarvisApp {
    fn live(initial: AppView) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut app = Self {
            root_view: initial,
            view: initial,
            navigation: view_index(initial),
            selected: 0,
            status: None,
            models: Vec::new(),
            model_filter: None,
            credentials: Vec::new(),
            services: Vec::new(),
            agent_tree: None,
            agent_expanded: BTreeSet::new(),
            agent_expansion_initialized: false,
            agent_selected: 0,
            agent_scroll: 0,
            agent_page_height: 0,
            logs: VecDeque::new(),
            log_target: 0,
            log_scroll: 0,
            log_visual_rows: 0,
            log_page_height: 0,
            log_tail: false,
            log_fixture_follow: false,
            system: Vec::new(),
            update: None,
            modal: None,
            loading: Vec::new(),
            error: None,
            banner: None,
            active: None,
            progress: VecDeque::new(),
            health_result: None,
            sender,
            receiver,
            fixture: false,
            fixture_failure: false,
            fixture_completion: None,
            tick: 0,
        };
        app.refresh_status();
        app.enter(initial)?;
        Ok(app)
    }

    #[cfg(any(feature = "tui-preview", test))]
    fn fixture(initial: AppView, failure: bool) -> Self {
        let (sender, receiver) = mpsc::channel();
        let agent_tree = fixture_agent_tree();
        let status = StatusReport {
            release: Some("v0.0.16".to_owned()),
            services: BTreeMap::from([
                ("Core", if failure { "failed" } else { "active" }.to_owned()),
                ("SurrealDB", "active".to_owned()),
                ("Config broker", "active".to_owned()),
                ("Codex broker", "active".to_owned()),
                (
                    "OpenSandbox",
                    if failure { "failed" } else { "active" }.to_owned(),
                ),
            ]),
            updater_enabled: "enabled".to_owned(),
            agent_bundle: Some(AgentBundle {
                id: agent_tree.bundle_id.clone(),
                agent_count: agent_tree.agents.len(),
            }),
        };
        let mut app = Self {
            root_view: initial,
            view: initial,
            navigation: view_index(initial),
            selected: 0,
            status: Some(status),
            models: vec![
                ModelRecord {
                    provider: "openai-api".to_owned(),
                    model: "gpt-fixture".to_owned(),
                    enabled: true,
                    source: "catalog fixture".to_owned(),
                },
                ModelRecord {
                    provider: "anthropic-api".to_owned(),
                    model: "claude-fixture-with-a-long-safe-name".to_owned(),
                    enabled: false,
                    source: "policy fixture".to_owned(),
                },
            ],
            model_filter: None,
            credentials: fixture_credentials(),
            services: fixture_services(failure),
            agent_tree: Some(agent_tree),
            agent_expanded: BTreeSet::from(["Trading".to_owned()]),
            agent_expansion_initialized: true,
            agent_selected: 0,
            agent_scroll: 0,
            agent_page_height: 0,
            logs: fixture_logs(),
            log_target: 0,
            log_scroll: 0,
            log_visual_rows: 0,
            log_page_height: 0,
            log_tail: false,
            log_fixture_follow: false,
            system: fixture_system(),
            update: Some(UpdateCenter::fixture(failure)),
            modal: None,
            loading: Vec::new(),
            error: None,
            banner: Some("Fixture Home Node · zero administrative capability".to_owned()),
            active: None,
            progress: VecDeque::new(),
            health_result: None,
            sender,
            receiver,
            fixture: true,
            fixture_failure: failure,
            fixture_completion: None,
            tick: 0,
        };
        app.navigation = view_index(initial);
        app
    }

    fn enter(&mut self, view: AppView) -> Result<()> {
        self.view = view;
        self.navigation = view_index(view);
        self.selected = 0;
        self.error = None;
        match view {
            AppView::Overview | AppView::Health | AppView::Services | AppView::Agents => {
                if self.status.is_none() {
                    self.refresh_status();
                }
                if matches!(view, AppView::Services | AppView::Agents)
                    && self.services.is_empty()
                    && !self.fixture
                {
                    self.refresh_services();
                }
                if view == AppView::Agents && self.agent_tree.is_none() && !self.fixture {
                    self.refresh_agent_tree();
                }
            }
            AppView::Update => {
                if self.update.is_none() {
                    self.update = Some(if self.fixture {
                        #[cfg(feature = "tui-preview")]
                        {
                            UpdateCenter::fixture(self.fixture_failure)
                        }
                        #[cfg(not(feature = "tui-preview"))]
                        unreachable!()
                    } else {
                        match UpdateCenter::live() {
                            Ok(center) => center,
                            Err(error) => {
                                let mut center = UpdateCenter::base(None);
                                center.show_result(
                                    false,
                                    "Update Center initialization failed".to_owned(),
                                    safe_error(&format!("trusted updater status: {error:#}")),
                                );
                                center
                            }
                        }
                    });
                }
            }
            AppView::Models => {
                if self.models.is_empty() && !self.fixture {
                    self.refresh_models();
                }
            }
            AppView::Credentials => {
                if self.credentials.is_empty() && !self.fixture {
                    self.refresh_credentials();
                }
            }
            AppView::Logs => {
                if self.logs.is_empty() && !self.fixture {
                    self.start_logs(false)?;
                }
            }
            AppView::System => {
                if self.system.is_empty() && !self.fixture {
                    self.refresh_system();
                }
            }
        }
        Ok(())
    }

    fn refresh_status(&mut self) {
        if self.fixture || self.loading.contains(&AppView::Overview) {
            return;
        }
        self.loading.push(AppView::Overview);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = status_report().map_err(|error| format!("{error:#}"));
            let _ = sender.send(Snapshot::Status(result));
        });
    }

    fn refresh_models(&mut self) {
        if self.fixture || self.loading.contains(&AppView::Models) {
            return;
        }
        self.loading.push(AppView::Models);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = read_model_policy()
                .map(|policy| policy.models)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Snapshot::Models(result));
        });
    }

    fn refresh_agent_tree(&mut self) {
        if self.fixture || self.loading.contains(&AppView::Agents) {
            return;
        }
        self.loading.push(AppView::Agents);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = active_agent_tree().map_err(|error| format!("{error:#}"));
            let _ = sender.send(Snapshot::Agents(result));
        });
    }

    fn refresh_credentials(&mut self) {
        if self.fixture || self.loading.contains(&AppView::Credentials) {
            return;
        }
        self.loading.push(AppView::Credentials);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let mut statuses = credential_statuses()
                .into_iter()
                .map(|status| CredentialView {
                    provider: status.provider.to_owned(),
                    status: if status.configured {
                        "configured"
                    } else {
                        "not configured"
                    }
                    .to_owned(),
                })
                .collect::<Vec<_>>();
            statuses.push(CredentialView {
                provider: "ollama-local".to_owned(),
                status: "no credential required".to_owned(),
            });
            let _ = sender.send(Snapshot::Credentials(statuses));
        });
    }

    fn refresh_system(&mut self) {
        if self.fixture || self.loading.contains(&AppView::System) {
            return;
        }
        self.loading.push(AppView::System);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let _ = sender.send(Snapshot::System(system_information()));
        });
    }

    fn refresh_services(&mut self) {
        if self.fixture || self.loading.contains(&AppView::Services) {
            return;
        }
        self.loading.push(AppView::Services);
        let sender = self.sender.clone();
        thread::spawn(move || {
            let services = SERVICE_UNITS
                .iter()
                .map(|(label, unit)| ServiceView {
                    label: (*label).to_owned(),
                    unit: (*unit).to_owned(),
                    active: systemctl_value("is-active", unit),
                    enabled: systemctl_value("is-enabled", unit),
                    since: systemctl_property(unit, "ActiveEnterTimestamp"),
                })
                .collect();
            let _ = sender.send(Snapshot::Services(services));
        });
    }

    fn tick(&mut self) -> Result<()> {
        self.tick = self.tick.wrapping_add(1);
        while let Ok(snapshot) = self.receiver.try_recv() {
            match snapshot {
                Snapshot::Status(result) => {
                    self.loading.retain(|view| *view != AppView::Overview);
                    match result {
                        Ok(status) => self.status = Some(status),
                        Err(error) => self.error = Some(safe_error(&error)),
                    }
                }
                Snapshot::Agents(result) => {
                    self.loading.retain(|view| *view != AppView::Agents);
                    match result {
                        Ok(tree) => self.replace_agent_tree(tree),
                        Err(error) => self.error = Some(safe_error(&error)),
                    }
                }
                Snapshot::Models(result) => {
                    self.loading.retain(|view| *view != AppView::Models);
                    match result {
                        Ok(models) => self.models = models,
                        Err(error) => self.error = Some(safe_error(&error)),
                    }
                }
                Snapshot::Credentials(statuses) => {
                    self.loading.retain(|view| *view != AppView::Credentials);
                    self.credentials = statuses;
                }
                Snapshot::Services(services) => {
                    self.loading.retain(|view| *view != AppView::Services);
                    self.services = services;
                }
                Snapshot::System(values) => {
                    self.loading.retain(|view| *view != AppView::System);
                    self.system = values;
                }
            }
        }
        if let Some(update) = self.update.as_mut() {
            if let Err(error) = update.tick() {
                update.terminate_active();
                update.show_result(
                    false,
                    "Update Center operation failed".to_owned(),
                    safe_error(&format!("{error:#}")),
                );
            }
        }
        if self
            .fixture_completion
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fixture_completion = None;
            self.complete_fixture_operation();
        }
        let status = if let Some(active) = self.active.as_mut() {
            let live_lines = active.drain(&mut self.progress);
            if matches!(active.operation, AppOperation::LogFollow) {
                for line in live_lines {
                    push_bounded(&mut self.logs, safe_log_entry(&line), 500);
                }
            }
            match active.try_status() {
                Ok(status) => status,
                Err(error) => {
                    let title = active.operation.title();
                    if let Some(mut child) = self.active.take() {
                        child.terminate();
                    }
                    self.modal = Some(Modal::Result {
                        success: false,
                        title: format!("{title} failed"),
                        detail: safe_error(&format!("poll trusted helper: {error}")),
                    });
                    None
                }
            }
        } else {
            None
        };
        if let Some(status) = status {
            let active = self.active.take().context("completed helper is missing")?;
            let outcome = active.finish(status, &mut self.progress);
            self.complete_operation(outcome);
        }
        Ok(())
    }

    fn complete_fixture_operation(&mut self) {
        let title = self
            .banner
            .take()
            .unwrap_or_else(|| "Fixture operation".to_owned());
        let failure = self.fixture_failure && title.contains("verification");
        if title.contains("verification") {
            self.health_result = Some(if failure {
                "failed · fixture verification reported a degraded service".to_owned()
            } else {
                "passed".to_owned()
            });
        }
        self.modal = Some(Modal::Result {
            success: !failure,
            title: if failure {
                "Fixture verification failed".to_owned()
            } else {
                format!("{title} completed")
            },
            detail: "Persistent fixture result · no helper or mutation was invoked".to_owned(),
        });
    }

    fn complete_operation(&mut self, outcome: AppOutcome) {
        let success = outcome.status.success();
        if matches!(
            outcome.operation,
            AppOperation::LogRefresh | AppOperation::LogFollow
        ) {
            for line in outcome.stdout.lines() {
                push_bounded(&mut self.logs, safe_log_entry(line), 500);
            }
            if matches!(outcome.operation, AppOperation::LogFollow) && !success {
                self.error = Some(operation_detail(&outcome));
            }
            return;
        }
        let title = if success {
            format!("{} succeeded", outcome.operation.title())
        } else {
            format!("{} failed", outcome.operation.title())
        };
        let detail = operation_detail(&outcome);
        if matches!(outcome.operation, AppOperation::HealthVerification) {
            self.health_result = Some(if success {
                "passed".to_owned()
            } else {
                format!("failed · {detail}")
            });
        }
        let refresh_status = matches!(
            outcome.operation,
            AppOperation::HealthVerification | AppOperation::AgentCheck | AppOperation::AgentUpdate
        );
        let refresh_models = matches!(
            outcome.operation,
            AppOperation::ModelRefresh
                | AppOperation::ModelEnable { .. }
                | AppOperation::ModelDisable { .. }
        );
        self.modal = Some(Modal::Result {
            success,
            title,
            detail,
        });
        if refresh_status {
            self.refresh_status();
            self.refresh_agent_tree();
        }
        if refresh_models {
            self.refresh_models();
        }
    }

    fn start_operation(&mut self, operation: AppOperation) -> Result<()> {
        if self.active.is_some() || self.fixture_completion.is_some() {
            return Ok(());
        }
        self.progress.clear();
        if matches!(operation, AppOperation::HealthVerification) {
            self.health_result = None;
        }
        push_bounded(&mut self.progress, operation.title(), 18);
        if self.fixture {
            self.banner = Some(operation.title());
            self.fixture_completion = Some(Instant::now() + Duration::from_millis(250));
            return Ok(());
        }
        match operation_command(&operation, LOG_TARGETS[self.log_target])
            .and_then(|(command, lock)| AppChild::spawn(command, operation.clone(), lock))
        {
            Ok(child) => self.active = Some(child),
            Err(error) => {
                self.modal = Some(Modal::Result {
                    success: false,
                    title: format!("{} failed", operation.title()),
                    detail: safe_error(&format!("{error:#}")),
                });
            }
        }
        Ok(())
    }

    fn start_logs(&mut self, follow: bool) -> Result<()> {
        #[cfg(any(feature = "tui-preview", test))]
        if self.fixture {
            self.log_fixture_follow = follow;
            self.log_tail = follow;
            if !follow {
                self.logs = fixture_logs();
                self.log_scroll = 0;
                self.log_visual_rows = 0;
                self.banner = Some("Fixture logs refreshed · no journal access".to_owned());
            } else {
                self.banner = Some("Fixture bounded follow mode enabled".to_owned());
            }
            return Ok(());
        }
        if !follow {
            self.logs.clear();
            self.log_scroll = 0;
            self.log_visual_rows = 0;
            self.log_tail = false;
        } else {
            self.log_tail = true;
        }
        self.start_operation(if follow {
            AppOperation::LogFollow
        } else {
            AppOperation::LogRefresh
        })
    }

    fn handle_event(&mut self, event: Event) -> Result<Option<TuiExitReason>> {
        let Event::Key(key) = event else {
            return Ok(None);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);

        if self.modal.is_some() {
            return self.handle_modal(key.code, ctrl_c);
        }
        if key.code == KeyCode::Char('?') {
            self.modal = Some(Modal::Help);
            return Ok(None);
        }
        if self.view == AppView::Update {
            return self.handle_update_event(Event::Key(key), ctrl_c);
        }
        if ctrl_c {
            if self.mutation_running() {
                self.banner = Some("Trusted mutation running · cancellation disabled".to_owned());
                return Ok(None);
            }
            self.stop_read_only_child();
            return Ok(Some(TuiExitReason::CtrlC));
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return self.back_or_exit(if key.code == KeyCode::Esc {
                TuiExitReason::Escape
            } else {
                TuiExitReason::Quit
            });
        }
        if key.code == KeyCode::Char('r') {
            self.refresh_current()?;
            return Ok(None);
        }
        if key.code == KeyCode::Enter
            && ((self.view == AppView::Overview && self.navigation == VIEWS.len())
                || (self.view == AppView::Health && self.selected == 2))
        {
            return self.back_or_exit(TuiExitReason::SelectedClose);
        }

        match self.view {
            AppView::Overview => self.handle_overview(key.code),
            AppView::Health => self.handle_health(key.code)?,
            AppView::Services => self.handle_table_navigation(key.code, self.services.len()),
            AppView::Agents => self.handle_agents(key.code)?,
            AppView::Models => self.handle_models(key.code)?,
            AppView::Credentials => self.handle_table_navigation(key.code, self.credentials.len()),
            AppView::Logs => self.handle_logs(key.code)?,
            AppView::System => self.handle_table_navigation(key.code, self.system.len()),
            AppView::Update => unreachable!(),
        }
        Ok(None)
    }

    fn handle_update_event(&mut self, event: Event, ctrl_c: bool) -> Result<Option<TuiExitReason>> {
        let key_code = match &event {
            Event::Key(key) => key.code,
            _ => return Ok(None),
        };
        let update = self
            .update
            .as_mut()
            .context("Update Center is unavailable")?;
        if (key_code == KeyCode::Char('q') || ctrl_c) && update.running_mutation() {
            update.handle_event(event)?;
            return Ok(None);
        }
        if key_code == KeyCode::Esc && update.screen != UpdateScreen::Overview {
            update.handle_event(event)?;
            return Ok(None);
        }
        if key_code == KeyCode::Esc || key_code == KeyCode::Char('q') || ctrl_c {
            return self.back_or_exit(if ctrl_c {
                TuiExitReason::CtrlC
            } else if key_code == KeyCode::Esc {
                TuiExitReason::Escape
            } else {
                TuiExitReason::Quit
            });
        }
        if key_code == KeyCode::Char('r') && update.screen == UpdateScreen::Overview {
            if let Err(error) = update.start(UpdateOperation::Status) {
                update.show_result(
                    false,
                    "Update Center refresh failed".to_owned(),
                    safe_error(&format!("{error:#}")),
                );
            }
            return Ok(None);
        }
        match update.handle_event(event) {
            Ok(Some(_)) => return self.back_or_exit(TuiExitReason::SelectedClose),
            Ok(None) => {}
            Err(error) => {
                update.terminate_active();
                update.show_result(
                    false,
                    "Update Center operation failed".to_owned(),
                    safe_error(&format!("{error:#}")),
                );
            }
        }
        Ok(None)
    }

    fn handle_modal(&mut self, code: KeyCode, ctrl_c: bool) -> Result<Option<TuiExitReason>> {
        if ctrl_c {
            self.modal = None;
            return Ok(Some(TuiExitReason::CtrlC));
        }
        match self.modal.as_mut() {
            Some(Modal::Help | Modal::Result { .. }) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                    self.modal = None;
                }
            }
            Some(Modal::ConfirmModelDisable { selected, .. }) => match code {
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h' | 'k') => *selected = 0,
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l' | 'j') => *selected = 1,
                KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                KeyCode::Enter => {
                    if *selected == 0 {
                        self.modal = None;
                    } else if let Some(Modal::ConfirmModelDisable {
                        provider, model, ..
                    }) = self.modal.take()
                    {
                        self.start_operation(AppOperation::ModelDisable { provider, model })?;
                    }
                }
                _ => {}
            },
            Some(Modal::ConfirmAgentRollback { selected }) => match code {
                KeyCode::Left | KeyCode::Up | KeyCode::Char('h' | 'k') => *selected = 0,
                KeyCode::Right | KeyCode::Down | KeyCode::Char('l' | 'j') => *selected = 1,
                KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
                KeyCode::Enter => {
                    if *selected == 0 {
                        self.modal = None;
                    } else {
                        self.modal = Some(Modal::Result {
                            success: false,
                            title: "Agent rollback unavailable".to_owned(),
                            detail:
                                "The trusted Rust transactional agent activator is not installed."
                                    .to_owned(),
                        });
                    }
                }
                _ => {}
            },
            None => {}
        }
        Ok(None)
    }

    fn handle_overview(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.navigation = wrap_up(self.navigation, VIEWS.len() + 1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.navigation = (self.navigation + 1) % (VIEWS.len() + 1)
            }
            KeyCode::Enter if self.navigation < VIEWS.len() => {
                let _ = self.enter(VIEWS[self.navigation]);
            }
            _ => {}
        }
    }

    fn handle_health(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = wrap_up(self.selected, 3),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1) % 3,
            KeyCode::Enter => match self.selected {
                0 => self.refresh_status(),
                1 => self.start_operation(AppOperation::HealthVerification)?,
                2 => unreachable!("Back is handled by the application router"),
                _ => unreachable!(),
            },
            _ => {}
        }
        Ok(())
    }

    fn handle_agents(&mut self, code: KeyCode) -> Result<()> {
        let rows = self.agent_rows();
        let count = rows.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.agent_selected = self.agent_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.agent_selected = self
                    .agent_selected
                    .saturating_add(1)
                    .min(count.saturating_sub(1))
            }
            KeyCode::PageUp => {
                self.agent_selected = self
                    .agent_selected
                    .saturating_sub(self.agent_page_height.max(1))
            }
            KeyCode::PageDown => {
                self.agent_selected = self
                    .agent_selected
                    .saturating_add(self.agent_page_height.max(1))
                    .min(count.saturating_sub(1))
            }
            KeyCode::Home => self.agent_selected = 0,
            KeyCode::End => self.agent_selected = count.saturating_sub(1),
            KeyCode::Enter | KeyCode::Right => self.open_agent_row(rows.get(self.agent_selected)),
            KeyCode::Left => self.collapse_or_parent(rows.get(self.agent_selected)),
            KeyCode::Char('e') => {
                self.agent_expanded = self.agent_groups().into_iter().collect();
                self.agent_expansion_initialized = true;
            }
            KeyCode::Char('c') => {
                self.agent_expanded.clear();
                self.agent_expansion_initialized = true;
                self.agent_selected = self
                    .agent_selected
                    .min(self.agent_rows().len().saturating_sub(1));
            }
            KeyCode::Char('x') => self.start_operation(AppOperation::AgentCheck)?,
            KeyCode::Char('u') => self.start_operation(AppOperation::AgentUpdate)?,
            KeyCode::Char('b') => {
                self.modal = Some(Modal::ConfirmAgentRollback { selected: 0 });
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_models(&mut self, code: KeyCode) -> Result<()> {
        let visible = self.visible_model_indices();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = wrap_up(self.selected, visible.len().max(1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % visible.len().max(1)
            }
            KeyCode::Char('f') => {
                self.cycle_model_filter();
                self.selected = 0;
            }
            KeyCode::Char('e') => {
                if let Some(model) = self.selected_model() {
                    self.start_operation(AppOperation::ModelEnable {
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                    })?;
                }
            }
            KeyCode::Char('d') => {
                if let Some(model) = self.selected_model() {
                    self.modal = Some(Modal::ConfirmModelDisable {
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                        selected: 0,
                    });
                }
            }
            KeyCode::Enter => {
                if let Some(model) = self.selected_model() {
                    self.modal = Some(Modal::Result {
                        success: true,
                        title: format!("{}/{}", model.provider, model.model),
                        detail: format!("Enabled: {} · Source: {}", model.enabled, model.source),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_logs(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Tab | KeyCode::Right => {
                self.log_target = (self.log_target + 1) % LOG_TARGETS.len();
                self.stop_read_only_child();
                self.start_logs(false)?;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.log_target = wrap_up(self.log_target, LOG_TARGETS.len());
                self.stop_read_only_child();
                self.start_logs(false)?;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.log_tail = false;
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_scroll = self.log_scroll.saturating_add(1).min(self.log_max_scroll());
                self.log_tail = self.log_scroll == self.log_max_scroll();
            }
            KeyCode::PageUp => {
                self.log_tail = false;
                self.log_scroll = self
                    .log_scroll
                    .saturating_sub(self.log_page_height.saturating_sub(1).max(1));
            }
            KeyCode::PageDown => {
                self.log_scroll = self
                    .log_scroll
                    .saturating_add(self.log_page_height.saturating_sub(1).max(1))
                    .min(self.log_max_scroll());
                self.log_tail = self.log_scroll == self.log_max_scroll();
            }
            KeyCode::Home => {
                self.log_tail = false;
                self.log_scroll = 0;
            }
            KeyCode::End => {
                self.log_tail = true;
                self.log_scroll = self.log_max_scroll();
            }
            KeyCode::Char('f') => {
                if self.log_following() {
                    if self.fixture {
                        self.log_fixture_follow = false;
                    } else {
                        self.stop_read_only_child();
                    }
                    self.log_tail = false;
                    self.banner = Some("Log follow stopped".to_owned());
                } else {
                    self.start_logs(true)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn log_max_scroll(&self) -> usize {
        self.log_visual_rows.saturating_sub(self.log_page_height)
    }

    fn log_following(&self) -> bool {
        self.log_fixture_follow
            || self
                .active
                .as_ref()
                .is_some_and(|child| matches!(child.operation, AppOperation::LogFollow))
    }

    fn handle_table_navigation(&mut self, code: KeyCode, count: usize) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = wrap_up(self.selected, count.max(1))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1) % count.max(1)
            }
            _ => {}
        }
    }

    fn refresh_current(&mut self) -> Result<()> {
        match self.view {
            AppView::Overview | AppView::Health => self.refresh_status(),
            AppView::Agents => {
                self.refresh_status();
                self.refresh_services();
                self.refresh_agent_tree();
            }
            AppView::Services => self.refresh_services(),
            AppView::Update => {
                if let Some(update) = self.update.as_mut() {
                    if update.screen == UpdateScreen::Overview {
                        update.start(UpdateOperation::Status)?;
                    }
                }
            }
            AppView::Models => self.start_operation(AppOperation::ModelRefresh)?,
            AppView::Credentials => self.refresh_credentials(),
            AppView::Logs => {
                self.stop_read_only_child();
                self.start_logs(false)?;
            }
            AppView::System => self.refresh_system(),
        }
        Ok(())
    }

    fn back_or_exit(&mut self, reason: TuiExitReason) -> Result<Option<TuiExitReason>> {
        if self.mutation_running() {
            self.banner = Some("Trusted mutation running · cancellation disabled".to_owned());
            return Ok(None);
        }
        self.stop_read_only_child();
        if self.view == self.root_view {
            Ok(Some(reason))
        } else {
            self.enter(self.root_view)?;
            Ok(None)
        }
    }

    fn mutation_running(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|child| child.operation.mutating())
            || self
                .update
                .as_ref()
                .is_some_and(UpdateCenter::running_mutation)
    }

    fn stop_read_only_child(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|child| !child.operation.mutating())
        {
            if let Some(mut child) = self.active.take() {
                child.terminate();
            }
        }
    }

    fn visible_model_indices(&self) -> Vec<usize> {
        self.models
            .iter()
            .enumerate()
            .filter(|(_, model)| {
                self.model_filter
                    .as_ref()
                    .is_none_or(|filter| model.provider == *filter)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_model(&self) -> Option<&ModelRecord> {
        let index = *self.visible_model_indices().get(self.selected)?;
        self.models.get(index)
    }

    fn agent_groups(&self) -> Vec<String> {
        let mut groups = self
            .agent_tree
            .as_ref()
            .into_iter()
            .flat_map(|tree| &tree.agents)
            .map(|agent| {
                agent
                    .group
                    .clone()
                    .unwrap_or_else(|| "Ungrouped".to_owned())
            })
            .collect::<Vec<_>>();
        groups.sort_by_key(|group| group.to_ascii_lowercase());
        groups.dedup();
        groups
    }

    fn agent_rows(&self) -> Vec<AgentTreeRow> {
        let mut grouped = BTreeMap::<String, Vec<AgentTreeAgent>>::new();
        if let Some(tree) = &self.agent_tree {
            for agent in &tree.agents {
                grouped
                    .entry(
                        agent
                            .group
                            .clone()
                            .unwrap_or_else(|| "Ungrouped".to_owned()),
                    )
                    .or_default()
                    .push(agent.clone());
            }
        }
        let mut groups = grouped.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|(group, _)| group.to_ascii_lowercase());
        let mut rows = Vec::new();
        for (group, mut agents) in groups {
            agents.sort_by_key(|agent| agent.name.to_ascii_lowercase());
            let expanded = self.agent_expanded.contains(&group);
            rows.push(AgentTreeRow::Group {
                name: group.clone(),
                expanded,
                count: agents.len(),
            });
            if expanded {
                rows.extend(agents.into_iter().map(|agent| AgentTreeRow::Agent {
                    group: group.clone(),
                    agent,
                }));
            }
        }
        rows
    }

    fn replace_agent_tree(&mut self, tree: Option<AgentTreeSnapshot>) {
        let selected_key = self
            .agent_rows()
            .get(self.agent_selected)
            .map(AgentTreeRow::key);
        self.agent_tree = tree;
        let groups = self.agent_groups();
        self.agent_expanded.retain(|group| groups.contains(group));
        if !self.agent_expansion_initialized {
            if let Some(first) = groups.first() {
                self.agent_expanded.insert(first.clone());
            }
            self.agent_expansion_initialized = true;
        }
        let rows = self.agent_rows();
        self.agent_selected = selected_key
            .and_then(|key| rows.iter().position(|row| row.key() == key))
            .unwrap_or_else(|| self.agent_selected.min(rows.len().saturating_sub(1)));
        self.agent_scroll = self.agent_scroll.min(rows.len().saturating_sub(1));
    }

    fn open_agent_row(&mut self, row: Option<&AgentTreeRow>) {
        match row {
            Some(AgentTreeRow::Group { name, expanded, .. }) => {
                if *expanded {
                    self.agent_expanded.remove(name);
                } else {
                    self.agent_expanded.insert(name.clone());
                }
                self.agent_expansion_initialized = true;
            }
            Some(AgentTreeRow::Agent { group, agent }) => {
                let bundle = self
                    .agent_tree
                    .as_ref()
                    .map_or("unavailable", |tree| tree.bundle_id.as_str());
                self.modal = Some(Modal::Result {
                    success: true,
                    title: agent.name.clone(),
                    detail: format!(
                        "Agent: {} · Group: {group} · State: active · Model policy: {} · Bundle: {bundle} · prompt/body: never loaded",
                        agent.id,
                        agent.model_policy.as_deref().unwrap_or("not declared")
                    ),
                });
            }
            None => {}
        }
    }

    fn collapse_or_parent(&mut self, row: Option<&AgentTreeRow>) {
        match row {
            Some(AgentTreeRow::Group {
                name,
                expanded: true,
                ..
            }) => {
                self.agent_expanded.remove(name);
                self.agent_expansion_initialized = true;
            }
            Some(AgentTreeRow::Agent { group, .. }) => {
                if let Some(index) = self.agent_rows().iter().position(
                    |row| matches!(row, AgentTreeRow::Group { name, .. } if name == group),
                ) {
                    self.agent_selected = index;
                }
            }
            _ => {}
        }
    }

    fn cycle_model_filter(&mut self) {
        let mut providers = self
            .models
            .iter()
            .map(|model| model.provider.clone())
            .collect::<Vec<_>>();
        providers.sort();
        providers.dedup();
        self.model_filter = match &self.model_filter {
            None => providers.first().cloned(),
            Some(current) => providers
                .iter()
                .position(|provider| provider == current)
                .and_then(|index| providers.get(index + 1).cloned()),
        };
    }
}

impl Drop for JarvisApp {
    fn drop(&mut self) {
        if let Some(mut child) = self.active.take() {
            child.terminate();
        }
        if let Some(update) = self.update.as_mut() {
            update.terminate_active();
        }
    }
}

pub(super) fn run_live(initial: AppView, trace_enabled: bool) -> Result<()> {
    run(JarvisApp::live(initial)?, trace_enabled)
}

#[cfg(feature = "tui-preview")]
pub(super) fn run_fixture(initial: AppView, trace_enabled: bool, failure: bool) -> Result<()> {
    run(JarvisApp::fixture(initial, failure), trace_enabled)
}

fn run(mut app: JarvisApp, trace_enabled: bool) -> Result<()> {
    let mut trace = TuiTrace::new(trace_enabled);
    let mut first_frame = true;
    let result = ratatui::run(|terminal| -> io::Result<TuiExitReason> {
        trace.record("unified Jarvis application closure entered");
        loop {
            app.tick().map_err(|error| {
                io::Error::other(format!("Jarvis application state: {error:#}"))
            })?;
            let draw = terminal.draw(|frame| render(frame, &mut app)).map(|_| ());
            trace.io("terminal.draw", draw)?;
            if first_frame {
                trace.record("first unified application frame drawn");
                first_frame = false;
            }
            if trace.io("event.poll", event::poll(Duration::from_millis(100)))? {
                let event = trace.io("event.read", event::read())?;
                trace.record_event(&event);
                if let Some(reason) = app.handle_event(event).map_err(|error| {
                    io::Error::other(format!("Jarvis application input: {error:#}"))
                })? {
                    return Ok(reason);
                }
            }
        }
    });
    let reason = result.as_ref().ok().copied();
    let failure_stage = trace
        .failure
        .clone()
        .unwrap_or_else(|| "terminal lifecycle initialization or cleanup".to_owned());
    trace.finish("home", &result, reason, true);
    result.map(|_| ()).map_err(|error| {
        anyhow::Error::new(error).context(format!(
            "Jarvis interactive terminal UI could not continue after terminal restoration; \
             stage: {failure_stage}; run `jarvis terminal-diagnostics` or use an explicit \
             command with plain/JSON output"
        ))
    })
}

fn render(frame: &mut ratatui::Frame, app: &mut JarvisApp) {
    let area = frame.area();
    let release = app
        .status
        .as_ref()
        .and_then(|status| status.release.as_deref())
        .unwrap_or("loading…");
    let outer = Block::default()
        .title(format!(" Jarvis Home Node · {release} "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let shell = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(inner);
    render_header(frame, shell[0], app);
    if app.view == AppView::Logs {
        render_view(frame, shell[1], app);
    } else if inner.width >= 72 {
        let panes =
            Layout::horizontal([Constraint::Length(20), Constraint::Min(20)]).split(shell[1]);
        render_navigation(frame, panes[0], app);
        render_view(frame, panes[1], app);
    } else {
        render_view(frame, shell[1], app);
    }
    render_footer(frame, shell[2], app);
    if let Some(modal) = &app.modal {
        render_modal(frame, centered(area, 70, 45), modal);
    }
}

fn render_header(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let spans = VIEWS.iter().flat_map(|view| {
        let selected = *view == app.view;
        [
            Span::styled(
                format!(" {} ", view.title()),
                Style::default().fg(if selected {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("│"),
        ]
    });
    frame.render_widget(Paragraph::new(Line::from(spans.collect::<Vec<_>>())), area);
}

fn render_navigation(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let mut lines = VIEWS
        .iter()
        .enumerate()
        .map(|(index, view)| selection_line(index == app.navigation, view.title()))
        .collect::<Vec<_>>();
    lines.push(selection_line(app.navigation == VIEWS.len(), "Exit"));
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::RIGHT)),
        area,
    );
}

fn render_view(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut JarvisApp) {
    match app.view {
        AppView::Overview => render_overview(frame, area, app),
        AppView::Update => render_update(frame, area, app),
        AppView::Health => render_health(frame, area, app),
        AppView::Services => render_services(frame, area, app),
        AppView::Agents => render_agents(frame, area, app),
        AppView::Models => render_models(frame, area, app),
        AppView::Credentials => render_credentials(frame, area, app),
        AppView::Logs => render_logs(frame, area, app),
        AppView::System => render_system(frame, area, app),
    }
}

fn render_overview(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let mut lines = vec![Line::styled("Overview", Style::default().fg(Color::Cyan))];
    if let Some(status) = &app.status {
        lines.push(key_value(
            "Release",
            status.release.as_deref().unwrap_or("unavailable"),
        ));
        for (name, state) in &status.services {
            lines.push(status_line(name, state));
        }
        let agents = status.agent_bundle.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |bundle| format!("{} · {} agents", bundle.id, bundle.agent_count),
        );
        lines.push(key_value("Agents", &agents));
        lines.push(status_line("Updater", &status.updater_enabled));
        if let Some(update) = &app.update {
            lines.push(key_value(
                "Core update",
                match update.summary.update_available {
                    Some(true) => "available",
                    Some(false) => "not available",
                    None => "unknown",
                },
            ));
        }
    } else {
        lines.push(Line::from("◐ Loading local Home Node state…"));
    }
    lines.push(Line::from(""));
    if area.width < 72 {
        lines.extend(
            VIEWS
                .iter()
                .enumerate()
                .map(|(index, view)| selection_line(index == app.navigation, view.title())),
        );
        lines.push(selection_line(app.navigation == VIEWS.len(), "Exit"));
    } else {
        lines.push(Line::from("Select a section from the navigation pane."));
    }
    render_lines(frame, area, lines, app);
}

fn render_update(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let Some(center) = &app.update else {
        render_empty(frame, area, "Update Center is loading…");
        return;
    };
    let mut lines = vec![Line::styled(
        "Update Center",
        Style::default().fg(Color::Cyan),
    )];
    lines.extend([
        key_value(
            "Current",
            center.summary.current.as_deref().unwrap_or("loading…"),
        ),
        key_value(
            "Latest stable",
            center.summary.latest.as_deref().unwrap_or("loading…"),
        ),
        key_value(
            "Update available",
            match center.summary.update_available {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            },
        ),
        key_value(
            "Updater timer",
            center.summary.updater.as_deref().unwrap_or("loading…"),
        ),
        key_value(
            "Rollback",
            center.summary.previous.as_deref().unwrap_or("unavailable"),
        ),
        key_value(
            "Last result",
            center.last_result.as_deref().unwrap_or("none"),
        ),
        Line::from(""),
    ]);
    for (label, current, latest) in [
        (
            "Core component",
            center.summary.core_current.as_deref(),
            center.summary.core_latest.as_deref(),
        ),
        (
            "CLI component",
            center.summary.cli_current.as_deref(),
            center.summary.cli_latest.as_deref(),
        ),
        (
            "Core Admin App",
            center.summary.core_app_current.as_deref(),
            center.summary.core_app_latest.as_deref(),
        ),
    ] {
        if current.is_some() || latest.is_some() {
            lines.push(key_value(
                label,
                &format!(
                    "{} → {}",
                    current.unwrap_or("unavailable"),
                    latest.unwrap_or("unavailable")
                ),
            ));
        }
    }
    if lines.last().is_some_and(|line| line.width() != 0) {
        lines.push(Line::from(""));
    }
    lines.extend(update_screen_lines(center));
    render_lines(frame, area, lines, app);
}

fn render_health(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let mut lines = vec![Line::styled("Health", Style::default().fg(Color::Cyan))];
    if let Some(status) = &app.status {
        lines.extend(
            status
                .services
                .iter()
                .map(|(name, state)| status_line(name, state)),
        );
        lines.push(status_line("Updater", &status.updater_enabled));
        lines.push(status_line(
            "Deployment verifier",
            if app
                .active
                .as_ref()
                .is_some_and(|child| matches!(child.operation, AppOperation::HealthVerification))
            {
                "running"
            } else {
                app.health_result
                    .as_deref()
                    .unwrap_or("not run in this session")
            },
        ));
    }
    lines.push(Line::from(""));
    for (index, action) in ["Refresh local checks", "Run full verification", "Back"]
        .iter()
        .enumerate()
    {
        lines.push(selection_line(index == app.selected, action));
    }
    append_progress(&mut lines, app);
    render_lines(frame, area, lines, app);
}

fn render_services(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let rows = app.services.iter().enumerate().map(|(index, service)| {
        Row::new(vec![
            service.unit.clone(),
            service.active.clone(),
            service.enabled.clone(),
            service.since.clone(),
        ])
        .style(row_style(index == app.selected, service.active == "active"))
    });
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(36),
            Constraint::Percentage(16),
            Constraint::Percentage(16),
            Constraint::Percentage(32),
        ],
    )
    .header(
        Row::new(["Jarvis unit", "Active", "Enabled", "Active since"])
            .style(Style::default().fg(Color::Cyan)),
    )
    .block(Block::default().title(" Services · visibility only "));
    frame.render_widget(table, area);
}

fn render_agents(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut JarvisApp) {
    let bundle = app
        .status
        .as_ref()
        .and_then(|status| status.agent_bundle.as_ref());
    let sections = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).split(area);
    let lines = vec![
        Line::styled("Agents", Style::default().fg(Color::Cyan)),
        key_value(
            "Active bundle",
            bundle.map_or("unavailable", |bundle| bundle.id.as_str()),
        ),
        key_value(
            "Agent count",
            &bundle.map_or(0, |bundle| bundle.agent_count).to_string(),
        ),
        key_value(
            "Updater",
            app.services
                .iter()
                .find(|service| service.label == "Agent updater")
                .map_or("unknown", |service| service.enabled.as_str()),
        ),
        Line::from(
            "Enter/→ expand · ← collapse/parent · e all · c none · x check · u update · b rollback",
        ),
    ];
    frame.render_widget(Paragraph::new(lines), sections[0]);

    let panes = if sections[1].width >= 68 {
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(sections[1])
            .to_vec()
    } else {
        vec![sections[1]]
    };
    let tree_area = panes[0];
    let tree_block = Block::default()
        .title(" Active bundle tree · manifest metadata only ")
        .borders(Borders::ALL);
    let tree_inner = tree_block.inner(tree_area);
    let rows = app.agent_rows();
    app.agent_page_height = tree_inner.height as usize;
    app.agent_selected = app.agent_selected.min(rows.len().saturating_sub(1));
    if app.agent_selected < app.agent_scroll {
        app.agent_scroll = app.agent_selected;
    } else if app.agent_page_height > 0
        && app.agent_selected >= app.agent_scroll.saturating_add(app.agent_page_height)
    {
        app.agent_scroll = app
            .agent_selected
            .saturating_add(1)
            .saturating_sub(app.agent_page_height);
    }
    app.agent_scroll = app.agent_scroll.min(rows.len().saturating_sub(1));
    let width = tree_inner.width as usize;
    let visible = rows
        .iter()
        .enumerate()
        .skip(app.agent_scroll)
        .take(app.agent_page_height)
        .map(|(index, row)| agent_tree_line(row, index == app.agent_selected, width))
        .collect::<Vec<_>>();
    frame.render_widget(tree_block, tree_area);
    frame.render_widget(
        Paragraph::new(if visible.is_empty() {
            vec![Line::styled(
                if app.loading.contains(&AppView::Agents) {
                    "Loading safe agent manifest metadata…"
                } else {
                    "No active agent metadata is available."
                },
                Style::default().fg(Color::DarkGray),
            )]
        } else {
            visible
        }),
        tree_inner,
    );

    if let Some(detail_area) = panes.get(1).copied() {
        let detail = rows.get(app.agent_selected);
        let detail_lines = match detail {
            Some(AgentTreeRow::Group { name, count, .. }) => vec![
                key_value("Group", name),
                key_value("Agents", &count.to_string()),
                Line::from(""),
                Line::from("No prompt or private file content is loaded."),
            ],
            Some(AgentTreeRow::Agent { group, agent }) => vec![
                key_value("Agent", &agent.name),
                key_value("ID", &agent.id),
                key_value("Group", group),
                key_value("Runtime state", "active"),
                key_value(
                    "Model policy",
                    agent.model_policy.as_deref().unwrap_or("not declared"),
                ),
                key_value(
                    "Bundle",
                    app.agent_tree
                        .as_ref()
                        .map_or("unavailable", |tree| tree.bundle_id.as_str()),
                ),
                Line::from(""),
                Line::from("Prompt/body: never loaded"),
            ],
            None => vec![Line::from("Select a safe manifest node.")],
        };
        frame.render_widget(
            Paragraph::new(detail_lines)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(
                    Block::default()
                        .title(" Safe details ")
                        .borders(Borders::ALL),
                ),
            detail_area,
        );
    }
}

fn agent_tree_line(row: &AgentTreeRow, selected: bool, width: usize) -> Line<'static> {
    let text = match row {
        AgentTreeRow::Group {
            name,
            expanded,
            count,
        } => format!("{} {name} ({count})", if *expanded { "▼" } else { "▶" }),
        AgentTreeRow::Agent { agent, .. } => format!(
            "  ├─ {}  active{}",
            agent.name,
            agent
                .model_policy
                .as_deref()
                .map_or_else(String::new, |policy| format!(" · {policy}"))
        ),
    };
    let available = width.saturating_sub(1);
    let fitted = if available == 0 {
        String::new()
    } else {
        split_at_display_width(&text, available).0
    };
    Line::styled(
        format!("{}{}", if selected { "›" } else { " " }, fitted),
        Style::default().fg(if selected { Color::Cyan } else { Color::White }),
    )
}

fn render_models(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let indices = app.visible_model_indices();
    let rows = indices
        .iter()
        .enumerate()
        .filter_map(|(visible_index, model_index)| {
            app.models.get(*model_index).map(|model| {
                Row::new(vec![
                    model.provider.clone(),
                    model.model.clone(),
                    if model.enabled { "yes" } else { "no" }.to_owned(),
                    model.source.clone(),
                ])
                .style(row_style(visible_index == app.selected, model.enabled))
            })
        });
    let title = format!(
        " Models · filter: {} · f filter · e enable · d disable · Enter details ",
        app.model_filter.as_deref().unwrap_or("all")
    );
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(23),
                Constraint::Percentage(37),
                Constraint::Length(9),
                Constraint::Percentage(40),
            ],
        )
        .header(
            Row::new(["Provider", "Model", "Enabled", "Source"])
                .style(Style::default().fg(Color::Cyan)),
        )
        .block(Block::default().title(title)),
        area,
    );
}

fn render_credentials(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let rows =
        app.credentials
            .iter()
            .enumerate()
            .map(|(index, credential)| {
                Row::new(vec![credential.provider.clone(), credential.status.clone()]).style(
                    row_style(index == app.selected, credential.status == "configured"),
                )
            });
    frame.render_widget(
        Table::new(
            rows,
            [Constraint::Percentage(42), Constraint::Percentage(58)],
        )
        .header(Row::new(["Provider", "Non-secret status"]).style(Style::default().fg(Color::Cyan)))
        .block(
            Block::default()
                .title(" Credentials · status only · secret values never enter TUI state "),
        ),
        area,
    );
}

fn safe_log_entry(line: &str) -> LogEntry {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(entry) = log_entry_from_json(&value, None, None) {
            return entry;
        }
        return LogEntry {
            timestamp: None,
            level: None,
            message: "[structured log omitted: no safe message field]".to_owned(),
            target: None,
        };
    }

    if let Some((timestamp, rest)) = split_journal_prefix(line) {
        let (source, message) = rest.split_once(": ").unwrap_or(("", rest));
        // `--no-hostname` is requested from journalctl. Taking only the final
        // source token also safely removes a hostname from older fixture or
        // compatibility output without displaying it on every row.
        let target = source
            .split_whitespace()
            .next_back()
            .and_then(|source| source.split('[').next())
            .filter(|source| !source.is_empty())
            .map(sanitize_log_text);
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
            if let Some(entry) =
                log_entry_from_json(&value, Some(timestamp.clone()), target.clone())
            {
                return entry;
            }
        }
        let system = target.as_deref() == Some("systemd");
        return LogEntry {
            timestamp: Some(timestamp),
            level: Some(if system { "SYSTEM" } else { "INFO" }.to_owned()),
            message: sanitize_log_text(message),
            target,
        };
    }

    LogEntry {
        timestamp: None,
        level: None,
        message: sanitize_log_text(line),
        target: None,
    }
}

fn log_entry_from_json(
    value: &serde_json::Value,
    fallback_timestamp: Option<String>,
    fallback_target: Option<String>,
) -> Option<LogEntry> {
    let object = value.as_object()?;
    let raw_message = object
        .get("message")
        .or_else(|| object.get("MESSAGE"))?
        .as_str()?;
    // Prefer journalctl's outer timestamp when present: it is already in the
    // Home Node's local timezone, while tracing JSON commonly uses UTC.
    let timestamp = fallback_timestamp.or_else(|| {
        object
            .get("timestamp")
            .or_else(|| object.get("time"))
            .or_else(|| object.get("@timestamp"))
            .and_then(serde_json::Value::as_str)
            .and_then(compact_timestamp)
    });
    let target = object
        .get("target")
        .or_else(|| object.get("SYSLOG_IDENTIFIER"))
        .or_else(|| object.get("_SYSTEMD_UNIT"))
        .and_then(serde_json::Value::as_str)
        .map(sanitize_log_text)
        .or(fallback_target);

    if let Ok(nested) = serde_json::from_str::<serde_json::Value>(raw_message) {
        if let Some(entry) = log_entry_from_json(&nested, timestamp.clone(), target.clone()) {
            return Some(entry);
        }
    }

    let system = target
        .as_deref()
        .is_some_and(|target| target == "systemd" || target.starts_with("systemd-"));
    let level = if system {
        "SYSTEM".to_owned()
    } else {
        object
            .get("level")
            .or_else(|| object.get("LEVEL"))
            .or_else(|| object.get("PRIORITY"))
            .and_then(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_u64().map(|v| v.to_string()))
            })
            .map(|level| compact_level(&level))
            .unwrap_or_else(|| "INFO".to_owned())
    };
    Some(LogEntry {
        timestamp,
        level: Some(level),
        message: sanitize_log_text(raw_message),
        target,
    })
}

fn compact_level(level: &str) -> String {
    match level.trim().to_ascii_uppercase().as_str() {
        "0" | "1" | "2" | "3" | "ERROR" | "ERR" | "CRITICAL" | "CRIT" => "ERROR".to_owned(),
        "4" | "WARN" | "WARNING" => "WARN".to_owned(),
        "7" | "TRACE" | "DEBUG" => "DEBUG".to_owned(),
        "SYSTEM" => "SYSTEM".to_owned(),
        _ => "INFO".to_owned(),
    }
}

fn compact_timestamp(timestamp: &str) -> Option<String> {
    let bytes = timestamp.as_bytes();
    (0..bytes.len().saturating_sub(7)).find_map(|start| {
        let candidate = bytes.get(start..start + 8)?;
        let valid = candidate[0].is_ascii_digit()
            && candidate[1].is_ascii_digit()
            && candidate[2] == b':'
            && candidate[3].is_ascii_digit()
            && candidate[4].is_ascii_digit()
            && candidate[5] == b':'
            && candidate[6].is_ascii_digit()
            && candidate[7].is_ascii_digit();
        valid.then(|| String::from_utf8_lossy(candidate).into_owned())
    })
}

fn split_journal_prefix(line: &str) -> Option<(String, &str)> {
    let (timestamp, rest) = line.split_once(' ')?;
    Some((compact_timestamp(timestamp)?, rest.trim_start()))
}

fn sanitize_log_text(text: &str) -> String {
    // Log entries are bounded independently from compact status/progress
    // strings so useful long messages can wrap instead of being cut at the
    // presentation helper's 240-character limit.
    let sanitized = text
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(4_096)
        .collect::<String>()
        .replace('\t', "    ");
    let compact = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = compact.to_ascii_lowercase();
    if [
        "api_key=",
        "api_key:",
        "\"api_key\"",
        "apikey=",
        "authorization:",
        "authorization=",
        "bearer ",
        "token=",
        "token:",
        "\"token\"",
        "access_token",
        "refresh_token",
        "password=",
        "password:",
        "\"password\"",
        "secret=",
        "secret:",
        "\"secret\"",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        "[potentially secret-bearing log content omitted]".to_owned()
    } else {
        compact
    }
}

fn wrapped_log_rows(logs: &VecDeque<LogEntry>, width: usize) -> Vec<String> {
    logs.iter()
        .flat_map(|entry| wrap_log_entry(entry, width.max(1)))
        .collect()
}

fn wrap_log_entry(entry: &LogEntry, width: usize) -> Vec<String> {
    let width = width.max(1);
    let header = match (&entry.timestamp, &entry.level) {
        (Some(timestamp), Some(level)) => format!("{timestamp} {level}"),
        (Some(timestamp), None) => timestamp.clone(),
        (None, Some(level)) => level.clone(),
        (None, None) => String::new(),
    };
    let body = log_message_with_target(entry);
    if header.is_empty() {
        return wrap_hanging("", &body, width);
    }
    let prefix = format!("{header}  ");
    if Line::raw(&prefix).width() < width {
        return wrap_hanging(&prefix, &body, width);
    }

    // A prefix that leaves no message column is clearer and safer as a
    // stacked header. Both halves still use the same hard-wrapping engine.
    let mut rows = wrap_hanging("", &header, width);
    rows.extend(wrap_hanging("", &body, width));
    rows
}

fn log_message_with_target(entry: &LogEntry) -> String {
    let Some(target) = entry.target.as_deref() else {
        return entry.message.clone();
    };
    if target == "systemd" || target.starts_with("systemd-") {
        return entry.message.clone();
    }
    let normalized_target = target
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    let normalized_message = entry
        .message
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if !normalized_target.is_empty() && normalized_message.contains(&normalized_target) {
        entry.message.clone()
    } else {
        format!("{target}: {}", entry.message)
    }
}

fn wrap_hanging(prefix: &str, message: &str, width: usize) -> Vec<String> {
    let prefix_width = Line::raw(prefix).width();
    let indent_width = prefix_width.min(width.saturating_sub(1));
    let indent = " ".repeat(indent_width);
    let mut remaining = message.trim().to_owned();
    if remaining.is_empty() {
        return vec![prefix.trim_end().to_owned()];
    }

    let mut rows = Vec::new();
    let mut first = true;
    while !remaining.is_empty() {
        let leader = if first { prefix } else { &indent };
        let leader_width = if first { prefix_width } else { indent_width };
        let available = width.saturating_sub(leader_width).max(1);
        let (piece, rest) = split_at_display_width(&remaining, available);
        rows.push(format!("{leader}{piece}"));
        remaining = rest;
        first = false;
    }
    rows
}

fn split_at_display_width(text: &str, width: usize) -> (String, String) {
    if Line::raw(text).width() <= width {
        return (text.to_owned(), String::new());
    }
    let mut used = 0;
    let mut hard_cut = 0;
    let mut whitespace = None;
    for (index, character) in text.char_indices() {
        let character_width = Line::raw(character.to_string()).width();
        if used + character_width > width {
            break;
        }
        used += character_width;
        hard_cut = index + character.len_utf8();
        if character.is_whitespace() {
            whitespace = Some((index, hard_cut));
        }
    }
    if hard_cut == 0 {
        let consumed = text.chars().next().map_or(0, char::len_utf8);
        return ("?".to_owned(), text[consumed..].to_owned());
    }
    let (cut, rest_start) = whitespace
        .filter(|(index, _)| *index > 0)
        .unwrap_or((hard_cut, hard_cut));
    (
        text[..cut].trim_end().to_owned(),
        text[rest_start..].trim_start().to_owned(),
    )
}

fn render_logs(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &mut JarvisApp) {
    let follow = app.log_following();
    let sections = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!(
                "Logs · {} · Tab target · f follow:{} · PgUp/PgDn/Home/End",
                LOG_TARGETS[app.log_target].unit(),
                if follow { "on" } else { "off" }
            ),
            Style::default().fg(Color::Cyan),
        )),
        sections[0],
    );

    let visual_rows = wrapped_log_rows(&app.logs, sections[1].width as usize);
    app.log_visual_rows = visual_rows.len();
    app.log_page_height = sections[1].height as usize;
    let max_scroll = app.log_max_scroll();
    if app.log_tail {
        app.log_scroll = max_scroll;
    } else {
        app.log_scroll = app.log_scroll.min(max_scroll);
    }
    let lines = visual_rows
        .into_iter()
        .skip(app.log_scroll)
        .take(app.log_page_height)
        .map(Line::from)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(if lines.is_empty() {
            vec![Line::styled(
                "No log messages available.",
                Style::default().fg(Color::DarkGray),
            )]
        } else {
            lines
        }),
        sections[1],
    );
}

fn render_system(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let lines = std::iter::once(Line::styled(
        "System / About",
        Style::default().fg(Color::Cyan),
    ))
    .chain(app.system.iter().map(|(key, value)| key_value(key, value)))
    .collect::<Vec<_>>();
    render_lines(frame, area, lines, app);
}

fn render_lines(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    mut lines: Vec<Line<'static>>,
    app: &JarvisApp,
) {
    if let Some(error) = &app.error {
        lines.push(Line::styled(
            format!("✗ {error}"),
            Style::default().fg(Color::Red),
        ));
    }
    if let Some(banner) = &app.banner {
        lines.push(Line::styled(
            banner.clone(),
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_footer(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &JarvisApp) {
    let text = if app.mutation_running() {
        "Trusted mutation running · cancellation disabled during transactional activation"
    } else if app.view == AppView::Agents {
        "↑↓/jk select · Enter/→ expand/details · ← parent · e/c expand/collapse · r refresh · Esc back"
    } else {
        "↑/↓ or j/k navigate · Enter select · Esc back · r refresh · ? help · q close"
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_modal(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, modal: &Modal) {
    frame.render_widget(ratatui::widgets::Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let lines = match modal {
        Modal::Help => vec![
            Line::styled("Jarvis keyboard help", Style::default().fg(Color::Cyan)),
            Line::from("↑/↓ or j/k  navigate"),
            Line::from("Enter        select/open"),
            Line::from("Esc/q        back or close at root"),
            Line::from("r            refresh current view"),
            Line::from("Tab          switch log target"),
            Line::from("Press Enter, Esc or q to close help"),
        ],
        Modal::Result {
            success,
            title,
            detail,
        } => vec![
            Line::styled(
                title.clone(),
                Style::default().fg(if *success { Color::Green } else { Color::Red }),
            ),
            Line::from(""),
            Line::from(detail.clone()),
            Line::from(""),
            Line::from("Press Enter, Esc or q to return"),
        ],
        Modal::ConfirmModelDisable {
            provider,
            model,
            selected,
        } => vec![
            Line::styled(
                format!("Disable {provider}/{model}?"),
                Style::default().fg(Color::Yellow),
            ),
            Line::from(""),
            selection_line(*selected == 0, "Cancel"),
            selection_line(*selected == 1, "Confirm disable"),
        ],
        Modal::ConfirmAgentRollback { selected } => vec![
            Line::styled(
                "Activate the previous verified private agent bundle?",
                Style::default().fg(Color::Yellow),
            ),
            Line::from(""),
            selection_line(*selected == 0, "Cancel"),
            selection_line(*selected == 1, "Confirm rollback"),
        ],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn append_progress(lines: &mut Vec<Line<'static>>, app: &JarvisApp) {
    if app.active.is_some() || app.fixture_completion.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!(
                "◐ {}",
                app.progress.back().map_or("Working…", String::as_str)
            ),
            Style::default().fg(Color::Cyan),
        ));
    }
}

fn render_empty(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, text: &str) {
    frame.render_widget(Paragraph::new(text.to_owned()), area);
}

fn selection_line(selected: bool, label: &str) -> Line<'static> {
    Line::styled(
        format!("{} {label}", if selected { "›" } else { " " }),
        Style::default().fg(if selected { Color::Cyan } else { Color::White }),
    )
}

fn key_value(key: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<20}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value.to_owned()),
    ])
}

fn status_line(key: &str, state: &str) -> Line<'static> {
    let healthy = matches!(state, "active" | "enabled" | "passed");
    Line::from(vec![
        Span::styled(format!("{key:<20}"), Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} {state}", if healthy { "✓" } else { "!" }),
            Style::default().fg(if healthy { Color::Green } else { Color::Yellow }),
        ),
    ])
}

fn row_style(selected: bool, healthy: bool) -> Style {
    Style::default().fg(if selected {
        Color::Cyan
    } else if healthy {
        Color::Green
    } else {
        Color::Yellow
    })
}

fn centered(area: ratatui::layout::Rect, width: u16, height: u16) -> ratatui::layout::Rect {
    let width = area.width.min(width);
    let height = area.height.min(height);
    let vertical = Layout::vertical([
        Constraint::Length((area.height - height) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Length((area.width - width) / 2),
        Constraint::Length(width),
        Constraint::Min(0),
    ])
    .split(vertical[1])[1]
}

fn operation_command(
    operation: &AppOperation,
    log_target: LogTarget,
) -> Result<(ProcessCommand, Option<File>)> {
    let mut command;
    let lock;
    match operation {
        AppOperation::HealthVerification => {
            command = trusted_command(Path::new(LIBEXEC).join("verify-home-node"));
            lock = None;
        }
        AppOperation::AgentCheck => {
            command = trusted_command(Path::new(LIBEXEC).join("private-agent-poll"));
            command.arg("--check");
            lock = None;
        }
        AppOperation::AgentUpdate => {
            command = trusted_command(Path::new(LIBEXEC).join("private-agent-poll"));
            lock = Some(mutation_lock("/run/jarvis-private-agent-update.lock")?);
        }
        AppOperation::ModelRefresh => {
            command = trusted_command(Path::new(SBIN).join("jarvis-models"));
            command.arg("refresh");
            lock = Some(mutation_lock(CONFIG_LOCK)?);
        }
        AppOperation::ModelEnable { provider, model } => {
            command = trusted_command(Path::new(SBIN).join("jarvis-models"));
            command.args(["enable", provider, model]);
            lock = Some(mutation_lock(CONFIG_LOCK)?);
        }
        AppOperation::ModelDisable { provider, model } => {
            command = trusted_command(Path::new(SBIN).join("jarvis-models"));
            command.args(["disable", provider, model]);
            lock = Some(mutation_lock(CONFIG_LOCK)?);
        }
        AppOperation::LogRefresh | AppOperation::LogFollow => {
            command = trusted_command("journalctl");
            command.args([
                "--no-pager",
                "--no-hostname",
                "--output=short-iso",
                "-u",
                log_target.unit(),
                "-n",
                "200",
            ]);
            if matches!(operation, AppOperation::LogFollow) {
                command.arg("-f");
            }
            lock = None;
        }
    }
    Ok((command, lock))
}

fn operation_detail(outcome: &AppOutcome) -> String {
    outcome
        .stderr
        .lines()
        .rev()
        .chain(outcome.stdout.lines().rev())
        .find(|line| !line.trim().is_empty())
        .map(sanitize_terminal_line)
        .unwrap_or_else(|| format!("trusted helper exited with {}", outcome.status))
}

fn system_information() -> Vec<(String, String)> {
    let release = active_release()
        .ok()
        .flatten()
        .unwrap_or_else(|| "unavailable".to_owned());
    let manifest = fs::read_to_string(Path::new(CURRENT_RELEASE).join("release.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let provenance = fs::read_to_string(Path::new(CURRENT_RELEASE).join("build-provenance.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    vec![
        ("Active release".to_owned(), release),
        ("Admin CLI".to_owned(), env!("CARGO_PKG_VERSION").to_owned()),
        ("Revision".to_owned(), json_string(&manifest, "revision")),
        ("Build OS".to_owned(), json_string(&provenance, "os")),
        ("Rust".to_owned(), json_string(&provenance, "rustc")),
        ("Target".to_owned(), json_string(&provenance, "target")),
        ("OS".to_owned(), os_pretty_name()),
        ("Kernel".to_owned(), command_text("uname", &["-r"])),
        ("Architecture".to_owned(), command_text("uname", &["-m"])),
        (
            "Hostname".to_owned(),
            read_trimmed("/proc/sys/kernel/hostname"),
        ),
        ("Uptime".to_owned(), uptime_text()),
        (
            "Updater timer".to_owned(),
            systemctl_value("is-enabled", "jarvis-updater.timer"),
        ),
    ]
}

#[cfg(any(feature = "tui-preview", test))]
fn fixture_credentials() -> Vec<CredentialView> {
    [
        ("anthropic", "configured"),
        ("openai", "configured"),
        ("deepseek", "not configured"),
        ("xai", "not configured"),
        ("zai", "configured"),
        ("ollama-cloud", "not configured"),
        ("ollama-local", "no credential required"),
    ]
    .into_iter()
    .map(|(provider, status)| CredentialView {
        provider: provider.to_owned(),
        status: status.to_owned(),
    })
    .collect()
}

#[cfg(any(feature = "tui-preview", test))]
fn fixture_agent_tree() -> AgentTreeSnapshot {
    let groups = ["Trading", "Development", "Personal", "Operations"];
    let mut agents = vec![
        ("ibkr", "IBKR", "Trading", "strong"),
        ("mt5", "MT5", "Trading", "standard"),
        ("crypto", "Crypto", "Trading", "strong"),
        ("polymarket", "Polymarket", "Trading", "research"),
        ("rust-dev", "Rust Development", "Development", "strong"),
        ("release-check", "Release Check", "Development", "standard"),
        ("home", "Home", "Personal", "standard"),
        ("calendar", "Calendar", "Personal", "fast"),
    ]
    .into_iter()
    .map(|(id, name, group, policy)| AgentTreeAgent {
        id: id.to_owned(),
        name: name.to_owned(),
        group: Some(group.to_owned()),
        model_policy: Some(policy.to_owned()),
    })
    .collect::<Vec<_>>();
    for index in 0..44 {
        let group = groups[index % groups.len()];
        agents.push(AgentTreeAgent {
            id: format!("fixture-agent-{index:02}"),
            name: format!("Fixture Agent {index:02} With Safe Long Display Metadata"),
            group: Some(group.to_owned()),
            model_policy: Some(
                if index % 3 == 0 {
                    "research"
                } else {
                    "standard"
                }
                .to_owned(),
            ),
        });
    }
    AgentTreeSnapshot {
        bundle_id: "fixture-bundle-2026-08-30".to_owned(),
        agents,
    }
}

#[cfg(any(feature = "tui-preview", test))]
fn fixture_services(failure: bool) -> Vec<ServiceView> {
    SERVICE_UNITS
        .iter()
        .map(|(label, unit)| ServiceView {
            label: (*label).to_owned(),
            unit: (*unit).to_owned(),
            active: if failure && *label == "OpenSandbox" {
                "failed"
            } else {
                "active"
            }
            .to_owned(),
            enabled: "enabled".to_owned(),
            since: "Sat 2026-08-30 08:00:00 CEST".to_owned(),
        })
        .collect()
}

#[cfg(any(feature = "tui-preview", test))]
fn fixture_logs() -> VecDeque<LogEntry> {
    let mut logs = VecDeque::from([
        safe_log_entry(
            r#"{"timestamp":"2026-08-30T12:08:11+02:00","level":"INFO","message":"starting jarvis-api","target":"jarvis_api","api_key":"fixture-must-never-render"}"#,
        ),
        safe_log_entry(
            r#"{"timestamp":"2026-08-30T12:08:12+02:00","level":"warn","message":"pricing registry unavailable; using conservative budget until pricing becomes available for every active provider and model route","target":"jarvis_usage"}"#,
        ),
        safe_log_entry(
            "2026-08-30T13:07:31+0200 jarvis-home-fixture systemd[1]: stopping jarvis-core.service",
        ),
        safe_log_entry(
            "2026-08-30T13:07:32+0200 jarvis-home-fixture systemd[1]: stopped jarvis-core.service",
        ),
        safe_log_entry(
            "fixture unknown line with a very long safely wrapped message that reaches the narrow SSH viewport and continues underneath its own message text without truncation across several additional visual terminal rows while preserving every bounded non-secret word during resize and scrolling so the final content remains visible through the final marker WRAPPED-LOG-END",
        ),
    ]);
    for line in 1..=115 {
        logs.push_back(safe_log_entry(&format!(
            "2026-08-30T13:08:{:02}+0200 jarvis-home-fixture jarvis-api[4242]: fixture bounded service event {line:03}",
            line % 60
        )));
    }
    logs
}

#[cfg(any(feature = "tui-preview", test))]
fn fixture_system() -> Vec<(String, String)> {
    [
        ("Active release", "v0.0.16"),
        ("Admin CLI", "fixture"),
        ("Revision", "0123456789abcdef0123456789abcdef01234567"),
        ("Build OS", "Ubuntu 26.04"),
        ("Rust", "rustc 1.97.1"),
        ("Target", "x86_64-unknown-linux-gnu"),
        ("OS", "Ubuntu fixture"),
        ("Kernel", "fixture-kernel"),
        ("Architecture", "x86_64"),
        ("Hostname", "jarvis-home-fixture"),
        ("Uptime", "12 days"),
        ("Updater timer", "enabled"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

fn json_string(value: &Option<serde_json::Value>, key: &str) -> String {
    value
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(serde_json::Value::as_str)
        .map(sanitize_terminal_line)
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn os_pretty_name() -> String {
    fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                .map(|value| value.trim_matches('"').to_owned())
        })
        .map(|value| sanitize_terminal_line(&value))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn command_text(program: &str, args: &[&str]) -> String {
    trusted_command(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| sanitize_terminal_line(text.trim()))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .ok()
        .map(|text| sanitize_terminal_line(text.trim()))
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn uptime_text() -> String {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map(|seconds| {
            format!(
                "{}d {:02}h",
                (seconds / 86_400.0) as u64,
                ((seconds as u64) / 3600) % 24
            )
        })
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn safe_error(error: &str) -> String {
    sanitize_terminal_line(error)
}

fn systemctl_property(unit: &str, property: &str) -> String {
    trusted_command("systemctl")
        .args(["show", "--property", property, "--value", unit])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| sanitize_terminal_line(value.trim()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn systemctl_value(action: &str, unit: &str) -> String {
    trusted_command("systemctl")
        .args([action, unit])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| sanitize_terminal_line(value.trim()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn view_index(view: AppView) -> usize {
    VIEWS
        .iter()
        .position(|candidate| *candidate == view)
        .unwrap_or(0)
}

fn wrap_up(value: usize, count: usize) -> usize {
    value.checked_sub(1).unwrap_or(count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn fixture_contains_every_view_without_secrets() {
        let app = JarvisApp::fixture(AppView::Overview, false);
        assert_eq!(VIEWS.len(), 9);
        assert!(app.credentials.iter().all(|credential| {
            !credential.status.contains("key") && !credential.status.contains("token")
        }));
        assert!(app.models.iter().all(|model| !model.model.contains('\n')));

        let degraded = JarvisApp::fixture(AppView::Overview, true);
        assert_eq!(
            degraded
                .status
                .as_ref()
                .and_then(|status| status.services.get("Core"))
                .map(String::as_str),
            Some("failed")
        );
    }

    #[test]
    fn model_disable_confirmation_defaults_to_cancel() {
        let mut app = JarvisApp::fixture(AppView::Models, false);
        app.handle_models(KeyCode::Char('d')).unwrap();
        assert!(matches!(
            app.modal,
            Some(Modal::ConfirmModelDisable { selected: 0, .. })
        ));
        assert!(app.active.is_none());
        assert!(app.fixture_completion.is_none());
    }

    #[test]
    fn agent_rollback_confirmation_defaults_to_cancel() {
        let mut app = JarvisApp::fixture(AppView::Agents, false);
        app.handle_agents(KeyCode::Char('b')).unwrap();
        assert!(matches!(
            app.modal,
            Some(Modal::ConfirmAgentRollback { selected: 0 })
        ));
        assert!(app.active.is_none());
        assert!(app.fixture_completion.is_none());
    }

    #[test]
    fn fixture_navigation_does_not_spawn_helpers() {
        let mut app = JarvisApp::fixture(AppView::Overview, false);
        app.navigation = view_index(AppView::Health);
        app.handle_overview(KeyCode::Enter);
        assert_eq!(app.view, AppView::Health);
        assert!(app.active.is_none());
    }

    #[test]
    fn log_and_progress_buffers_are_bounded_and_sanitized() {
        let mut lines = VecDeque::new();
        for index in 0..1_000 {
            push_bounded(
                &mut lines,
                sanitize_terminal_line(&format!("{index}\u{1b}[31m")),
                500,
            );
        }
        assert_eq!(lines.len(), 500);
        assert!(lines.iter().all(|line| !line.contains('\u{1b}')));
    }

    #[test]
    fn logs_parse_only_safe_json_and_compact_systemd_output() {
        let json = safe_log_entry(
            r#"{"timestamp":"2026-08-30T12:08:11+02:00","level":"warn","message":"pricing registry unavailable","target":"jarvis_usage","token":"must-not-render"}"#,
        );
        assert_eq!(json.timestamp.as_deref(), Some("12:08:11"));
        assert_eq!(json.level.as_deref(), Some("WARN"));
        assert_eq!(json.message, "pricing registry unavailable");
        assert_eq!(json.target.as_deref(), Some("jarvis_usage"));
        assert!(!format!("{json:?}").contains("must-not-render"));

        let journal_json = safe_log_entry(
            r#"2026-08-30T12:48:04+02:00 jarvis-api[3962]: {"timestamp":"2026-08-30T10:48:04.354642Z","level":"WARN","message":"no persona file; using built-in fallback persona","path":"/protected/private/path","target":"jarvis_api"}"#,
        );
        assert_eq!(journal_json.timestamp.as_deref(), Some("12:48:04"));
        assert_eq!(journal_json.level.as_deref(), Some("WARN"));
        assert!(!format!("{journal_json:?}").contains("/protected/private/path"));

        let system = safe_log_entry(
            "2026-08-30T13:07:31+0200 jarvis-home-fixture systemd[1]: stopping jarvis-core.service",
        );
        assert_eq!(system.timestamp.as_deref(), Some("13:07:31"));
        assert_eq!(system.level.as_deref(), Some("SYSTEM"));
        assert_eq!(system.target.as_deref(), Some("systemd"));
        let rendered = wrap_log_entry(&system, 80).join("\n");
        assert!(rendered.contains("13:07:31 SYSTEM  stopping jarvis-core.service"));
        assert!(!rendered.contains("jarvis-home-fixture"));

        let unsafe_plain = safe_log_entry("provider failed with api_key=must-not-render");
        assert_eq!(
            unsafe_plain.message,
            "[potentially secret-bearing log content omitted]"
        );
        assert!(!unsafe_plain.message.contains("must-not-render"));
    }

    #[test]
    fn logs_wrap_with_hanging_indent_and_reflow_without_truncation() {
        let long_message = format!(
            "pricing registry unavailable; {}WRAPPED-LOG-END",
            "using conservative budget until pricing becomes available ".repeat(8)
        );
        assert!(long_message.len() > 240);
        let entry = LogEntry {
            timestamp: Some("12:08:11".to_owned()),
            level: Some("WARN".to_owned()),
            message: sanitize_log_text(&long_message),
            target: Some("jarvis_usage".to_owned()),
        };
        let wide = wrap_log_entry(&entry, 80);
        let narrow = wrap_log_entry(&entry, 42);
        assert!(narrow.len() > wide.len());
        assert!(narrow.iter().all(|row| Line::raw(row).width() <= 42));
        let prefix = "12:08:11 WARN  ";
        assert!(narrow[0].starts_with(prefix));
        assert!(narrow
            .iter()
            .skip(1)
            .all(|row| row.starts_with(&" ".repeat(Line::raw(prefix).width()))));
        assert!(narrow.join(" ").contains("WRAPPED-LOG-END"));

        for width in [50, 80, 140] {
            let rows = wrap_log_entry(&entry, width);
            assert!(rows.iter().all(|row| Line::raw(row).width() <= width));
        }
        let stacked = wrap_log_entry(&entry, 12);
        assert!(stacked.iter().all(|row| Line::raw(row).width() <= 12));
        assert_eq!(stacked[0], "12:08:11");
        assert_eq!(stacked[1], "WARN");

        let one_column = wrap_log_entry(
            &LogEntry {
                timestamp: None,
                level: None,
                message: "界x".to_owned(),
                target: None,
            },
            1,
        );
        assert!(one_column.iter().all(|row| Line::raw(row).width() <= 1));
    }

    #[test]
    fn logs_hard_wrap_a_five_hundred_character_unbroken_token() {
        let token = format!("https://fixture.invalid/{}", "a".repeat(540));
        let rows = wrap_hanging("", &token, 50);
        assert!(rows.len() > 10);
        assert!(rows.iter().all(|row| Line::raw(row).width() <= 50));
        assert_eq!(rows.concat(), token);
        for width in [50, 80, 160] {
            assert!(wrap_hanging("", &token, width)
                .iter()
                .all(|row| Line::raw(row).width() <= width));
        }
    }

    #[test]
    fn log_scroll_uses_wrapped_visual_rows_and_survives_reflow() {
        let mut app = JarvisApp::fixture(AppView::Logs, false);
        let mut wide = Terminal::new(TestBackend::new(100, 16)).unwrap();
        wide.draw(|frame| render(frame, &mut app)).unwrap();
        let wide_rows = app.log_visual_rows;
        assert_eq!(wide_rows, wrapped_log_rows(&app.logs, 98).len());
        assert!(wide_rows >= app.logs.len());
        assert!(app.log_page_height > 0);

        app.handle_logs(KeyCode::End).unwrap();
        assert!(app.log_tail);
        assert_eq!(app.log_scroll, app.log_max_scroll());
        app.handle_logs(KeyCode::Up).unwrap();
        assert!(!app.log_tail);
        let scrolled = app.log_scroll;

        let mut narrow = Terminal::new(TestBackend::new(54, 16)).unwrap();
        narrow.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.log_visual_rows, wrapped_log_rows(&app.logs, 52).len());
        assert!(app.log_visual_rows > wide_rows);
        assert_eq!(app.log_scroll, scrolled.min(app.log_max_scroll()));

        app.handle_logs(KeyCode::Home).unwrap();
        assert_eq!(app.log_scroll, 0);
        app.handle_logs(KeyCode::PageDown).unwrap();
        assert!(app.log_scroll > 0);
    }

    #[test]
    fn fixture_log_refresh_and_follow_remain_rootless_and_bounded() {
        let mut app = JarvisApp::fixture(AppView::Logs, false);
        let count = app.logs.len();
        app.handle_logs(KeyCode::Char('f')).unwrap();
        assert!(app.log_following());
        assert!(app.active.is_none());
        app.refresh_current().unwrap();
        assert!(!app.log_following());
        assert_eq!(app.logs.len(), count);
        app.handle_logs(KeyCode::Char('f')).unwrap();
        app.handle_logs(KeyCode::Char('f')).unwrap();
        assert!(!app.log_following());
        app.refresh_current().unwrap();
        assert_eq!(app.logs.len(), count);
        assert!(app.active.is_none());
    }

    #[test]
    fn agents_tree_expands_collapses_navigates_and_preserves_refresh_state() {
        let mut app = JarvisApp::fixture(AppView::Agents, false);
        assert!(app.agent_expanded.contains("Trading"));
        let expanded_rows = app.agent_rows().len();
        app.handle_agents(KeyCode::Char('c')).unwrap();
        let collapsed_rows = app.agent_rows().len();
        assert!(collapsed_rows < expanded_rows);
        assert_eq!(collapsed_rows, app.agent_groups().len());

        app.agent_selected = app
            .agent_rows()
            .iter()
            .position(|row| matches!(row, AgentTreeRow::Group { name, .. } if name == "Trading"))
            .unwrap();
        app.handle_agents(KeyCode::Right).unwrap();
        assert!(app.agent_expanded.contains("Trading"));
        app.handle_agents(KeyCode::Down).unwrap();
        assert!(matches!(
            app.agent_rows().get(app.agent_selected),
            Some(AgentTreeRow::Agent { group, .. }) if group == "Trading"
        ));
        app.handle_agents(KeyCode::Left).unwrap();
        assert!(matches!(
            app.agent_rows().get(app.agent_selected),
            Some(AgentTreeRow::Group { name, .. }) if name == "Trading"
        ));

        let refreshed = app.agent_tree.clone();
        app.replace_agent_tree(refreshed);
        assert!(app.agent_expanded.contains("Trading"));
    }

    #[test]
    fn agents_tree_scrolls_large_fixture_and_renders_narrow_without_private_content() {
        let mut app = JarvisApp::fixture(AppView::Agents, false);
        app.handle_agents(KeyCode::Char('e')).unwrap();
        let rows = app.agent_rows();
        assert!(rows.len() > 50);
        app.agent_selected = rows.len() - 1;
        let mut terminal = Terminal::new(TestBackend::new(50, 16)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(app.agent_scroll > 0);
        assert!(app.agent_page_height > 0);
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("active"));
        assert!(!rendered.contains("instructions"));
        assert!(!rendered.contains("Jarvis.md"));
        assert!(!rendered.contains("fixture-secret"));
    }

    #[test]
    fn fixture_shell_renders_every_view_at_narrow_width_without_secret_material() {
        let mut app = JarvisApp::fixture(AppView::Overview, false);
        let backend = TestBackend::new(48, 20);
        let mut terminal = Terminal::new(backend).unwrap();

        for view in VIEWS {
            app.view = view;
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let marker = if view == AppView::Logs {
                LOG_TARGETS[0].unit()
            } else {
                view.title()
            };
            assert!(rendered.contains(marker), "missing {marker}");
            assert!(!rendered.contains("sk-"));
            assert!(!rendered.contains("fixture-secret"));
            assert!(!rendered.contains("fixture-must-never-render"));
        }
    }

    #[test]
    fn service_view_is_limited_to_the_fixed_jarvis_allowlist() {
        let app = JarvisApp::fixture(AppView::Services, false);
        let units = app
            .services
            .iter()
            .map(|service| service.unit.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            units,
            SERVICE_UNITS
                .iter()
                .map(|(_, unit)| *unit)
                .collect::<Vec<_>>()
        );
    }
}
