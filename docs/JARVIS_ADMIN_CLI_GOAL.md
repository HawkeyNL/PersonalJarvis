# Goal: Unified `jarvis` Home Node administration CLI

Implement a single first-class root-operated administration command for the PersonalJarvis Home Node.

The owner should no longer need to remember separate script paths or manually enter `/etc/jarvis` for routine administration.

The installed command must be available as:

```bash
sudo jarvis <command> [options]
```

At minimum support:

```bash
sudo jarvis help
sudo jarvis --help
sudo jarvis version
sudo jarvis status
sudo jarvis health

sudo jarvis update
sudo jarvis update --latest
sudo jarvis update --version v0.0.9
sudo jarvis update --check
sudo jarvis update --status
sudo jarvis update --rollback

sudo jarvis models list
sudo jarvis models refresh
sudo jarvis models enable <provider> <model>
sudo jarvis models disable <provider> <model>
sudo jarvis models status

sudo jarvis credentials list
sudo jarvis credentials set <provider>
sudo jarvis credentials test <provider>
sudo jarvis credentials remove <provider>

sudo jarvis agents status
sudo jarvis agents check
sudo jarvis agents update
sudo jarvis agents rollback

sudo jarvis services status
sudo jarvis logs core
sudo jarvis logs updater
```

Aliases may remain for backwards compatibility, such as `jarvis-models` and `jarvis-credentials`, but the canonical owner UX is the single `jarvis` command.

Work against CURRENT main. Reuse the existing model/credential scripts, release updater, private-agent updater, verified release staging, health checks, immutable bundle activation and rollback mechanisms. Do not create parallel/conflicting implementations.

## CLI design

Use a proper command/subcommand parser where practical. The UX must be discoverable:

```bash
sudo jarvis --help
sudo jarvis update --help
sudo jarvis models --help
sudo jarvis credentials --help
sudo jarvis agents --help
```

Unknown commands/options must fail non-zero with a concise useful message and suggest the relevant `--help`.

Normal output should be concise, structured and readable. Respect `NO_COLOR`; use restrained color only on interactive terminals.

Never print secret values.

## `jarvis update`

`sudo jarvis update` is equivalent to `sudo jarvis update --latest`.

`--latest` must resolve the latest **published stable GitHub Release** for `HawkeyNL/PersonalJarvis`, not merely the highest local Git tag.

Ignore drafts, prereleases, unpublished tags and branches.

Before activating an update:

1. resolve the target release;
2. verify expected release assets exist;
3. download/stage through the existing verified release flow;
4. verify checksum/digest using the current trusted mechanism;
5. preserve current known-good release;
6. activate using the existing Home Node installer;
7. run bounded `/livez` and `/readyz` checks;
8. verify critical systemd services;
9. report success only after the target release is actually healthy.

If activation/health fails, use the existing rollback boundary and leave the prior known-good release active where possible.

Do not silently rotate unrelated credentials, owner persona, device state, SurrealDB credentials or agent configuration during a routine Core update.

### `--version`

Example:

```bash
sudo jarvis update --version v0.0.9
```

Only published stable releases are accepted by default. Reject malformed/unknown versions before mutating production.

### `--check`

Must perform no update. Show conceptually:

```text
Current:  v0.0.9
Latest:   v0.0.10
Update:   available
```

Return useful exit semantics/documentation so scripts can distinguish no-update/update-available/failure if appropriate.

### `--status`

Show current release, previous known-good release, updater timer state, last update attempt/result where available, and whether a newer stable release exists.

### `--rollback`

Rollback only to a known verified historical release managed by Jarvis. Never accept arbitrary filesystem paths.

Require explicit confirmation on an interactive terminal unless a deliberate `--yes` option is supplied.

After rollback, run the same Core health checks.

## Models integration

Do not duplicate model-policy logic. Wrap/reuse the model access system merged with the intelligent router.

`sudo jarvis models ...` must provide the same protected behavior as the existing `jarvis-models` tooling.

A credential alone must never enable a paid model. `enable` operates only on models discovered/recognized by the model registry and persists through the existing root-managed policy.

## Credentials integration

Wrap/reuse the secure credential manager.

`sudo jarvis credentials set <provider>` must keep hidden TTY entry and must never accept/display the secret in normal argv/stdout/logs.

`list` shows configured/status only. `test` uses the smallest safe provider validation. `remove` requires confirmation and preserves other providers.

Do not weaken `/etc/jarvis` permissions just to simplify administration.

## Agent administration

Integrate with the private-agent updater architecture rather than letting Jarvis Core clone/pull the private repository.

Commands should conceptually support:

- `agents status`: active bundle, source commit if known, agent count, last update result;
- `agents check`: check whether a newer approved `PersonalJarvisAgents/main` commit is available without activating it;
- `agents update`: trigger the trusted updater once and report validation/activation/health result;
- `agents rollback`: switch atomically to a previously validated bundle.

The private GitHub credential remains updater-only and unreadable by Core, arbitrary agents and OpenSandbox/Codex.

## Status / health

`sudo jarvis status` should provide a compact Home Node overview, for example:

```text
Jarvis Home Node
Core release       v0.0.9
Core               healthy
SurrealDB          healthy
Agent bundle       bundle-... (16 agents)
Model router       ready
Configured AI      3 providers / 7 enabled models
Updater             enabled
Private agents      up to date
OpenSandbox         disabled
```

Do not expose credentials or sensitive config values.

`sudo jarvis health` should perform actual bounded checks and return non-zero when production is unhealthy.

## Logs

Provide safe convenience log access, e.g.:

```bash
sudo jarvis logs core
sudo jarvis logs surrealdb
sudo jarvis logs updater
sudo jarvis logs agents
```

Default to a bounded recent number of lines and support useful options such as `--follow`/`--lines` if implemented safely.

Do not add a generic arbitrary-unit passthrough that accidentally becomes a root shell primitive.

## Version command

`sudo jarvis version` should show at least:

- installed CLI/tooling version where meaningful;
- active Core release;
- optionally source/build commit if embedded.

Do not confuse repository checkout version with the active production release.

## Installation

Install the canonical command into an appropriate root-managed executable path such as `/usr/local/bin/jarvis` through the idempotent Home Node setup/update flow.

The command must remain available after reboot and across release upgrades.

Prefer packaging the CLI/wrapper in release/deployment assets so operational behavior is versioned and reproducible.

Existing standalone scripts may remain internal implementation details, but owner documentation should point to `sudo jarvis ...`.

## Security

The admin CLI is a privileged control surface. Treat arguments and provider/model identifiers as untrusted input.

Do not:

- `eval` user input;
- allow arbitrary command execution;
- allow arbitrary filesystem paths for update/rollback/log commands;
- expose secrets;
- give Jarvis Core root privileges;
- grant Core the private GitHub updater credential;
- weaken existing systemd/filesystem hardening;
- bypass device/owner approvals for operations that already require them.

Use explicit allowlists for service/log names and provider IDs where applicable.

## Concurrency / locking

Prevent conflicting privileged mutations from running concurrently.

For example, two `jarvis update` processes or an update plus rollback should not race. Reuse or introduce a root-owned lock with bounded failure behavior.

Model/credential/agent mutations should also coordinate with existing broker/updater locks where required.

## Failure UX

When an operation fails, print:

- which stage failed;
- whether production is still on the previous known-good state;
- concise next diagnostic command, e.g. `sudo jarvis logs core`;
- no secret material.

Do not leave an ambiguous partial-success message.

## Tests

Add regression tests for at least:

- root requirement where appropriate;
- `jarvis --help` and subcommand help;
- unknown commands/options;
- `update` defaults to stable latest;
- drafts/prereleases/tags without published releases are ignored;
- `update --version` validates the release;
- checksum/staging failure never activates;
- failed Core health rolls back or preserves known-good release;
- `update --check` is non-mutating;
- rollback accepts only known verified releases;
- concurrent update locking;
- models wrapper preserves allowlist semantics;
- credentials wrapper does not leak secrets;
- agent updater remains separate from Core credentials;
- status/health do not reveal secrets;
- log subcommands are allowlisted;
- idempotent Home Node install/upgrade preserves command availability.

Use fixtures/mocks for GitHub release discovery in normal CI; do not depend on live GitHub availability for deterministic unit tests.

## Documentation

Update Home Node operations documentation so the normal owner workflow becomes:

```bash
sudo jarvis status
sudo jarvis update --check
sudo jarvis update
sudo jarvis models list
sudo jarvis credentials list
sudo jarvis agents status
```

Document the advanced/manual scripts only as troubleshooting/internal fallback, not the primary UX.

## Definition of done

Complete when:

- one canonical `sudo jarvis ...` admin CLI is installed on the Home Node;
- top-level and subcommand `--help` are useful;
- `sudo jarvis update` safely installs the latest published stable release;
- `--version`, `--check`, `--status` and `--rollback` work safely;
- release verification/health/rollback reuse existing trusted mechanisms;
- model administration is available under `jarvis models`;
- credential administration is available under `jarvis credentials` with hidden/no-leak secret behavior;
- private-agent administration is available under `jarvis agents` without giving Core repo credentials;
- status/health/log convenience commands are safe and useful;
- existing security boundaries are not weakened;
- normal Home Node administration no longer requires manually entering `/etc/jarvis` or remembering deployment script paths;
- CI/security/deployment tests pass;
- documentation matches production behavior.
