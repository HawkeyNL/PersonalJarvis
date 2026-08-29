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

- Interactive TTY: `sudo jarvis status` opens a compact dashboard (`q`, Esc or
  Ctrl-C closes it). Health, model, credential and agent status use the same
  responsive table language. Bounded log snapshots use full-screen tables;
  `logs --follow` keeps a rolling view and updates stream through a cancellable
  TUI.
- Interactive `sudo jarvis update` opens one persistent Update Center session.
  Checks refresh that screen in place; update and rollback results remain until
  owner dismissal. Arrow keys or `j`/`k` navigate and Enter activates the
  selected allowlisted action.
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

Resize these views and exercise `q`, Esc, and Ctrl-C. The feature is absent
from release builds and the preview path performs no system reads, subprocess
execution, mutation, or privileged operation.
