# Goal: improve Home Node setup and verification output UX

Implement a human-friendly terminal presentation for the production Home Node setup and verification scripts without changing their security semantics, exit behavior, idempotency, or machine-readable logs.

## Current problem

The real v0.0.8 Home Node deployment now succeeds end-to-end, including:

- SurrealDB healthy
- scoped Core DB account retained
- release verification/staging
- protected persona loaded
- private AgentRegistry loaded with 16 agents
- `jarvis-api` listening on `127.0.0.1:8080`
- `/livez` and `/readyz` returning 200
- permission/security verification passing

However the shell output is visually dense and hard to scan. It currently mixes plain status labels such as `CREATE`, `UPDATE`, `UNCHANGED`, raw Docker Compose output, `ok:` verification lines, service status, curl output, and expected negative permission-test stderr such as `Permission denied`.

The goal is to keep all checks and detail available while making normal operator output clear, structured, and readable.

## Scope

Primary scripts:

- `deploy/systemd/setup-home-node.sh`
- `deploy/systemd/verify-home-node.sh`
- `deploy/systemd/install-home-node-core.sh`

Review adjacent helpers only where their output materially affects the main setup UX.

## Requirements

### 1. Structured sections

Use clear sections, for example:

```text
Jarvis Home Node Setup — v0.0.8

[1/8] Preparing host
      ✓ Service identity ready
      ✓ Jarvis directories ready

[2/8] SurrealDB
      ✓ Container healthy
      ✓ Scoped Core credentials retained

[3/8] Public release
      ✓ v0.0.8 checksum verified
      ✓ Release staged

[4/8] Protected configuration
      ✓ Persona installed
      ✓ Agent bundle active — 16 agents

[5/8] Services
      ✓ SurrealDB active
      ✓ Jarvis Core active

[6/8] Health
      ✓ /livez  200
      ✓ /readyz 200

[7/8] Security checks
      ✓ Core runs as jarvis
      ✓ Persona read-only
      ✓ Agent bundle read-only
      ✓ Root secrets unreadable
      ✓ Ports 8000/8080 loopback-only

[8/8] Complete
      ✓ Home Node ready
```

Exact wording can follow existing project terminology.

### 2. Color, but safely

When stdout is a TTY and color is supported, use restrained ANSI colors:

- green: success
- yellow: warning/unchanged/degraded
- cyan/blue: section headings/progress
- red: errors
- dim/gray: optional detail

Honor `NO_COLOR` and disable color when output is not a TTY.

Never put secrets into colored/debug output.

### 3. Preserve non-interactive behavior

Scripts are used by automation and system administration.

Do not:

- require an interactive terminal
- change exit codes
- hide failures
- make CI depend on ANSI sequences
- break piping/redirection

Plain non-TTY output must remain readable.

### 4. Quiet noisy expected output

Normal successful setup should not dump unnecessary Docker Compose progress or expected permission-denied stderr.

For example, expected negative security probes such as attempting to create a file in a read-only bundle should report:

```text
✓ Active agent bundle is not writable by jarvis
```

rather than printing the expected `Permission denied` first.

Suppress only output whose failure is explicitly expected and checked. Unexpected failures must remain visible.

### 5. Better Docker output

For setup helpers that invoke Docker Compose, prefer concise status in normal mode.

Provide a verbose/debug mode that exposes full underlying command output when troubleshooting.

Possible interface:

```text
JARVIS_VERBOSE=1 sudo ./deploy/systemd/setup-home-node.sh ...
```

or a deliberate `--verbose` flag if that integrates more cleanly.

Do not make debugging harder.

### 6. Health-check startup messaging

`install-home-node-core.sh` already gained bounded diagnostics after the v0.0.7 startup issue. Present startup progress clearly:

```text
Starting Jarvis Core …
✓ Jarvis Core active
✓ /livez ready
✓ /readyz ready
```

On failure, print concise service status and recent relevant journal lines, then exit non-zero.

### 7. Verification summary

`verify-home-node.sh` currently prints many individual `ok:` lines. Keep every check, but group them by category:

- identities/permissions
- services
- network exposure
- health
- protected inputs
- secrets
- optional services

At the end print a compact summary such as:

```text
Security verification: 27 passed, 0 failed
Home Node verification PASSED
```

Do not skip checks merely to shorten output.

### 8. Status vocabulary

Replace dense machine-like labels in the default UI where appropriate:

- `CREATE` -> `Created`
- `UPDATE` -> `Updated`
- `UNCHANGED` -> `Unchanged`
- `VERIFY` -> `Verifying`

Internal helper APIs may retain stable status enum/string values if tests rely on them; presentation can map them to clearer text.

### 9. First-owner bootstrap secret

The one-time bootstrap secret is security-sensitive and must still be shown exactly when required.

Present it prominently but safely, e.g.:

```text
IMPORTANT — FIRST OWNER BOOTSTRAP SECRET
Store this now. It will not be shown again.

<secret>
```

Do not echo it in verbose logs, summaries, journald, or later verification output.

Do not change its generation/storage semantics.

### 10. Final operator summary

Successful setup should end with useful non-secret state:

```text
Jarvis Home Node ready
Release:       v0.0.8
Core:          active
SurrealDB:     healthy
Agents:        16
API:           http://127.0.0.1:8080
Public ingress: not configured
Updater timer: enabled
```

Do not expose passwords, tokens, private prompts, bootstrap secret, or root credentials.

### 11. Shared shell presentation helper

Avoid duplicating ANSI/status formatting across scripts.

Create a small trusted shell helper/library if appropriate, for example under `deploy/lib/`, containing functions conceptually like:

- `ui_heading`
- `ui_step`
- `ui_success`
- `ui_warning`
- `ui_error`
- `ui_detail`
- `ui_run` / verbose command wrapper

Keep it simple and auditable. Do not add a large shell framework dependency.

### 12. Tests

Add tests for presentation behavior where practical:

- setup remains non-interactive
- non-TTY mode contains no ANSI escape sequences
- `NO_COLOR` disables colors
- verbose mode preserves underlying diagnostics
- expected permission-denied probes do not leak noisy stderr in normal mode
- failures still print useful diagnostics and return non-zero
- bootstrap secret is not repeated in final summaries
- existing deployment/security assertions remain intact

Do not weaken existing deployment tests to accommodate pretty output.

### 13. Security invariants

This is a presentation/operations UX change only.

Do not change:

- file ownership/modes
- service users
- systemd hardening
- loopback-only network binding
- SurrealDB credentials
- AgentRegistry permissions
- release checksum verification
- updater trust model
- bootstrap authentication model
- public/private repo boundary

## Definition of done

Complete when:

- successful Home Node setup is easy to scan visually
- setup/verification have clear sections and progress
- TTY output uses restrained optional color
- `NO_COLOR` and non-TTY output are clean
- expected negative probes no longer print confusing raw errors
- verbose troubleshooting remains available
- failures remain explicit and non-zero
- final summary shows useful non-secret system state
- all existing security/deployment tests still pass
- new presentation tests pass
- no security boundary or provisioning semantics changed
