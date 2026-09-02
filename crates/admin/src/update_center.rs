//! Persistent Update Center state machine and trusted updater child lifecycle.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum UpdateInvocation {
    Center,
    Check,
    Status,
    Latest,
    Version(String),
    Rollback,
}

impl UpdateInvocation {
    pub(super) fn from_args(args: &UpdateArgs) -> Self {
        if args.check {
            Self::Check
        } else if args.status {
            Self::Status
        } else if args.latest {
            Self::Latest
        } else if let Some(version) = &args.version {
            Self::Version(version.clone())
        } else if args.rollback {
            Self::Rollback
        } else {
            Self::Center
        }
    }
}

pub(super) fn trusted_updater_command() -> Result<ProcessCommand> {
    let mut command = trusted_command(Path::new(LIBEXEC).join("update-core-release"));
    load_updater_environment(&mut command)?;
    Ok(command)
}

#[derive(Clone, Debug, Default)]
pub(super) struct UpdateSummary {
    pub(super) current: Option<String>,
    pub(super) latest: Option<String>,
    pub(super) previous: Option<String>,
    pub(super) updater: Option<String>,
    pub(super) update_available: Option<bool>,
    pub(super) core_current: Option<String>,
    pub(super) core_latest: Option<String>,
    pub(super) cli_current: Option<String>,
    pub(super) cli_latest: Option<String>,
    pub(super) core_app_current: Option<String>,
    pub(super) core_app_latest: Option<String>,
}

impl UpdateSummary {
    pub(super) fn merge_helper_output(&mut self, output: &str) -> Result<()> {
        let values = parse_key_value_output(output)?;
        for (key, destination) in [
            ("current", &mut self.current),
            ("latest", &mut self.latest),
            ("previous", &mut self.previous),
            ("updater", &mut self.updater),
            ("core_current", &mut self.core_current),
            ("core_latest", &mut self.core_latest),
            ("cli_current", &mut self.cli_current),
            ("cli_latest", &mut self.cli_latest),
            ("core_app_current", &mut self.core_app_current),
            ("core_app_latest", &mut self.core_app_latest),
        ] {
            if let Some(value) = values.get(key) {
                *destination = (value != "unavailable").then(|| value.clone());
            }
        }
        self.update_available = values
            .get("update")
            .map(|value| value == "available")
            .or_else(|| match (&self.latest, &self.current) {
                (Some(latest), Some(current)) => Some(release_is_newer(latest, current)),
                _ => None,
            });
        Ok(())
    }
}

pub(super) fn release_is_newer(candidate: &str, current: &str) -> bool {
    fn components(tag: &str) -> Option<[u64; 3]> {
        let mut parts = tag.strip_prefix('v')?.split('.');
        let value = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        parts.next().is_none().then_some(value)
    }
    matches!((components(candidate), components(current)), (Some(candidate), Some(current)) if candidate > current)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RollbackCandidate {
    pub(super) version: String,
    pub(super) current: bool,
    pub(super) verified: bool,
    pub(super) rollback_capable: bool,
    pub(super) reason: String,
}

#[derive(Clone, Debug)]
pub(super) enum UpdateOperation {
    Status,
    Check,
    Latest,
    Version(String),
    Candidates,
    Rollback(String),
}

impl UpdateOperation {
    pub(super) fn configure(&self, command: &mut ProcessCommand) {
        match self {
            Self::Status => {
                command.arg("--status");
            }
            Self::Check => {
                command.arg("--check");
            }
            Self::Latest => {
                command.arg("--latest");
            }
            Self::Version(version) => {
                command.args(["--version", version]);
            }
            Self::Candidates => {
                command.arg("--rollback-candidates");
            }
            Self::Rollback(version) => {
                command.args(["--rollback-version", version]);
            }
        }
    }

    pub(super) fn is_mutating(&self) -> bool {
        matches!(self, Self::Latest | Self::Version(_) | Self::Rollback(_))
    }

    pub(super) fn title(&self) -> String {
        match self {
            Self::Status => "Refreshing Update Center".to_owned(),
            Self::Check => "Checking for updates".to_owned(),
            Self::Latest => "Updating to latest stable release".to_owned(),
            Self::Version(version) => format!("Installing {version}"),
            Self::Candidates => "Loading rollback candidates".to_owned(),
            Self::Rollback(version) => format!("Rolling back to {version}"),
        }
    }
}

pub(super) enum ChildStream {
    Stdout(String),
    Stderr(String),
}

pub(super) struct UpdateChild {
    child: Child,
    pending: Arc<Mutex<VecDeque<ChildStream>>>,
    readers: Vec<thread::JoinHandle<()>>,
    operation: UpdateOperation,
    stdout: VecDeque<String>,
    stderr: VecDeque<String>,
}

pub(super) struct ChildOutcome {
    pub(super) operation: UpdateOperation,
    pub(super) status: ExitStatus,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

impl UpdateChild {
    pub(super) fn spawn(operation: UpdateOperation) -> Result<Self> {
        let mut command = trusted_updater_command()?;
        operation.configure(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start trusted updater")?;
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
        })
    }

    pub(super) fn drain(&mut self, messages: &mut VecDeque<String>) {
        let drained = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending.drain(..).collect::<Vec<_>>()
        };
        for message in drained {
            let (line, destination) = match message {
                ChildStream::Stdout(line) => (line, &mut self.stdout),
                ChildStream::Stderr(line) => (line, &mut self.stderr),
            };
            push_bounded(destination, line.clone(), 256);
            push_bounded(messages, sanitize_terminal_line(&line), 18);
        }
    }

    pub(super) fn try_status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub(super) fn finish(
        mut self,
        status: ExitStatus,
        messages: &mut VecDeque<String>,
    ) -> ChildOutcome {
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        self.drain(messages);
        ChildOutcome {
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

    pub(super) fn terminate(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

impl Drop for UpdateChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

pub(super) fn forward_child_lines<R: Read + Send + 'static>(
    stream: R,
    pending: Arc<Mutex<VecDeque<ChildStream>>>,
    stderr: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in io::BufReader::new(stream).lines().map_while(Result::ok) {
            let message = if stderr {
                ChildStream::Stderr(line)
            } else {
                ChildStream::Stdout(line)
            };
            let mut pending = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.len() == 128 {
                pending.pop_front();
            }
            pending.push_back(message);
        }
    })
}

pub(super) fn push_bounded<T>(lines: &mut VecDeque<T>, line: T, capacity: usize) {
    if lines.len() == capacity {
        lines.pop_front();
    }
    lines.push_back(line);
}

pub(super) fn sanitize_terminal_line(line: &str) -> String {
    line.chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(240)
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UpdateScreen {
    Overview,
    Running,
    VersionInput,
    RollbackSelection,
    RollbackConfirm,
    Result,
}

#[derive(Clone, Debug)]
pub(super) struct UpdateResultState {
    success: bool,
    title: String,
    detail: String,
}

#[derive(Clone, Copy)]
pub(super) struct FixtureUpdateMode {
    pub(super) fail_mutations: bool,
}

pub(super) struct UpdateCenter {
    pub(super) summary: UpdateSummary,
    pub(super) screen: UpdateScreen,
    pub(super) selected: usize,
    pub(super) input: String,
    pub(super) input_error: Option<String>,
    pub(super) candidates: Vec<RollbackCandidate>,
    pub(super) confirmation: Option<RollbackCandidate>,
    pub(super) result: Option<UpdateResultState>,
    pub(super) messages: VecDeque<String>,
    pub(super) last_result: Option<String>,
    pub(super) active: Option<UpdateChild>,
    pub(super) operation: Option<UpdateOperation>,
    pub(super) fixture: Option<FixtureUpdateMode>,
    pub(super) fixture_completion: Option<Instant>,
    pub(super) animation_tick: usize,
    pub(super) client_replacement: Option<String>,
}

impl UpdateCenter {
    pub(super) fn live() -> Result<Self> {
        let mut center = Self::base(None);
        center.start(UpdateOperation::Status)?;
        Ok(center)
    }

    #[cfg(any(feature = "tui-preview", test))]
    pub(super) fn fixture(fail_mutations: bool) -> Self {
        let mut center = Self::base(Some(FixtureUpdateMode { fail_mutations }));
        center.summary = UpdateSummary {
            current: Some("v0.0.15".to_owned()),
            latest: Some("v0.0.16".to_owned()),
            previous: Some("v0.0.14".to_owned()),
            updater: Some("enabled".to_owned()),
            update_available: Some(true),
            core_current: Some("0.1.0".to_owned()),
            core_latest: Some("0.1.0".to_owned()),
            cli_current: Some("0.1.0".to_owned()),
            cli_latest: Some("0.1.0".to_owned()),
            core_app_current: Some("0.1.0".to_owned()),
            core_app_latest: Some("0.2.0".to_owned()),
        };
        center.last_result = Some("Fixture data only · no administrative capability".to_owned());
        center
    }

    pub(super) fn base(fixture: Option<FixtureUpdateMode>) -> Self {
        Self {
            summary: UpdateSummary::default(),
            screen: UpdateScreen::Overview,
            selected: 0,
            input: String::new(),
            input_error: None,
            candidates: Vec::new(),
            confirmation: None,
            result: None,
            messages: VecDeque::new(),
            last_result: None,
            active: None,
            operation: None,
            fixture,
            fixture_completion: None,
            animation_tick: 0,
            client_replacement: None,
        }
    }

    pub(super) fn start(&mut self, operation: UpdateOperation) -> Result<()> {
        self.messages.clear();
        push_bounded(&mut self.messages, operation.title(), 18);
        self.selected = 0;
        self.screen = UpdateScreen::Running;
        self.operation = Some(operation.clone());
        if self.fixture.is_some() {
            self.fixture_completion = Some(Instant::now() + Duration::from_millis(300));
        } else {
            self.active = Some(UpdateChild::spawn(operation)?);
        }
        Ok(())
    }

    pub(super) fn tick(&mut self) -> Result<()> {
        self.animation_tick = self.animation_tick.wrapping_add(1);
        if self
            .fixture_completion
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.fixture_completion = None;
            let operation = self
                .operation
                .take()
                .context("fixture operation is missing")?;
            self.complete_fixture(operation);
            return Ok(());
        }

        let status = if let Some(active) = self.active.as_mut() {
            active.drain(&mut self.messages);
            active.try_status().context("poll trusted updater")?
        } else {
            None
        };
        if let Some(status) = status {
            let active = self.active.take().context("completed updater is missing")?;
            let outcome = active.finish(status, &mut self.messages);
            self.operation = None;
            if let Err(error) = self.complete(outcome) {
                self.show_result(
                    false,
                    "Update operation failed".to_owned(),
                    sanitize_terminal_line(&format!(
                        "Trusted helper response could not be validated: {error}"
                    )),
                );
            }
        }
        Ok(())
    }

    pub(super) fn complete(&mut self, outcome: ChildOutcome) -> Result<()> {
        let check_success =
            matches!(outcome.operation, UpdateOperation::Check) && outcome.status.code() == Some(2);
        if !outcome.status.success() && !check_success {
            let rolled_back = outcome.stderr.contains("rollback completed")
                || outcome.stderr.contains("restored")
                || outcome.stdout.contains("rolled back");
            let detail = outcome
                .stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .or_else(|| {
                    outcome
                        .stdout
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                })
                .map(sanitize_terminal_line)
                .unwrap_or_else(|| format!("trusted updater exited with {}", outcome.status));
            self.show_result(
                false,
                if rolled_back {
                    "Update failed · previous release restored".to_owned()
                } else {
                    "Update operation failed".to_owned()
                },
                detail,
            );
            return Ok(());
        }
        match outcome.operation {
            UpdateOperation::Status => {
                self.summary.merge_helper_output(&outcome.stdout)?;
                self.last_result = Some("Update status refreshed".to_owned());
                self.screen = UpdateScreen::Overview;
            }
            UpdateOperation::Check => {
                self.summary.merge_helper_output(&outcome.stdout)?;
                self.last_result = Some(match self.summary.update_available {
                    Some(true) => "Update available".to_owned(),
                    Some(false) => "Already up to date".to_owned(),
                    None => "Update check completed".to_owned(),
                });
                self.screen = UpdateScreen::Overview;
            }
            UpdateOperation::Candidates => {
                self.candidates = serde_json::from_str(&outcome.stdout)
                    .context("trusted updater returned invalid rollback candidate JSON")?;
                self.selected = 0;
                self.screen = UpdateScreen::RollbackSelection;
            }
            UpdateOperation::Latest => {
                let target = self
                    .summary
                    .latest
                    .as_deref()
                    .unwrap_or("latest stable release");
                let target = target.to_owned();
                self.summary.current = Some(target.clone());
                self.summary.update_available = Some(false);
                let title = format!("Updated successfully to {target}");
                self.show_result(
                    true,
                    title.clone(),
                    "The trusted updater completed activation and Core readiness checks."
                        .to_owned(),
                );
                self.client_replacement = Some(title);
            }
            UpdateOperation::Version(version) => {
                self.summary.current = Some(version.clone());
                self.summary.update_available = self
                    .summary
                    .latest
                    .as_deref()
                    .map(|latest| release_is_newer(latest, &version));
                let title = format!("Updated successfully to {version}");
                self.show_result(
                    true,
                    title.clone(),
                    "The trusted updater completed activation and Core readiness checks."
                        .to_owned(),
                );
                self.client_replacement = Some(title);
            }
            UpdateOperation::Rollback(version) => {
                self.summary.previous = self.summary.current.replace(version.clone());
                self.summary.update_available = self
                    .summary
                    .latest
                    .as_deref()
                    .map(|latest| release_is_newer(latest, &version));
                let title = format!("Rolled back successfully to {version}");
                self.show_result(
                    true,
                    title.clone(),
                    "The trusted updater completed activation and Core readiness checks."
                        .to_owned(),
                );
                self.client_replacement = Some(title);
            }
        }
        Ok(())
    }

    pub(super) fn complete_fixture(&mut self, operation: UpdateOperation) {
        let fail_mutations = self.fixture.is_some_and(|fixture| fixture.fail_mutations);
        match operation {
            UpdateOperation::Status => {
                self.last_result = Some("Fixture status refreshed".to_owned());
                self.screen = UpdateScreen::Overview;
            }
            UpdateOperation::Check => {
                self.last_result =
                    Some("Fixture-check-complete · update available: v0.0.16".to_owned());
                self.screen = UpdateScreen::Overview;
            }
            UpdateOperation::Candidates => {
                self.candidates = vec![
                    RollbackCandidate {
                        version: "v0.0.15".to_owned(),
                        current: true,
                        verified: true,
                        rollback_capable: false,
                        reason: "active release".to_owned(),
                    },
                    RollbackCandidate {
                        version: "v0.0.14".to_owned(),
                        current: false,
                        verified: true,
                        rollback_capable: true,
                        reason: "eligible".to_owned(),
                    },
                    RollbackCandidate {
                        version: "v0.0.10-legacy".to_owned(),
                        current: false,
                        verified: false,
                        rollback_capable: false,
                        reason: "verification marker is missing or invalid".to_owned(),
                    },
                ];
                self.screen = UpdateScreen::RollbackSelection;
            }
            operation @ (UpdateOperation::Latest
            | UpdateOperation::Version(_)
            | UpdateOperation::Rollback(_)) => {
                let target = match &operation {
                    UpdateOperation::Latest => "v0.0.16".to_owned(),
                    UpdateOperation::Version(version) | UpdateOperation::Rollback(version) => {
                        version.clone()
                    }
                    _ => unreachable!(),
                };
                if fail_mutations {
                    self.show_result(
                        false,
                        "Update failed".to_owned(),
                        "Fixture readiness failure; previous release restored".to_owned(),
                    );
                } else {
                    self.show_result(
                        true,
                        format!("Updated successfully to {target}"),
                        "Fixture completion is persistent until owner dismissal".to_owned(),
                    );
                }
            }
        }
        self.operation = None;
    }

    pub(super) fn show_result(&mut self, success: bool, title: String, detail: String) {
        self.last_result = Some(title.clone());
        self.result = Some(UpdateResultState {
            success,
            title,
            detail,
        });
        self.selected = 0;
        self.screen = UpdateScreen::Result;
    }

    pub(super) fn running_mutation(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(UpdateOperation::is_mutating)
    }

    pub(super) fn take_client_replacement(&mut self) -> Option<String> {
        self.client_replacement.take()
    }

    pub(super) fn terminate_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.terminate();
        }
        self.fixture_completion = None;
        self.operation = None;
    }

    pub(super) fn handle_event(&mut self, event: Event) -> Result<Option<TuiExitReason>> {
        let Event::Key(key) = event else {
            return Ok(None);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(None);
        }
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('q') || ctrl_c {
            if self.running_mutation() {
                self.last_result = Some(
                    "Trusted mutation is in a non-cancellable phase; wait for its result"
                        .to_owned(),
                );
                return Ok(None);
            }
            self.terminate_active();
            return Ok(Some(if ctrl_c {
                TuiExitReason::CtrlC
            } else {
                TuiExitReason::Quit
            }));
        }
        if key.code == KeyCode::Esc {
            if self.running_mutation() {
                self.last_result = Some(
                    "Trusted mutation is in a non-cancellable phase; wait for its result"
                        .to_owned(),
                );
            } else if self.screen == UpdateScreen::Overview {
                return Ok(Some(TuiExitReason::Escape));
            } else {
                self.terminate_active();
                self.screen = UpdateScreen::Overview;
                self.selected = 0;
            }
            return Ok(None);
        }

        if self.screen == UpdateScreen::VersionInput {
            match key.code {
                KeyCode::Backspace => {
                    self.input.pop();
                    self.input_error = None;
                }
                KeyCode::Char(character)
                    if self.input.len() < 32
                        && (character.is_ascii_digit() || matches!(character, 'v' | '.')) =>
                {
                    self.input.push(character);
                    self.input_error = None;
                }
                KeyCode::Enter => {
                    if valid_release_tag(&self.input) {
                        self.start(UpdateOperation::Version(self.input.clone()))?;
                    } else {
                        self.input_error = Some("Use vMAJOR.MINOR.PATCH".to_owned());
                    }
                }
                _ => {}
            }
            return Ok(None);
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => return self.activate_selection(),
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn selection_count(&self) -> usize {
        match self.screen {
            UpdateScreen::Overview => 6,
            UpdateScreen::RollbackSelection => self.candidates.len().max(1),
            UpdateScreen::RollbackConfirm | UpdateScreen::Result => 2,
            UpdateScreen::Running | UpdateScreen::VersionInput => 1,
        }
    }

    pub(super) fn move_selection(&mut self, direction: isize) {
        let count = self.selection_count();
        if count <= 1 {
            return;
        }
        self.selected = if direction < 0 {
            self.selected.checked_sub(1).unwrap_or(count - 1)
        } else {
            (self.selected + 1) % count
        };
    }

    pub(super) fn activate_selection(&mut self) -> Result<Option<TuiExitReason>> {
        match self.screen {
            UpdateScreen::Overview => match self.selected {
                0 => self.start(UpdateOperation::Check)?,
                1 => self.start(UpdateOperation::Latest)?,
                2 => {
                    self.input.clear();
                    self.input_error = None;
                    self.screen = UpdateScreen::VersionInput;
                }
                3 => self.start(UpdateOperation::Candidates)?,
                4 => self.start(UpdateOperation::Status)?,
                5 => return Ok(Some(TuiExitReason::SelectedClose)),
                _ => unreachable!(),
            },
            UpdateScreen::RollbackSelection => {
                if let Some(candidate) = self.candidates.get(self.selected).cloned() {
                    if candidate.rollback_capable {
                        self.confirmation = Some(candidate);
                        self.selected = 0;
                        self.screen = UpdateScreen::RollbackConfirm;
                    } else {
                        self.last_result = Some(format!(
                            "{} is unavailable: {}",
                            candidate.version, candidate.reason
                        ));
                    }
                }
            }
            UpdateScreen::RollbackConfirm => {
                if self.selected == 0 {
                    self.screen = UpdateScreen::RollbackSelection;
                } else if let Some(candidate) = self.confirmation.clone() {
                    self.start(UpdateOperation::Rollback(candidate.version))?;
                }
            }
            UpdateScreen::Result => {
                if self.selected == 0 {
                    self.screen = UpdateScreen::Overview;
                    self.selected = 0;
                } else {
                    return Ok(Some(TuiExitReason::SelectedClose));
                }
            }
            UpdateScreen::Running | UpdateScreen::VersionInput => {}
        }
        Ok(None)
    }
}

pub(super) fn update_screen_lines(center: &UpdateCenter) -> Vec<Line<'static>> {
    let selected_line = |index: usize, label: String| {
        if center.selected == index {
            Line::styled(format!("> {label}"), Style::default().fg(Color::Cyan))
        } else {
            Line::from(format!("  {label}"))
        }
    };
    match center.screen {
        UpdateScreen::Overview => [
            "Check for updates",
            "Update to latest",
            "Install specific version",
            "Rollback",
            "Refresh",
            "Close",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, label)| selected_line(index, label.to_owned()))
        .collect(),
        UpdateScreen::Running => {
            let spinner = ["◐", "◓", "◑", "◒"];
            let mut lines = vec![Line::styled(
                format!(
                    "{} {}",
                    spinner[center.animation_tick % spinner.len()],
                    center
                        .operation
                        .as_ref()
                        .map(UpdateOperation::title)
                        .unwrap_or_else(|| "Working".to_owned())
                ),
                Style::default().fg(Color::Cyan),
            )];
            lines.extend(center.messages.iter().cloned().map(Line::from));
            lines
        }
        UpdateScreen::VersionInput => {
            let mut lines = vec![
                Line::styled("Install specific version", Style::default().fg(Color::Cyan)),
                Line::from(format!("> {}", center.input)),
                Line::from("Type vMAJOR.MINOR.PATCH and press Enter"),
            ];
            if let Some(error) = &center.input_error {
                lines.push(Line::styled(error.clone(), Style::default().fg(Color::Red)));
            }
            lines
        }
        UpdateScreen::RollbackSelection => {
            let mut lines = vec![Line::styled(
                "Rollback candidates · version | current | verified | rollback | reason",
                Style::default().fg(Color::Cyan),
            )];
            if center.candidates.is_empty() {
                lines.push(Line::from("No managed release candidates found"));
            } else {
                let start = center.selected.saturating_sub(4);
                let end = (start + 8).min(center.candidates.len());
                lines.extend(
                    center
                        .candidates
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(end - start)
                        .map(|(index, candidate)| {
                            selected_line(
                                index,
                                format!(
                                    "{} | {} | {} | {} | {}",
                                    candidate.version,
                                    candidate.current,
                                    candidate.verified,
                                    candidate.rollback_capable,
                                    candidate.reason
                                ),
                            )
                        }),
                );
            }
            lines
        }
        UpdateScreen::RollbackConfirm => {
            let candidate = center
                .confirmation
                .as_ref()
                .map(|candidate| candidate.version.as_str())
                .unwrap_or("unavailable");
            vec![
                Line::styled(
                    format!("Confirm rollback to {candidate}?"),
                    Style::default().fg(Color::Yellow),
                ),
                selected_line(0, "Cancel".to_owned()),
                selected_line(1, format!("Rollback to {candidate}")),
            ]
        }
        UpdateScreen::Result => {
            let result = center.result.as_ref();
            vec![
                Line::styled(
                    result
                        .map_or("Operation complete", |result| result.title.as_str())
                        .to_owned(),
                    Style::default().fg(if result.is_some_and(|result| result.success) {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Line::from(
                    result
                        .map_or("", |result| result.detail.as_str())
                        .to_owned(),
                ),
                Line::from(""),
                selected_line(0, "Back to Update Center".to_owned()),
                selected_line(1, "Close".to_owned()),
            ]
        }
    }
}
