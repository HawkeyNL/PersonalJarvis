# Jarvis Home Node administration

The canonical owner interface on a provisioned Home Node is the root-operated,
allowlisted command:

```bash
sudo jarvis status
sudo jarvis health
sudo jarvis update --check
sudo jarvis update
```

`jarvis` is installed as a root-owned executable at `/usr/local/sbin/jarvis` by
the idempotent Home Node preparation/install flow. It does not grant Jarvis
Core, agents, Codex, or OpenSandbox any administrative authority.

On an interactive terminal, `sudo jarvis status` opens a compact Ratatui
dashboard; press `q`, Esc, or Ctrl-C to leave it. Redirected output,
`TERM=dumb`, `NO_COLOR`, and `--json` remain plain and script-friendly. For
example, `sudo jarvis --json status` emits stable JSON without ANSI controls,
prompts, or an alternate terminal screen.

## Updates

```bash
sudo jarvis update                 # latest published stable release
sudo jarvis update --version v0.0.9
sudo jarvis update --check          # non-mutating; exit 2 means available
sudo jarvis update --status
sudo jarvis update --rollback       # asks for confirmation
```

Only GitHub Releases that are neither draft nor prerelease are accepted. The
existing verified-release protocol downloads the artifact and checksum over
HTTPS, validates archive layout and release manifest, preserves the previous
release, restarts Core, and waits for bounded readiness checks. A failed
activation restores the previous known-good release. Automatic timer updates
continue to refuse schema-changing releases; perform those manually with a
backup and recovery plan.

The update source is a root-owned `/etc/jarvis/updater.env`; it is created by
setup and is never taken from the invoking shell environment. A release carries
the versioned Rust admin binary and updater helper alongside Core. After Core
readiness succeeds, those two tools are staged and activated together from the
already checksum-verified artifact.

### Legacy v0.0.10 mixed-tooling recovery

Old installers could activate a new Core while leaving the old shell admin
helper in place. To cross that one unavoidable boundary, install a release that
contains `migrate-installed-tooling` with the legacy updater, then run the
verified binary directly once:

```bash
sudo env JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis \
  jarvis update --version vMAJOR.MINOR.PATCH
sudo /opt/jarvis/current/jarvis migrate-installed-tooling
sudo jarvis update --check
```

The migration atomically installs only the CLI and updater bundled in the
active verified release and writes `/etc/jarvis/updater.env` as `root:root
0600`. Thereafter normal updates need no repository environment variable.

## Models and credentials

```bash
sudo jarvis models list
sudo jarvis models refresh
sudo jarvis models enable openai-api gpt-4o-mini
sudo jarvis credentials list
sudo jarvis credentials set openai
```

The command delegates to the existing root-managed policy and credential
helpers. A provider key never enables a model on its own. Credential input is
accepted only from the controlling TTY and is never accepted in an argument,
printed, or written to the journal.

## Private agents

```bash
sudo jarvis agents status
sudo jarvis agents check
sudo jarvis agents update
sudo jarvis agents rollback
```

The private repository credential remains confined to the root-only private
agent updater. The Core, agent bundle, Codex and OpenSandbox do not receive it.
Agent rollback may only select a validated immutable bundle already under
`/var/lib/jarvis/agents/releases`.

Destructive operations prompt only on a controlling TTY. In automation they
fail closed unless their explicit `--yes` option is supplied. Credential input
is never rendered by the TUI or emitted in JSON.

## Diagnostics

```bash
sudo jarvis services status
sudo jarvis logs core --lines 100
sudo jarvis logs updater
sudo jarvis logs agents --follow
```

Log targets are allowlisted; the command is not a generic `journalctl` or
`systemctl` passthrough. For the full command list use `sudo jarvis --help`.
The previous `jarvis-models`, `jarvis-credentials`, and private updater helpers
remain compatibility/internal fallbacks, but normal owner operations should use
`sudo jarvis ...`.
