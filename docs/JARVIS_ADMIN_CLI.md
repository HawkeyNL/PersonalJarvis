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

Machine-readable read-only forms also include:

```bash
sudo jarvis --json health
sudo jarvis --json update --check
sudo jarvis --json update --status
sudo jarvis --json models list
sudo jarvis --json credentials list
sudo jarvis --json agents status
sudo jarvis --json logs core --lines 100
```

JSON mode rejects mutating updates and streaming `logs --follow`; it never
silently prompts or emits terminal control sequences.

## Updates

```bash
sudo jarvis update                 # persistent Update Center on an interactive TTY
sudo jarvis update --latest         # explicit latest published stable release
sudo jarvis update --version v0.0.9
sudo jarvis update --check          # non-mutating; exit 2 means available
sudo jarvis update --status
sudo jarvis update --rollback       # asks for confirmation
```

The Update Center remains open while checks and trusted updater operations run.
It shows the active and latest stable releases, update availability, updater
timer state, rollback availability and the last result in the current session.
Use the arrow keys or `j`/`k`, Enter, Esc and `q`. Successful and failed
operations remain visible until the owner returns to the overview or closes the
screen. Bare `jarvis update` is rejected outside an interactive rich terminal;
automation must select an explicit operation.

Explicit `--check` and `--status` never initialize Ratatui or an alternate
screen. They print stable inline output; their `--json` variants remain
machine-readable. Explicit `--latest`, `--version` and `--rollback` remain
available for reviewed automation and SSH workflows.

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

The installed release receives a root-owned `release.verification` marker only
after the downloaded archive passes its published SHA-256. Explicit rollback
activates the historical Core, Rust CLI and updater helper as one unit. If
readiness or tooling activation fails, the previous symlink, Core and tooling
are restored before the operation reports failure.

The trusted helper enumerates rollback candidates for the Update Center and
reports version, current/verified state, rollback eligibility and a bounded
reason. It independently validates the managed-root path, stable tag,
manifest, verification marker, expected binaries/tooling, ownership,
permissions and schema compatibility. Invalid historical releases are shown as
unavailable or skipped; they cannot mask a valid target or block an unrelated
future update.

Releases installed by an updater older than v0.0.14 may lack that persisted
marker even though the legacy updater verified the published checksum. On the
first later mutating update or rollback, the current and immediately previous
release are migrated once: the updater revalidates their exact manifests,
required executables, root ownership and non-writable immutable layout, then
atomically records a manifest-bound legacy marker. Read-only check/status
commands never mutate this state, and unrelated historical directories are
never promoted by this compatibility path.

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

## Home Node development without replacing production

Clone and build as the normal owner, never through `sudo cargo` or `sudo cargo
run`:

```bash
mkdir -p ~/dev
git clone https://github.com/HawkeyNL/PersonalJarvis.git ~/dev/PersonalJarvis
cd ~/dev/PersonalJarvis
cargo build -p jarvis-admin --bin jarvis
cargo run -p jarvis-admin --features tui-preview -- tui-preview healthy-status
cargo run -p jarvis-admin -- terminal-diagnostics
```

After reviewing the exact binary, install it under a separate name for
root-only, read-only integration tests:

```bash
sudo install -o root -g root -m 0755 target/debug/jarvis /usr/local/sbin/jarvis-dev
sudo /usr/local/sbin/jarvis-dev --tui-trace status
```

This does not replace `/usr/local/sbin/jarvis`. Do not perform a mutating
update through `jarvis-dev` until the read-only acceptance matrix in
`docs/RELEASE_CANDIDATE.md` passes.
