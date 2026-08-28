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

- Interactive TTY: `sudo jarvis status` opens a compact dashboard (`q` or Esc
  closes it).
- Pipes, `TERM=dumb`, `NO_COLOR`, and `--json`: use plain/stable output; JSON
  never enables alternate-screen, ANSI animation, cursor changes or prompts.
- Root remains required for administration. The Core service stays
  unprivileged.
- Ratatui's managed run lifecycle restores the terminal after normal exit,
  errors and panics. Secret prompts remain separate TTY-only flows and never
  enter the dashboard state or JSON output.

The UI intentionally remains restrained: fast commands retain plain output,
while dashboards and future long-running transaction progress can use the TUI.
