# PersonalJarvis — Agent Guide

## Purpose and operating model

PersonalJarvis is a security-first personal AI operating system. It combines a Rust backend, device-bound authentication, agent tooling, local and hosted LLM routing, personal data services, and future trading/research capabilities.

The repository is the source of truth. This guide directs AI agents; it does not replace the architecture, ADRs, security standards, backlog, or executable tests.

Before changing code, read:

- `README.md` for the product map, local development, and current module map.
- `STATUS.md`, `TODOS.md`, and `STEPS.md` for active work and sequencing.
- Relevant ADRs in `decisions/`.
- `docs/AI_SECURITY_HARDENING.md` for mandatory runtime security boundaries.
- `docs/AI_REVIEW_IMPROVEMENTS_2026-08-15.md` for reviewed priorities.
- `docs/AI_HOME_NODE_GUI_PLAN.md` and `decisions/ADR-033-HOME-NODE-DEPLOYMENT.md` before any Home Node, GUI, remote-access, or deployment work.
- The security documents named in the README's “Mandatory code-agent security baseline”.

Inspect the actual code, tests, deployment files, and current branch history before proposing a change. Never guess an API, policy, protocol, secret location, service topology, or RivetLink capability. Do not implement unrelated work, “cleanup”, or speculative infrastructure.

## Current architecture

- The backend is a Rust workspace. `services/api` is the Axum API; domain responsibilities live in `crates/*`.
- The API is modular: `services/api/src/lib.rs` is the composition root; state, extraction, errors, rate limiting, metering, audit, validation, MCP, and route concerns are separate modules. Preserve that ownership and do not recreate a monolithic `lib.rs`.
- PostgreSQL is the persistent system of record; pgvector is planned/used for the memory platform. Local supporting services run through `deploy/compose/docker-compose.yml`.
- The macOS/iOS client is under `apps/client`. Server-side API keys remain in the backend environment; they never belong in clients, prompts, browser/UI automation, commits, logs, or tests.
- Agent execution is deliberately constrained. The typed agent action allowlist, sandbox, timeout/output limits, kill switch, audit trail, and signed-approval flow are security boundaries, not convenience features.

## Policy, approvals, and protected boundaries

`jarvis-policy` is the single source of truth for capability and risk decisions. Callers may adapt policy types at crate boundaries, but must not create parallel risk enums, duplicate allow/deny logic, or bypass policy in an executor, GUI tool, Claude Code path, MCP server, or trading integration.

Use the intended decision path:

```text
request / LLM → jarvis-policy → capability + risk decision
              → deny or signed approval → bounded executor → audit
```

- Read-only and mutating actions must remain distinct.
- High-risk or mutating actions require a real, device-signed, action-bound, unexpired approval. A boolean `approved` flag is never sufficient.
- Approval verification immediately precedes execution; changed arguments, expiry, or nonce replay must fail closed.
- No LLM response may directly execute arbitrary shell commands, sudo, host control, or unrestricted child processes.
- The agent sandbox must prevent workspace escape through absolute paths, traversal, and symlinks.
- Secrets are denied even for reads: do not expose `.env*`, keys, certificates, SSH material, credentials, or inherited full process environments.
- `core/**` and `.git/**` are protected from agent writes. Do not weaken these protections, including through Claude Code or GUI automation.
- Keep kill switches default-off where currently designed, enforce resource/time/output bounds, use generic external errors, and audit privileged activity without logging secrets or sensitive prompt/UI data.

Security failures fail closed. Do not relax a check merely to pass a test or accelerate a feature.

## Home Node and deployment boundary

The intended Home Node is a UM890 Pro running Ubuntu Desktop LTS. Desktop is intentional: the node must support a persistent GUI session for TradingView, MT5/Wine where needed, browsers, and controlled computer-use, while operating without a physical monitor after setup.

Target shape:

```text
private user devices
  → RivetLink / private network
  → Ubuntu Desktop Home Node
      ├─ Jarvis Core: native Rust process managed by systemd
      ├─ Docker: PostgreSQL + pgvector and justified supporting services
      └─ non-root GUI session: TradingView, MT5/Wine, browser, Cua
```

- Jarvis Core stays native under systemd; do not move it into the ordinary Docker stack. It must remain independently recoverable if Docker or a service fails.
- Docker services use pinned images and persistent volumes. Do not add Redis, Kubernetes, public reverse proxies, or new platform infrastructure unless a demonstrated workload requires them.
- SSH, RDP/VNC, PostgreSQL, Redis, Docker socket, and privileged APIs must not be public. Remote administration and GUI access use RivetLink/private networking with explicit device authorization, encryption, revocation, and auditable access.
- Inspect RivetLink's existing protocol and security model before integrating it or introducing a duplicate tunnel/VPN.
- GUI applications run as non-root users with isolated data/configuration where feasible. A disconnected viewer must not terminate critical applications.
- Document and test the supported GUI/remote-display approach, including X11/Wayland implications, before relying on it for automation.

## Cua, GUI, and trading boundaries

Computer-use/Cua is a privileged capability, never unrestricted desktop, filesystem, shell, root, or credential access.

```text
Jarvis agent → ComputerUse capability → policy check
             → approved app/session allowlist → Cua → GUI
```

Every GUI action needs bounded scope, timeout/cancellation, audit, kill switch, and an explicit human-takeover path through RivetLink. Treat screenshots and UI state as potentially sensitive; retain only what is necessary.

TradingView and MT5 are UI tools, not the authoritative trading engine. GUI automation must never bypass the deterministic policy/risk/execution gateway, signed approvals, or live-trading gates. Never enable live trading from development work.

## CI and verification

Run the same backend checks as CI before opening a code PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo audit
```

Integration tests need PostgreSQL and `DATABASE_URL`; use the compose setup and command documented in `README.md`. For a formatting failure, run `cargo fmt --all`, inspect the resulting diff, and commit only intentional formatting changes.

The CI workflow runs format → clippy → tests → audit, so a format failure prevents later checks from running. The last reviewed main run at commit `1055e19` was reported as format-only; verify the latest GitHub Actions result before repeating that conclusion or changing CI. Do not alter the workflow merely to mask a code formatting failure.

## Workflow for changes

1. Read the documents above and inspect the relevant code/tests first.
2. Select one narrow, dependency-ready task. Mark project tracking documents in progress when the repository convention requires it.
3. Design within existing boundaries. Record a material architecture decision as an ADR before or alongside implementation.
4. Implement the smallest coherent change; avoid drive-by refactors and unrelated files.
5. Add focused tests: integration tests for security boundaries, unit tests for pure policy/validation logic, plus operational verification for deployment work.
6. Run all applicable checks, review the diff, and update `STATUS.md`, `TODOS.md`, changelog, and documentation when required by repository convention.
7. Create a small, independently reviewable PR. State scope, verification, security impact, rollback/recovery implications, and deferred work. One PR should not attempt API refactoring, policy integration, Home Node deployment, and Cua integration together.

Do not claim a task is complete without reproducible evidence.

## Current priorities

Work incrementally in this order, confirming the current state before starting:

1. Preserve the completed API modularization and keep route/security ownership clear.
2. Complete/verify `jarvis-policy` integration so it remains the authoritative runtime decision path, with cross-caller regression tests.
3. Maintain the trusted-proxy/client-IP model: never trust spoofed forwarding headers; cover direct, trusted-proxy, and untrusted-proxy cases.
4. Build a typed tool/capability registry foundation before broadening agent execution.
5. Establish the Home Node baseline: systemd Core, Docker PostgreSQL/pgvector with persistence, private access, health checks, and recovery.
6. Introduce RivetLink and Cua only after their capability/policy, allowlist, audit, kill-switch, human-takeover, and GUI-session boundaries are proven.
7. Extend security regression coverage for sandbox escapes, secret/Core/`.git` denials, signed approval/replay/expiry, audits, rate limits, input bounds, forwarding headers, and policy consistency.
8. Only then expand agent runtime, data integrations, trading, backtesting, and other backlog features.

When requirements are unclear or authority is missing, stop and ask rather than inventing infrastructure or weakening the security model.
