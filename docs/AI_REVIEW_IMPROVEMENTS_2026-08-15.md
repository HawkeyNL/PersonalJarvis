# AI Review Improvements — 2026-08-15

## Purpose

This document is a follow-up engineering brief based on a review of the current `main` branch after the recent security/agent work.

**Do not implement everything in one giant PR.** Treat each section as an independently reviewable change. Inspect the existing architecture and tests before modifying code. Preserve existing security guarantees and add regression tests for every behavior changed.

The goal is to improve maintainability and close the remaining gaps without adding speculative infrastructure.

---

## Priority 1 — Split the API crate before adding more features

### Problem

`services/api/src/lib.rs` has grown to roughly 121 KB. This makes ownership, review, testing, and future agent-generated changes unnecessarily difficult and increases the chance of accidental coupling.

### Required change

Refactor the API crate into cohesive modules without changing external behavior.

Suggested structure (adapt to the existing architecture rather than copying blindly):

```text
services/api/src/
  lib.rs
  state.rs
  error.rs
  routes/
    mod.rs
    auth.rs
    devices.rs
    agent.rs
    chat.rs
    portfolio.rs
    broker.rs
    system.rs
  middleware/
    mod.rs
    auth.rs
    rate_limit.rs
  dto/
    mod.rs
    auth.rs
    agent.rs
    chat.rs
    portfolio.rs
  audit.rs
```

### Constraints

- No behavior changes unless required to preserve correctness.
- Keep public API compatibility where practical.
- Avoid creating dozens of tiny modules with no meaningful ownership boundary.
- Keep route registration readable in `lib.rs`.
- Preserve all existing tests.
- Add/update module-level tests where useful.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets --all-features`, and the full test suite.

### Acceptance criteria

- `lib.rs` becomes a small composition/root module.
- Auth, agent, audit, rate limiting and other concerns have clear ownership.
- No duplicated business logic is introduced during the refactor.

---

## Priority 2 — Make `jarvis-policy` the single source of truth

### Problem

The repository now contains policy/risk concepts both in the agent implementation and in the `jarvis-policy` crate. The policy crate currently acts primarily as a foundation rather than the authoritative runtime decision point.

Duplicated policy logic will eventually drift.

### Required change

Make `jarvis-policy` the canonical policy layer for capability/risk decisions.

Desired direction:

```text
LLM / Agent request
        ↓
   jarvis-policy
        ↓
 capability + risk decision
        ↓
 approval requirement / denial
        ↓
 Tool Registry / Executor
```

### Requirements

- Define one canonical representation for capability/risk classification.
- Avoid maintaining parallel enums or subtly different classification logic in multiple crates.
- The agent executor must consume the policy decision rather than independently reimplementing it.
- Preserve the existing signed-approval requirements for mutating operations.
- Preserve the existing hard denials for Core, `.git`, secrets and sandbox escapes.
- Add tests proving that policy decisions are consistent across callers.
- Do not turn the policy crate into a dumping ground for execution code.

### Acceptance criteria

There is one authoritative policy decision path, with adapters only where crate boundaries require them.

---

## Priority 3 — Verify and harden client-IP handling behind RivetLink/proxies

### Problem

The in-process rate limiter is appropriate for the current single-node architecture, but the Home Node will eventually sit behind RivetLink and/or a reverse proxy.

A client-controlled `X-Forwarded-For` or similar header must never be blindly trusted for security decisions.

### Required change

Audit how the API determines the source IP for rate limiting, login lockout and audit events.

### Requirements

- Identify the actual trusted network/proxy boundary.
- Only trust forwarded client-IP headers from explicitly trusted proxy hops.
- If no trusted proxy is configured, use the actual socket peer address.
- Document the trust model.
- Ensure RivetLink integration does not accidentally collapse every client into one shared rate-limit bucket unless that is deliberate.
- Add tests for direct clients, trusted proxies and untrusted spoofed forwarding headers.
- Never use a client-supplied forwarding header as an authentication factor.

### Acceptance criteria

An attacker cannot bypass IP-based rate limits or pollute security attribution simply by sending a forged forwarding header.

---

## Priority 4 — Home Node deployment must preserve the security model

This is an implementation track rather than a request to add a new feature immediately.

When deploying the UM890 Home Node, follow the architecture in `docs/AI_HOME_NODE_GUI_PLAN.md` and the existing security hardening documentation.

### Required deployment properties

- Ubuntu Desktop LTS because the node requires GUI applications.
- Jarvis Core as a native `systemd` service.
- PostgreSQL/pgvector and supporting infrastructure in Docker where appropriate.
- Persistent database volumes.
- SSH available through the private/RivetLink path rather than public internet exposure.
- No public PostgreSQL, Redis, Docker socket, RDP or privileged control API.
- GUI applications must run as a non-root user.
- GUI session must be able to persist without a physical monitor.
- Cua/computer-use must operate through an explicit capability/policy boundary.
- Remote GUI access must not bypass Jarvis authorization or trading risk controls.
- Monitoring and health checks must cover Core, Docker, database, GUI and computer-use services.

### Important

Do not introduce Redis, Kubernetes, a public reverse proxy, or other infrastructure merely because it is common in server deployments. Add infrastructure only when the current workload requires it.

---

## Priority 5 — Separate computer-use from unrestricted host control

Before implementing Cua/RivetLink automation, enforce the same principle already used by the agent sandbox:

> GUI automation is a privileged capability, not unrestricted access to the machine.

### Requirements

- Explicit capability for GUI/computer-use.
- Target application/session allowlist.
- Policy check before actions.
- Action timeout and cancellation.
- Audit every privileged GUI action at an appropriate level without storing sensitive screenshots unnecessarily.
- Kill switch.
- Human takeover path through RivetLink.
- No bypass around trading Risk/Execution policy.
- No credential harvesting through GUI automation.

### Acceptance criteria

Jarvis can operate approved GUI applications, but Cua cannot become an alternative path to arbitrary shell/root/file-system access.

---

## Priority 6 — Preserve and extend security regression tests

Every future change to authentication, authorization, agent execution or remote access must include regression tests.

At minimum preserve coverage for:

- default-off agent kill switch;
- sandbox escape denial;
- secret-path denial;
- Core and `.git` write denial;
- signed approval verification;
- approval nonce replay denial;
- approval expiry;
- audit events;
- rate-limit behavior;
- login lockout;
- request-size/input bounds;
- spoofed forwarding headers;
- policy decision consistency.

Prefer integration tests for security boundaries and unit tests for pure policy/validation logic.

---

## Priority 7 — Do not expand scope until the foundations are clean

The repository has a large backlog covering agent runtime, MCP, market data, IBKR, MT5, risk/execution, backtesting and content automation.

Do not implement those features merely because they are listed in the backlog.

The preferred order is:

1. API modularization.
2. Single policy source of truth.
3. Authentication/rate-limit proxy trust hardening.
4. Tool/capability registry foundation.
5. Home Node deployment baseline.
6. Cua/RivetLink integration with policy boundaries.
7. Then expand agent runtime and trading capabilities incrementally.

Each feature should have a clear capability boundary, tests, observability and recovery behavior before being considered production-ready.

---

## Definition of done

This review is complete when:

- API modules have clear ownership and `lib.rs` is no longer a monolith.
- `jarvis-policy` is the canonical risk/capability decision layer.
- Forwarded client IPs are handled according to an explicit trusted-proxy model.
- Security regression tests cover the above boundaries.
- Home Node deployment follows the documented architecture without unnecessary infrastructure.
- Cua/RivetLink cannot bypass existing authorization or trading risk controls.

**Do not weaken existing protections to make implementation easier.**
