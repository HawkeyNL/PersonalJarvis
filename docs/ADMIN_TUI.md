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
- Pipes, `TERM=dumb`, `NO_COLOR`, and `--json`: use plain/stable output; JSON
  never enables alternate-screen, ANSI animation, cursor changes or prompts.
- Root remains required for administration. The Core service stays
  unprivileged.
- Ratatui's managed run lifecycle restores the terminal after normal exit,
  errors and panics. Secret prompts remain separate TTY-only flows and never
  enter the dashboard state or JSON output.

Update progress is driven by messages emitted only after the underlying
release resolver, SHA-256 verifier, archive validator, immutable stager,
readiness probe and tooling activator complete their real step. Esc/Ctrl-C
terminates the child operation and restores the terminal. Fast commands retain
plain output where a full-screen interface adds no value.
