# Jarvis Home Node administration

## Source layout

The Rust administrator keeps command dispatch in `crates/admin/src/main.rs`.
Terminal capability/lifecycle support, the persistent Update Center, safe
agent-tree projection, versioned compatibility-helper resolution and tests live
in dedicated sibling modules. This keeps presentation state separate from the
trusted filesystem and process boundaries without introducing a second TUI
lifecycle.

The primary owner interface on a provisioned Home Node is the root-operated,
persistent administration console:

```bash
sudo jarvis
```

It provides Overview, Update, Health, Services, Agents, Models, Costs,
Credentials, Logs and System views in one Ratatui session. Opening a section
does not start a nested alternate-screen application. Direct commands remain first-class for
troubleshooting, automation and direct navigation:

```bash
sudo jarvis status
sudo jarvis health
sudo jarvis update --check
sudo jarvis update
```

`jarvis` is installed as a root-owned executable at `/usr/local/sbin/jarvis` by
the idempotent Home Node preparation/install flow. It does not grant Jarvis
Core, agents, Codex, or OpenSandbox any administrative authority.

On an interactive terminal, `sudo jarvis` opens Overview. `sudo jarvis status`,
`sudo jarvis health`, and `sudo jarvis update` enter their corresponding views
directly through the same application architecture. Press Esc to return, and
`q` or Ctrl-C to close at the root. Redirected output, `TERM=dumb`, `NO_COLOR`,
and `--json` remain plain and script-friendly. Bare non-interactive `jarvis`
requires an explicit command and never performs an implicit mutation. Bare
`jarvis --json` also fails with an explicit-command message rather than opening
a TUI.

Machine-readable read-only forms also include:

```bash
sudo jarvis --json health
sudo jarvis --json update --check
sudo jarvis --json update --status
sudo jarvis --json models list
sudo jarvis --json usage
sudo jarvis --json credentials list
sudo jarvis --json agents status
sudo jarvis --json logs core --lines 100
```

JSON mode rejects mutating updates and streaming `logs --follow`; it never
silently prompts or emits terminal control sequences.

`sudo jarvis version` reports the active release plus the independent Core,
admin CLI and installed Core Admin App component versions. Its `--json` output
retains the existing `admin_version` and `active_core` keys and adds explicit
component fields.

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
The status/check result also shows current and latest Core, CLI and Core Admin
App versions. A new verified bundle is offered when any of those components is
updated.
Use the arrow keys or `j`/`k`, Enter, Esc and `q`. Failed operations remain
visible until the owner returns to the overview or closes the screen. A
successful update or rollback replaces the CLI executable, so the TUI restores
the terminal, exits immediately, and prints the result in normal terminal
output. Run `sudo jarvis` again to use the newly active client. Bare
`jarvis update` is rejected outside an interactive rich terminal; automation
must select an explicit operation.

Explicit `--check` and `--status` never initialize Ratatui or an alternate
screen. They print stable inline output; their `--json` variants remain
machine-readable. Explicit `--latest`, `--version` and `--rollback` remain
available for reviewed automation and SSH workflows.

Inside the main console, Update Center is a view in the shared application
state machine. Its check, mutation and result states do not create a nested
Ratatui lifecycle. Transactional activation remains non-cancellable and owned
by the trusted updater helper.

Only GitHub Releases that are neither draft nor prerelease are accepted. The
existing verified-release protocol downloads the artifact and checksum over
HTTPS, validates archive layout and release manifest, preserves the previous
release, restarts Core, and waits for bounded readiness checks. A failed
activation restores the previous known-good release. Automatic timer updates
continue to refuse schema-changing releases; perform those manually with a
backup and recovery plan.

Releases declaring `tooling.systemd_units: 1` also carry the exact ten
Home Node unit files as immutable `systemd-<unit-name>` artifacts. They and the
fixed unit manager are listed exactly once in `artifact-binaries.sha256`.
Activation validates every unit, preserves the installed canonical files,
atomically installs the candidate set, reloads systemd, restarts required
services in dependency order, and commits only after Core readiness succeeds.
Failure restores both `/opt/jarvis/current` and the previous canonical unit
set. A same-version `sudo jarvis update --version vX.Y.Z` reconciles drift from
that verified release instead of returning early.

The host ownership boundary is:

```text
/opt/jarvis/releases/vX.Y.Z/    immutable, verified release artifacts
/opt/jarvis/current             active release symlink
/etc/systemd/system/jarvis-*    installed copies of release-owned unit policy
/etc/systemd/system/*.d/*.conf  administrator-owned overrides (never deleted)
/etc/jarvis/                    protected configuration and credentials
/var/lib/jarvis/                persistent state
/run/jarvis-*                   ephemeral systemd-managed runtime state
```

Security/lifecycle directives in administrator drop-ins (for example
`ExecStart=` or `ProtectSystem=`) are incompatible with managed activation and
fail with the exact drop-in path. Benign tuning is retained. Releases predating
the capability remain inspectable. Once managed units are active, an old
release without its own exact unit artifacts is not offered as a normal future
rollback target; the updater never invents historic unit contents.

The verified release also contains `install-home-node-core`, so the supported
fresh-install step consumes its own unit artifacts after staging:

```bash
sudo /opt/jarvis/releases/vX.Y.Z/install-home-node-core \
  /opt/jarvis/releases/vX.Y.Z
```

The higher-level setup command may still prepare identities, secrets and
SurrealDB, but it no longer supplies the production unit bytes. Routine
upgrades need neither a Git checkout nor files below `deploy/`.

For the one-time transition from a release whose already-installed updater
predates `systemd_units`, run the normal version command twice. The first
verified activation installs the new updater; the second recognizes the same
active version and atomically reconciles its units:

```bash
sudo jarvis update --version vX.Y.Z
sudo jarvis update --version vX.Y.Z
sudo jarvis health
```

No file is copied from a checkout and no arbitrary repair path is accepted.
All subsequent releases install binaries and units together in one updater
transaction; same-version invocation remains the supported drift repair.

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
sudo jarvis credentials set huggingface
sudo jarvis credentials test huggingface
sudo jarvis models refresh huggingface
sudo jarvis models list huggingface
sudo jarvis models providers huggingface openai/gpt-oss-20b
sudo jarvis models set-route huggingface openai/gpt-oss-20b cheapest
sudo jarvis models enable huggingface openai/gpt-oss-20b
sudo jarvis credentials list
sudo jarvis credentials set openai
```

Plain, interactive and JSON model listings all include exact reviewed input,
cached-input and output prices per million tokens when known. Unknown remote
model IDs are labelled `unknown`; local models are labelled `local`. All three
presentations use the same typed pricing projection consumed by Core Admin.
Owner pricing entries take precedence over reviewed coverage shipped in the
active verified release.

Hugging Face uses `https://router.huggingface.co/v1` for metadata and
OpenAI-compatible chat. Its base model is the owner-authorized identity; the
HF inference route (`auto`, `fastest`, `cheapest`, `preferred`, or a currently
discovered live provider id) is stored separately. Selecting a route does not
enable a model. Dynamic routes use the highest complete live-provider price as
a conservative preflight estimate. If any eligible price is unavailable,
Jarvis reserves against the maximum accepted HF catalog-price ceiling, which
normally makes the request fail closed under the owner budget rather than
guessing a cheap price. Provider invoices remain authoritative.
Usage telemetry retains the non-secret base model, requested HF route and cost
classification. The actual inference provider remains `unknown` unless the HF
API reports it through a stable documented field; Jarvis never guesses it.

The bounded rich HF catalog is stored separately at
`/etc/jarvis/huggingface-catalog.json` as `root:jarvis 0640`. It contains only
model/provider metadata, never the token. The root model policy remains the
authorization source. New discoveries always start disabled.
The opaque token is stored only as
`JARVIS_LLM_HUGGINGFACE_API_KEY=…` in
`/etc/jarvis/secrets/huggingface.env`; `credentials test` performs only an
authenticated bounded `GET /v1/models` probe and never generates tokens.

Monthly aggregate telemetry is available through:

```bash
sudo jarvis usage
sudo jarvis --json usage
```

It includes request and token totals, daily/provider/model breakdowns and
estimated spend. The same data is available as the persistent Costs view in
`sudo jarvis`. Core refreshes the snapshot at startup, after metered requests
and periodically so a temporary database failure can recover without an extra
model call. The snapshot is bounded, root-controlled and contains no
prompts, replies, credentials or request identifiers. Provider invoices remain
authoritative. Calls made before token collection was introduced cannot be
reconstructed, and a provider that reports no token metrics contributes no
invented token count.

The command delegates to the existing root-managed policy and credential
helpers. A provider key never enables a model on its own. Credential input is
accepted only from the controlling TTY and is never accepted in an argument,
printed, or written to the journal.

The integrated Credentials view intentionally displays configuration status
only. Set, test and remove remain explicit trusted commands so secret input is
handled through `/dev/tty`; no credential value is stored in TUI state or
captured child output.

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
cargo run -p jarvis-admin --features tui-preview -- tui-preview home
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
