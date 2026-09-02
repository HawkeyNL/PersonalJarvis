//! Shared terminal capability checks, diagnostics and Ratatui lifecycle support.

use super::*;

pub(super) struct Presentation {
    pub(super) json: bool,
    pub(super) interactive: bool,
    pub(super) tui_trace: bool,
}
impl Presentation {
    pub(super) fn new(json: bool, tui_trace: bool) -> Self {
        Self {
            json,
            interactive: !json && terminal_supports_rich_output(),
            tui_trace,
        }
    }
    pub(super) fn intro(&self, text: &str) {
        if !self.json {
            println!("{text}");
        }
    }
    pub(super) fn outro(&self, text: &str) {
        if !self.json {
            println!("{text}");
        }
    }
}

pub(super) fn terminal_supports_rich_output() -> bool {
    terminal_supports_rich_output_for(
        io::stdout().is_terminal(),
        std::env::var_os("NO_COLOR").is_none(),
        std::env::var("TERM").ok().as_deref(),
    )
}

pub(super) fn terminal_supports_rich_output_for(
    stdout_is_tty: bool,
    color_allowed: bool,
    term: Option<&str>,
) -> bool {
    stdout_is_tty && color_allowed && term != Some("dumb")
}

pub(super) fn terminal_diagnostics(json: bool) -> Result<()> {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TuiExitReason {
    Quit,
    Escape,
    CtrlC,
    ProcessCompleted,
    SelectedClose,
    ClientReplaced,
}

impl std::fmt::Display for TuiExitReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Quit => "q key",
            Self::Escape => "Escape key",
            Self::CtrlC => "Ctrl-C",
            Self::ProcessCompleted => "child process completed",
            Self::SelectedClose => "Close action",
            Self::ClientReplaced => "active admin client replaced by update",
        })
    }
}

pub(super) fn close_exit_reason(event: &Event) -> Option<TuiExitReason> {
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

pub(super) struct TuiTrace {
    pub(super) enabled: bool,
    pub(super) started: std::time::Instant,
    pub(super) entries: VecDeque<String>,
    pub(super) failure: Option<String>,
}

impl TuiTrace {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: std::time::Instant::now(),
            entries: VecDeque::new(),
            failure: None,
        }
    }

    pub(super) fn record(&mut self, entry: impl Into<String>) {
        if !self.enabled {
            return;
        }
        if self.entries.len() == 16 {
            self.entries.pop_front();
        }
        self.entries.push_back(entry.into());
    }

    pub(super) fn io<T>(&mut self, stage: &str, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result {
            self.failure = Some(format!("{stage}: {error}"));
        }
        result
    }

    pub(super) fn record_event(&mut self, event: &Event) {
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

    pub(super) fn finish<T>(
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

#[cfg(feature = "tui-preview")]
pub(super) fn status_tui(report: &StatusReport, trace_enabled: bool) -> Result<()> {
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

#[cfg(any(feature = "tui-preview", test))]
pub(super) fn render_status_dashboard(frame: &mut ratatui::Frame, report: &StatusReport) {
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

#[cfg(any(feature = "tui-preview", test))]
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

pub(super) fn table_tui(
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
