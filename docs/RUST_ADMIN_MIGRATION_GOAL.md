# Goal addendum: Rust-first Jarvis administration and deployment orchestration

This addendum is mandatory scope for PR #29.

The v0.0.10 production regressions exposed a broader maintainability problem: security-sensitive Home Node orchestration is spread across Bash scripts that mix TTY handling, secrets, state transitions, subprocess execution, retries, rollback, parsing and presentation.

Do not rewrite every shell helper at once. Instead, migrate the owner-facing and security-sensitive orchestration into a first-class Rust admin binary while keeping narrowly scoped shell/system helpers temporarily behind that boundary where they remain simpler and safe.

## 1. Canonical binary

Implement `jarvis` as a Rust binary/crate in the existing workspace and install it as the canonical owner command:

```text
sudo jarvis ...
```

Use:

- `clap` for command hierarchy, arguments and generated help;
- Ratatui with the Crossterm backend for restrained interactive presentation;
- `serde` / `serde_json` for structured config/release/API data;
- `reqwest` for release/API discovery instead of shelling out to `curl` where practical;
- `tokio` only where async materially improves network/process orchestration;
- `secrecy`/`zeroize` or equivalent for secret-bearing in-memory values where appropriate;
- a well-maintained file-locking primitive for mutation locks;
- typed error handling (`thiserror`/`anyhow` according to existing project conventions).

Avoid dependency sprawl where stdlib or existing workspace dependencies are sufficient.

## 2. Rust owns the critical orchestration

Move the following control-plane responsibilities into Rust:

- `jarvis status`
- `jarvis health`
- `jarvis version`
- `jarvis logs`
- `jarvis update` discovery/preflight/stage/activate/health/rollback orchestration
- `jarvis models` owner-facing command flow
- `jarvis credentials` owner-facing command flow and secret prompting
- `jarvis agents` status/update/rollback orchestration

Rust should invoke systemd/Docker/internal Jarvis binaries with typed `Command` arguments, never by concatenating untrusted shell strings.

## 3. Do not blindly rewrite every helper

Simple, narrowly scoped root helpers may remain temporarily if they have a clear contract and do not own complex state machines.

Examples that may remain during the first migration if justified:

- a small systemd installation helper;
- a fixed `docker compose` wrapper;
- immutable release extraction helper;
- tiny host preparation primitives.

However, the Rust admin CLI must become the only normal owner-facing entry point. Existing Bash CLIs such as `jarvis-models.sh`, `jarvis-credentials.sh` and setup/update wrappers become internal compatibility shims and should be marked deprecated once equivalent Rust paths are production-ready.

Do not maintain two independent implementations of update policy or credential semantics.

## 4. Cliclack UX

Use CliClack for interactive terminal presentation. Keep it polished but restrained.

Conceptual output:

```text
┌  Jarvis Home Node
│
◇  System
│  Release          v0.0.11
│  Core             ● Running
│  SurrealDB        ● Healthy
│  Config Broker    ● Running
│  API              ● Ready
│
◇  AI
│  Providers        2 configured
│  Models           4 enabled
│  Budget           €3.42 / €50.00
│
└  Jarvis is ready
```

And update flow:

```text
┌  Jarvis Update
│
◇  Checking latest stable release
◆  v0.0.11 available
│  Current          v0.0.10
│  Target           v0.0.11
│
◇  Preflight
│  ✓ checksum verified
│  ✓ rollback available
│  ✓ disk space sufficient
│
◇  Staging release
◇  Validating protected configuration
◇  Restarting services
◇  Running health checks
│
└  Updated successfully to v0.0.11
```

Requirements:

- no giant ASCII art;
- no animation in non-TTY output;
- no spinner around commands that may emit a secret;
- semantic color only;
- `NO_COLOR` support;
- clean ASCII/plain fallback when Unicode/color are unsuitable;
- stdout suitable for humans, stderr for diagnostics/errors;
- optional `--json` for status/check/list operations where useful.

## 5. TTY and subprocess policy

The Rust CLI must explicitly classify subprocesses as one of:

- inherited interactive TTY;
- captured non-secret output;
- streamed verbose diagnostics;
- silent/internal structured operation.

Never accidentally destroy TTY semantics by redirecting a security-sensitive child to a tempfile.

Interactive secret/bootstrap operations inherit the real terminal or are implemented directly in Rust.

This must eliminate the class of v0.0.10 `ui_run` bug rather than merely adding another Bash exception.

## 6. Credential manager migration

Prefer implementing credential prompting/storage orchestration directly in Rust.

Required:

- hidden TTY input via CliClack/password-safe primitive;
- secret never supplied as normal argv;
- no echo/logging/debug formatting;
- atomic temp-file + ownership/mode + rename semantics;
- rollback on failed Core health check where appropriate;
- provider allowlist represented as typed enum/registry;
- `list` never displays values;
- `test` uses provider-specific safe validation;
- no provider credentials inherited by arbitrary child processes, OpenSandbox or Codex.

Existing credential shell helper may remain temporarily as a compatibility implementation only if the Rust path is not yet complete, but the final owner flow must not depend on it.

## 7. Update state machine

Implement the updater as an explicit Rust state machine or equivalently clear staged transaction:

```text
ResolveRelease
  -> Preflight
  -> Download
  -> Verify
  -> Stage
  -> Validate
  -> PrepareServices
  -> Activate
  -> Restart
  -> HealthCheck
     -> CommitSuccess
     -> Rollback on failure
```

Each transition should be testable.

Persist only the minimum update transaction metadata necessary for safe recovery. Never leave `/opt/jarvis/current` pointing at a failed release without an automatic rollback attempt.

The updater must use the latest published stable GitHub Release for `--latest`, excluding drafts/prereleases.

## 8. Network handling in Rust

Use a real HTTP client instead of parsing `curl` output for release discovery where practical.

Requirements:

- bounded connect/read timeouts;
- useful user-agent;
- explicit status handling;
- response size bounds where relevant;
- typed JSON release parsing;
- no silent fallback from failed checksum verification;
- interrupted/failed download leaves active production untouched;
- download to root-controlled temporary/staging path and atomically finalize.

## 9. Process execution safety

Centralize subprocess execution helpers in Rust.

Never use `sh -c` with interpolated owner/provider/model/version/repository values.

Use `Command::new()` + `.arg()`/`.args()`.

Have typed allowlists for systemd units exposed through `jarvis logs`, such as:

- core
- surrealdb
- config-broker
- codex-broker
- opensandbox
- updater
- agents-updater

Do not allow arbitrary root `journalctl` unit selection through user input.

## 10. Systemd/runtime lifecycle still must be fixed

The Rust migration does not replace the concrete v0.0.10 service fixes.

Still fix:

- `RuntimeDirectory=jarvis-config-broker`;
- safe persistent state directory lifecycle;
- sibling unit runtime/state/path audit;
- cold reboot from empty `/run`;
- `systemd-analyze verify` coverage.

Do not add Rust-side `mkdir /run/...` hacks for paths systemd should own.

## 11. Bootstrap/setup migration boundary

Routine upgrades must use Rust:

```text
sudo jarvis update --latest
```

Fresh-machine bootstrap may initially retain a small shell entry point because the `jarvis` binary does not yet exist before installation.

If a bootstrap shell remains, restrict its role to:

- prerequisite setup;
- verified initial binary installation;
- handing control to the Rust `jarvis` installer/orchestrator.

After `jarvis` exists, normal setup/update/configuration should route through Rust.

Design toward a future minimal bootstrap such as:

```text
bootstrap-home-node.sh -> install verified jarvis binary -> sudo jarvis install ...
```

Do not require a Git checkout for routine upgrades.

## 12. Rust-native tests

Add unit/integration tests for:

- Clap command parsing/help tree;
- invalid/unknown command behavior;
- status aggregation;
- health aggregation;
- release semver comparison;
- latest-stable filtering;
- prerelease/draft exclusion;
- update state transitions;
- checksum mismatch;
- network timeout;
- disk/preflight failure;
- rollback transition;
- concurrent mutation lock;
- provider/model input validation;
- credentials redaction;
- no secret in Debug/Display/error output;
- process argument construction without shell interpolation;
- TTY/non-TTY/NO_COLOR rendering;
- JSON output stability for documented machine-readable commands.

Use fixtures/mocks rather than mutating the developer machine in normal unit tests.

## 13. Compatibility and migration

For one or more releases, existing commands may remain as compatibility wrappers:

```text
jarvis-models -> exec jarvis models ...
jarvis-credentials -> exec jarvis credentials ...
```

or equivalent.

Prefer wrappers pointing toward the Rust implementation rather than Rust invoking old shell frontends indefinitely.

Print deprecation messaging only where it will not break automation; document the transition.

## 14. Release packaging

Package the Rust `jarvis` binary in the release artifacts and install a stable path such as `/usr/local/sbin/jarvis` or another existing project-consistent location.

The command must not depend on executing from the repository checkout.

`sudo jarvis version` must be able to identify its own binary/release version and the active Core release.

## Definition of done

Complete when:

- `sudo jarvis` is a Rust binary using Clap;
- interactive presentation uses CliClack;
- normal owner administration no longer depends on Bash UI orchestration;
- critical update orchestration is typed and transactional in Rust;
- credential input/orchestration is Rust-native or has a clearly temporary compatibility boundary;
- TTY-sensitive subprocesses explicitly inherit the terminal;
- `NO_COLOR` and non-TTY output are clean;
- status/health/update/models/credentials/agents commands have coherent polished UX;
- root subprocess calls avoid interpolated shell execution;
- existing systemd v0.0.10 regressions are still fixed properly;
- shell helpers remaining after this PR are narrowly scoped and documented;
- routine Home Node updates require no repo checkout and use `sudo jarvis update`;
- tests cover state-machine, TTY, secret, rollback and command-injection boundaries;
- release packaging includes the Rust admin binary.
