# Jarvis admin terminal presentation

Jarvis uses Ratatui with the Crossterm backend for interactive administration.
It was selected over `bubbletea-rs` because Ratatui has a substantially more
mature release history, backend support and terminal restoration API. The
Bubble Tea Rust ecosystem is promising and visually aligned with this project,
but remains pre-1.0 and describes its APIs as still evolving.

The administrative domain stays independent from presentation: status, update,
model, credential and agent operations return typed state and execute fixed
allowlisted helpers. The TUI only renders that state; it never grants
capabilities or executes shell input.

- Interactive TTY: `sudo jarvis` opens the persistent Jarvis Home Node console.
  Its shared shell routes between Overview, Update, Health, Services, Agents,
  Models, Credentials, Logs and System without entering another alternate
  screen. Local state renders first; bounded background operations refresh the
  active view without blocking resize or navigation.
- Interactive `sudo jarvis status`, `sudo jarvis health` and `sudo jarvis
  update` directly enter the corresponding view through the same application
  architecture. Existing explicit commands remain available for direct use and
  scripting.
- Use Up/Down or `j`/`k` to navigate, Enter to activate, Esc to go back, `q` to
  go back or close at the root, `r` to refresh and `?` for help. Logs additionally
  support Page Up/Page Down, Home/End, target switching and a bounded follow
  mode.
- Pipes, `TERM=dumb`, `NO_COLOR`, and `--json`: use plain/stable output; JSON
  never enables alternate-screen, ANSI animation, cursor changes or prompts.
- Root remains required for administration. The Core service stays
  unprivileged.
- Ratatui's managed run lifecycle restores the terminal after normal exit,
  errors and panics. Secret prompts remain separate TTY-only flows and never
  enter the dashboard state or JSON output.

Update progress is driven by messages emitted only after the underlying
release resolver, SHA-256 verifier, archive validator, immutable stager,
readiness probe and tooling activator complete their real step. Read-only child
operations can be cancelled and reaped. Transactional mutations deliberately
remain non-cancellable while the trusted helper may be activating or
recovering a release; their final success, failure or rollback state stays on
screen. Explicit fast `update --check` and `update --status` commands retain
plain output where a full-screen interface adds no value.

The Services and Logs views use fixed Jarvis-owned unit/target allowlists. The
Models view passes only typed provider/model values from the validated policy
to trusted helpers. Credentials displays configuration status only: credential
values never enter TUI state, child output, command arguments or trace data.
Secret setting and removal remain explicit direct commands using the trusted
controlling-TTY path.

The Agents view builds its expandable tree exclusively from bounded safe fields
in the active bundle manifest (`id`, optional `name`, optional `group` and
optional provider-neutral `model_policy`). Older flat manifests appear under
`Ungrouped`. The view never opens the referenced agent definition files, so
private instructions and prompt bodies cannot enter presentation state.

Bare non-interactive `jarvis` performs no action and asks for an explicit
command. Bare `jarvis --json` likewise fails instead of opening Ratatui. Every
JSON command stays machine-oriented and never initializes the terminal UI.

## Safe diagnostics and fixture preview

`jarvis terminal-diagnostics` is a rootless, read-only command. It reports only
TTY booleans, `TERM`, dimensions, `NO_COLOR`, the rich-output decision,
Crossterm raw-mode/alternate-screen/event-poll availability, restoration and
whether sudo metadata is present. It does not enumerate the environment or
read Jarvis configuration. `--json` emits the same bounded report as JSON.

Use `--tui-trace` on an interactive command to print a bounded, non-secret
lifecycle trace after Ratatui restores the terminal. The trace records the
first successful frame, resize/event classes, the failing I/O stage, and an
explicit exit reason. It never records pasted or typed contents.
Persistent views that close successfully in under 0.75 seconds report their
exit reason automatically after terminal restoration, so a flash cannot remain
an unexplained exit-zero result.

The rootless preview is compiled only when its opt-in feature is selected and
contains fixture data only:

```bash
cargo run -p jarvis-admin --features tui-preview -- tui-preview home
cargo run -p jarvis-admin --features tui-preview -- tui-preview home-degraded
cargo run -p jarvis-admin --features tui-preview -- tui-preview healthy-status
cargo run -p jarvis-admin --features tui-preview -- tui-preview degraded-status
cargo run -p jarvis-admin --features tui-preview -- tui-preview models
cargo run -p jarvis-admin --features tui-preview -- tui-preview credentials
cargo run -p jarvis-admin --features tui-preview -- tui-preview agents
cargo run -p jarvis-admin --features tui-preview -- tui-preview update-center
cargo run -p jarvis-admin --features tui-preview -- tui-preview update-center-failure
cargo run -p jarvis-admin --features tui-preview -- tui-preview update-running
cargo run -p jarvis-admin --features tui-preview -- tui-preview update-success
cargo run -p jarvis-admin --features tui-preview -- tui-preview update-failure-rollback
cargo run -p jarvis-admin --features tui-preview -- tui-preview logs
cargo run -p jarvis-admin --features tui-preview -- tui-preview narrow-long
```

`home` is the complete fixture Home Node console and is the preferred visual
development entry point. It includes healthy/degraded state, services, agents,
models, non-secret credential status, bounded long logs, system provenance and
the Update Center's running/success/failure states. Resize it and exercise `q`,
Esc, Ctrl-C and narrow layouts. The feature is absent from release builds and
the preview path performs no system reads, subprocess execution, mutation or
privileged operation.
