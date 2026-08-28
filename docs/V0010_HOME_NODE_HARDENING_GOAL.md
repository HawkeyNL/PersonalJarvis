# Goal: v0.0.10 Home Node hardening + polished `sudo jarvis` UX

Perform a focused production hardening pass after the real Ubuntu Home Node upgrade exposed two v0.0.10 deployment regressions. Also polish the newly merged unified `sudo jarvis` owner CLI so it is pleasant and safe to use interactively.

Do not weaken any security boundary to improve UX. Work against current main and inspect adjacent Home Node systemd/deployment scripts for the same classes of bug rather than fixing only the two observed lines.

## 1. Fix TTY destruction in pretty command runner

Observed on a real interactive Home Node:

- parent shell: stdin TTY YES, stdout TTY YES
- normal `setup-home-node.sh` fails during SurrealDB root configuration with `requires an interactive terminal so secrets are not redirected into logs`
- `JARVIS_VERBOSE=1` succeeds

Root cause: `ui_run` redirects child stdout/stderr to a regular temporary file, while security-sensitive helpers intentionally require both stdin and stdout to remain a TTY.

Requirements:

- keep the TTY checks in secret/bootstrap helpers;
- add an explicit TTY-preserving runner such as `ui_run_interactive` / `ui_run_tty`, or an equally clear mechanism;
- use it for every helper that may prompt, display one-time secrets, or explicitly requires a TTY;
- do not pipe/capture secrets into temporary files;
- normal pretty mode must work without `JARVIS_VERBOSE=1`;
- noninteractive automation must remain fail-closed unless a command explicitly supports a reviewed noninteractive mode;
- test stdin and stdout TTY behavior with a pseudo-terminal fixture;
- audit all deployment/admin scripts for other `-t`, `/dev/tty`, hidden-input or confirmation paths that are accidentally wrapped by output capture.

## 2. Fix config broker runtime/state directory lifecycle

Observed on the real v0.0.10 Home Node:

`jarvis-config-broker.service` exits `226/NAMESPACE` before ExecStart because `/run/jarvis-config-broker` does not exist while it is listed in `ReadWritePaths=`.

The release binary itself exists.

Requirements:

- use systemd `RuntimeDirectory=jarvis-config-broker` with restrictive mode/ownership appropriate to root:jarvis;
- use `StateDirectory=` where appropriate for persistent `/var/lib/jarvis/config-broker` state, or otherwise guarantee safe idempotent creation before namespacing;
- preserve `ProtectSystem=strict`, `NoNewPrivileges`, capability bounding and all existing hardening;
- do not rely on a manual `mkdir /run/...` workaround;
- verify cold boot where `/run` starts empty;
- ensure restart/stop lifecycle does not leave insecure permissions;
- test service startup after reboot semantics.

Systemd's documented behavior should guide this: `RuntimeDirectory=` exists specifically for managed `/run` state, while `ReadWritePaths=` paths must otherwise exist unless deliberately prefixed as optional. Do not simply prefix a required runtime path with `-` and hide the lifecycle bug.

## 3. Audit sibling systemd units for the same class of defect

Inspect every production Jarvis unit, including at least:

- Core
- config broker
- Codex broker/runtime
- OpenSandbox
- private-agent updater
- public updater
- SurrealDB wrapper

For every unit:

- validate all `ReadWritePaths`, `ReadOnlyPaths`, socket paths, runtime paths, state paths and working directories;
- ensure required ephemeral paths are created by systemd or a trusted preparation step;
- ensure required persistent paths are created idempotently with exact ownership/mode;
- verify referenced binaries exist in the release/install layout;
- run `systemd-analyze verify` against generated/installed units in CI where practical;
- add a fixture approximating a fresh boot with empty `/run`.

Do not create directories globally merely to make unit validation pass.

## 4. Audit setup idempotency and partial-failure recovery

The real upgrade failed partway through multiple times. A rerun must safely continue.

Test failure/retry boundaries around:

- host preparation
- SurrealDB existing credentials
- scoped DB account
- release already staged
- persona already installed
- agent bundle already active
- model policy already initialized
- systemd units partially installed/enabled
- broker failed before Core activation
- updater timers already enabled

Existing credentials must not rotate merely because a prior setup attempt failed later.

## 5. Transactional release activation

Review the v0.0.10 installation flow for activation ordering.

A new `/opt/jarvis/current` must not become the durable active release until all required binaries/config/unit prerequisites for that release are ready. If activation is necessarily early, failures must automatically restore the previous known-good release.

Test failures of:

- config broker
- Core
- schema verification
- readiness endpoint

Expected: previous verified release remains/restores usable, with concise diagnostics.

## 6. Preflight before mutating production

Add a production upgrade preflight that validates as much as possible before changing the active Home Node:

- requested release exists and checksum verifies;
- required release binaries exist (`jarvis-api`, agent validator, config broker, Codex broker when required by release metadata);
- unit templates parse;
- required host commands exist;
- required persistent paths/config exist or can safely be created;
- Docker/SurrealDB prerequisites are healthy;
- private agent source is valid before activation;
- sufficient disk space for staging + rollback copy/retained release.

Preflight must never print secrets.

## 7. Polish `sudo jarvis` as the canonical owner CLI

The CLI currently works conceptually but its presentation should feel like a coherent administration product rather than dead shell output.

Use restrained terminal UX, not a full-screen TUI.

### General presentation

When stdout is an interactive color-capable terminal and `NO_COLOR` is not set:

- concise Jarvis header/title where useful;
- semantic colors only: green healthy/success, yellow warning/pending, red failure, cyan/blue headings;
- Unicode symbols only when terminal supports them safely; otherwise plain ASCII fallback;
- aligned labels/tables;
- blank lines between meaningful groups;
- no giant banners/ASCII art;
- no spinner that hides failures or breaks logs.

When piped/non-TTY or `NO_COLOR=1`:

- plain deterministic output;
- no ANSI escapes;
- stable enough for simple shell parsing where documented.

Consider a future `--json` machine-readable mode for status/check/list commands; implement it now where straightforward, but do not delay the core hotfix solely for it.

### `sudo jarvis --help`

Make help concise but useful, e.g. conceptually:

`Jarvis Home Node administration`

Commands grouped by intent:

- System: `status`, `health`, `version`, `logs`
- Updates: `update`
- AI: `models`, `credentials`
- Agents: `agents`

Include examples such as:

- `sudo jarvis update --check`
- `sudo jarvis update --latest`
- `sudo jarvis models list`
- `sudo jarvis credentials set ollama`

Every nested command must have useful `--help` and `-h`.

### `sudo jarvis status`

Produce a compact dashboard-like summary, conceptually:

Jarvis Home Node

System
  Release        v0.0.10
  Core           ● running
  SurrealDB      ● healthy
  Config broker  ● running
  API            ● ready
  Updater        ● enabled

AI
  Providers      2 configured
  Models         4 enabled
  Monthly spend  €3.42 / €50.00

Agents
  Bundle         bundle-...
  Agents         16
  Auto-update    ● enabled

Do not fail the entire status command merely because one component is unhealthy; show per-component state and return a documented non-zero status when overall health is degraded.

### `sudo jarvis health`

Show checks with compact symbols and elapsed time where useful. On failure, print a short actionable hint, not 100 journal lines by default.

Offer a documented verbose flag for deeper diagnostics.

### `sudo jarvis version`

Show at least:

- installed active release;
- latest stable release when network lookup succeeds;
- source/commit metadata from release manifest where available;
- whether update is available.

Network failure should not prevent showing installed version.

### `sudo jarvis update --check`

Output should clearly distinguish:

- current version;
- latest stable version;
- update available yes/no;
- no mutation performed.

### `sudo jarvis update --latest`

Before mutation show a concise plan:

- current -> target;
- release verified;
- rollback target;
- major components that will restart;

For an interactive terminal request confirmation if policy requires it. `--yes` is explicit automation consent.

During update show high-level phases only:

1. Preflight
2. Stage release
3. Validate protected config/agents
4. Install services
5. Health checks
6. Commit activation
7. Cleanup

On success show final version and health.

On failure show which phase failed, whether rollback succeeded, and the single best next diagnostic command.

### `sudo jarvis models`

`list` should render a readable table such as:

Provider | Model | Enabled | Health | Tier | Cost

Never imply a credential enables a model. Clearly distinguish discovered vs enabled.

### `sudo jarvis credentials`

`list` must show provider/configuration/health only, never values.

`set` keeps hidden TTY input and displays a concise success/validation/restart result.

### `sudo jarvis agents`

Status should show active bundle, source commit when known, count, updater state, last successful sync and rollback candidate.

## 8. Error UX

Normalize common errors into actionable messages.

Bad:
`curl: (7) Failed to connect...`

Better:
`ERROR Core did not become ready within 15s`
`Hint: sudo jarvis logs core`

Bad:
`status=226/NAMESPACE`

Better admin-facing summary:
`ERROR Config broker could not start`
`Hint: sudo jarvis logs config-broker`

Raw underlying diagnostics remain available under `--verbose` / `jarvis logs`.

## 9. Exit codes

Document and test useful exit behavior:

- 0 success/healthy/no update when appropriate;
- non-zero invalid invocation;
- non-zero degraded health;
- distinct non-zero update available for `--check` only if intentionally documented (otherwise keep check script-friendly with explicit output/JSON);
- non-zero update/install failure;
- rollback failure must be distinguishable in diagnostics.

Avoid surprising shell behavior.

## 10. Security regression sweep

Because the CLI centralizes privileged administration, audit for:

- shell injection through provider/model/version/repository strings;
- path traversal;
- unsafe symlink following in root-owned writes;
- TOCTOU around temp files/atomic rename;
- command injection via release metadata;
- secret leakage in argv, `set -x`, journal, temp output, errors;
- unsafe environment inheritance under sudo;
- arbitrary `journalctl` unit selection through `jarvis logs`;
- arbitrary rollback paths/versions;
- concurrent update/model/credential mutation races.

Use strict allowlists and locks where appropriate. Never construct root shell commands by concatenating untrusted strings.

## 11. Network/update resilience

Test:

- GitHub unavailable;
- DNS unavailable;
- release endpoint timeout;
- checksum asset missing;
- checksum mismatch;
- download interrupted;
- latest release equals installed;
- requested explicit older release;
- prerelease/draft must not be selected as latest;
- malformed GitHub response.

The running Home Node must remain untouched when discovery/staging fails before activation.

## 12. Disk/release retention

Ensure updates cannot fill the disk indefinitely.

Keep the active release plus a bounded number of verified rollback releases according to a documented retention policy. Never delete the current release or sole known-good rollback candidate during cleanup.

Preflight should fail before activation if safe staging/rollback space is unavailable.

## 13. Reboot acceptance test

After the hotfix, perform a production-like cold reboot test:

- `/run` starts clean;
- SurrealDB returns healthy;
- config broker starts without manual mkdir;
- Core becomes ready;
- model policy is readable through broker boundary;
- updater timers are enabled as configured;
- `sudo jarvis status` is healthy;
- `sudo jarvis health` succeeds;
- `/livez` and `/readyz` succeed.

## 14. Real Home Node acceptance sequence

The intended owner flow after release should be:

```bash
sudo jarvis --help
sudo jarvis version
sudo jarvis status
sudo jarvis health
sudo jarvis update --check
sudo jarvis models list
sudo jarvis credentials list
sudo jarvis agents status
```

Then, when the next release exists:

```bash
sudo jarvis update --latest
```

This must successfully update the Home Node using the canonical CLI without requiring a repository checkout or manual `setup-home-node.sh` invocation for routine upgrades.

## 15. Release

After CI and production-like tests pass, create the next patch release after v0.0.10 if no newer stable release exists.

## Definition of done

Complete when:

- normal pretty setup preserves TTY for interactive secret operations;
- no TTY security checks were weakened;
- config broker runtime directory is systemd-managed and survives reboot lifecycle correctly;
- sibling units are audited for missing runtime/state paths;
- partial setup failures are safely rerunnable;
- release activation/rollback is transactional enough that a failed new release does not strand the Home Node;
- preflight catches predictable deployment failures before activation;
- `sudo jarvis` output is polished, restrained and consistent;
- `--help` exists throughout the command tree;
- status/health/version/update/model/credential/agent output is readable and non-secret;
- `NO_COLOR` and non-TTY output remain clean;
- common errors are actionable;
- update/network/disk/rollback edge cases are tested;
- root CLI injection/path/secret/concurrency risks are covered by regression tests;
- cold reboot works from empty `/run`;
- next patch release is produced only after the full hotfix passes.