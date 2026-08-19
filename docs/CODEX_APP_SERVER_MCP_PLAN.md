# Codex App Server + MCP Integration Plan

## Purpose

This document defines how PersonalJarvis should integrate Codex through Jarvis Core on the Ubuntu Home Node.

Jarvis Core remains the authoritative brain and security boundary. Codex is a replaceable engineering/agent runtime that Core may invoke for bounded tasks. MCP is used for narrowly scoped external tools, not as a replacement for Jarvis' internal Rust architecture.

Before implementation, inspect `AGENTS.md`, current ADRs, security docs, the agent runtime, MCP code, tool registry, OpenAI integration and Home Node deployment files. Do not duplicate abstractions that already exist.

## Target architecture

```text
Jarvis App
    |
    | HTTPS/WSS
    v
Jarvis API
    |
    v
Jarvis Core
    |
    +-- conversation/context/memory
    +-- orchestrator
    +-- jarvis-policy
    +-- tool/capability registry
    +-- usage/cost controls
    +-- trading/risk boundaries
    |
    +-----------------------------+
    |                             |
    v                             v
Native Jarvis agents         Codex Adapter
                                  |
                                  | local structured IPC / JSON-RPC
                                  v
                           Codex App Server
                                  |
                                  +-- coding/engineering agent loop
                                  +-- isolated worktrees
                                  +-- tests/reviews
                                  +-- progress events
                                  +-- MCP client
                                          |
                                          v
                                  scoped Jarvis MCP tools
```

Core decides whether Codex is needed, creates the task, grants capabilities, sets workspace/time/resource limits, audits the run and receives the result. Codex decides how to perform the engineering task inside that scope.

## Use Codex App Server, not shell glue

Prefer the current supported Codex App Server / structured integration mechanism at implementation time rather than treating the CLI as a text subprocess.

Do not make the primary integration simply:

```rust
Command::new("codex").arg("...")
```

unless current official integration options make that genuinely necessary.

A structured adapter is needed for task IDs, lifecycle, progress events, cancellation, timeouts, errors and future UI integration. Keep Codex-specific protocol details isolated in a dedicated crate/module, for example `crates/codex`, if no suitable existing abstraction exists.

## OpenAI-first, provider-neutral Core

OpenAI may be the only active hosted provider for v1. Do not hard-code Jarvis around OpenAI.

Core should depend on provider-neutral concepts such as model capabilities, requests/responses, reasoning tier, tools, usage and cost. Future xAI/Grok support should be addable without changing trading, memory, policy or MCP architecture.

Treat normal inference and Codex as different capabilities:

```text
OpenAI API       -> normal Jarvis chat/reasoning/agent planning
Codex App Server -> engineering and long-horizon repository tasks
```

Do not route ordinary chat through Codex by default.

## Core owns routing

The Jarvis App sends a normal message to Jarvis. It should not select an agent/runtime itself.

```text
message -> Jarvis API -> Jarvis Core
                         |
                         +-- normal question -> normal agent/model
                         +-- trading question -> trading orchestrator
                         +-- system question -> bounded system agent
                         +-- coding task -> Codex engineering task
```

Core should create a typed engineering task rather than only concatenating a free-form prompt.

## Codex must not mutate the live Core

Preserve the current protected boundaries. Jarvis/Codex may inspect authorized source, but production/self-development must use an isolated worktree/workspace and normal review/release flow.

```text
running release
    -> isolated development worktree
    -> Codex changes/tests/review
    -> branch / PR
    -> reviewed tag/release
    -> Home Node updater
```

Codex must never directly replace `/opt/jarvis/current`, overwrite the running binary, change production secrets, bypass protected `core/**` or `.git/**` restrictions, disable policy, grant itself permissions or deploy generated code directly into production.

## MCP is an external tool boundary

Do not turn all internal Rust functions into MCP. Keep policy, auth, memory, risk, audit and orchestration native Rust where that is the natural boundary.

Use MCP for isolated/discoverable tools and external applications/services.

Potential tools:

### Market data MCP

Read-only first: prices, candles, spread, sessions, symbol metadata and later authorized futures/Level 2 feeds.

### MT5 MCP

Read-oriented initial surface: account info, positions, orders, trade history, symbols and backtest/EA state.

Never expose unrestricted direct order execution to an LLM/Codex tool.

### Backtest MCP

Start bounded backtests, inspect status/results, compare strategy versions and perform walk-forward/out-of-sample evaluation where supported.

### System MCP

Read-only first: CPU, RAM, disk, temperature, bounded logs, service state and Home Node health. Mutating system actions must be separate policy-gated capabilities, not a generic `run_command` tool.

### Research MCP

Approved news/data/research retrieval with evidence/source metadata and timestamps.

## Task-scoped capabilities

Codex must not receive every MCP server/tool simply because it exists. Core creates a scoped capability grant for each task.

Example for investigating an MT5 strategy regression:

```text
Allowed:
- isolated repo worktree
- read trading performance
- market-data read tools
- backtest start/read

Denied:
- live order execution
- production secrets
- arbitrary host shell
- production deploy
- policy/security modification
```

Enforce this in code. Prompt instructions alone are not authorization.

If Codex connects directly to an MCP server, use a scoped/ephemeral capability credential; never hand it a permanent master credential.

## Hard trading boundary

Never implement:

```text
Codex/GPT -> MCP -> mt5.buy(...)
```

The allowed architecture is:

```text
AI / trading agent
    -> typed TradeIntent
    -> jarvis-policy
    -> deterministic Risk Engine
    -> account/prop-firm rules
    -> Execution Gateway
    -> MT5 / EA
```

The deterministic layer owns max risk, daily loss/drawdown, exposure, spread/slippage, session/news gates, account-specific prop rules and kill switches. LLM confidence never overrides these controls.

## Trading engineering use case

Codex can help Jarvis develop and improve EAs/strategy code without direct live deployment.

```text
performance issue
    -> Jarvis evidence investigation
    -> bounded Codex EngineeringTask
    -> isolated MQL5/Rust worktree
    -> candidate changes
    -> backtests via MCP
    -> walk-forward/out-of-sample
    -> compare with baseline
    -> PR / candidate artifact
```

Live progression must remain separate: development -> backtest -> out-of-sample -> shadow/paper -> review -> approved release -> controlled live deployment.

## Authentication and ChatGPT subscription

Use the official Codex authentication mechanism available at implementation time, including ChatGPT-account sign-in where supported. Do not scrape browser sessions or copy ChatGPT cookies/tokens into Jarvis.

Codex auth state is a sensitive host credential. It must not be exposed to prompts, MCP, logs, Docker images or git. Run Codex under an unprivileged dedicated engineering/service identity where practical.

Jarvis must tolerate Codex being unavailable, logged out, rate-limited or temporarily exhausted. Codex failure must never stop normal Jarvis Core operation.

## Home Node process isolation

Conceptually:

```text
systemd
+-- jarvis-core.service          authoritative runtime
+-- codex-app-server.service    if persistent lifecycle is justified
+-- narrow MCP services/workers
```

A persistent Codex server is not mandatory. If on-demand start/stop is safer, Core may request it through a narrow supervisor. Do not give Jarvis Core blanket sudo or Docker-socket access to manage it.

Prefer local IPC (Unix socket, loopback-only endpoint or supported stdio protocol). Never expose Codex App Server publicly.

## Task lifecycle

Codex work must be first-class asynchronous tasks, not one blocking call.

Suggested states:

```text
QUEUED
STARTING
RUNNING
WAITING_FOR_TOOL
WAITING_FOR_APPROVAL
CANCELLING
COMPLETED
FAILED
TIMED_OUT
```

Each task should track a task ID, origin, workspace, capability grant, timeout/deadline, cancellation token, safe progress events, outcome, usage/cost where available and audit linkage.

## Realtime progress to Jarvis App

Long Codex tasks should use the Jarvis realtime event channel. The app may show safe summaries such as:

```text
I'm inspecting the strategy implementation.
The backtest is still running.
I found a regression and I'm testing a candidate fix.
```

Do not expose hidden chain-of-thought, raw internal reasoning, secrets, environment variables or sensitive tool payloads.

## Cancellation, time and resources

Jarvis must be able to cancel Codex tasks and associated MCP/backtest workers. Add wall-clock deadlines and explicit timestamps; do not rely on a model's internal sense of time.

Because the UM890 also runs Jarvis, SurrealDB, GUI/trading apps and remote access, enforce resource-aware limits: maximum concurrent Codex tasks, queue size, timeouts, disk/worktree quotas and backtest concurrency. Interactive/trading/risk functions should outrank background engineering work.

## Audit

Audit control-plane/security events such as task requested/started/stopped, capability grant, approval request/result, protected-path denial, MCP authorization denial and final outcome. Do not log complete prompts/source/tool payloads by default when they may contain sensitive data.

## Failure behavior

If Codex is missing, logged out, rate-limited, crashes, hangs or an MCP call fails, fail only the task. Core remains healthy. Return a clear result and avoid infinite retries of expensive or privileged work.

## MCP security requirements

Every MCP server must have narrow typed inputs/outputs, capability enforcement, timeouts/cancellation, safe errors, audit hooks and tests for denied operations. Do not expose environment dumps, secrets, unrestricted filesystems or generic arbitrary shell execution.

An MCP tool description is not an access-control system.

## Tool Registry integration

Normalize native and MCP tools through the existing tool registry. Risk/policy classification belongs to `jarvis-policy`, not duplicated inside Codex adapters.

Expected decision path:

```text
Codex requests tool
    -> scoped tool gateway / MCP server
    -> jarvis-policy / capability grant
    -> deny | allow | signed approval
    -> bounded execution
    -> audit + result
```

## Implementation phases

Implement incrementally, not as one giant PR.

### Phase 1: adapter foundation
- inspect current architecture and create/update ADR;
- structured Codex adapter;
- task lifecycle, timeout and cancellation;
- development-only connection to Codex App Server;
- mock protocol tests;
- no privileged tools.

### Phase 2: isolated engineering workspace
- safe worktree/workspace manager;
- protected path enforcement;
- bounded engineering process execution;
- PR-ready output;
- regression/security tests.

### Phase 3: scoped MCP/tool grants
- integrate tool registry;
- ephemeral capability grants;
- first read-only MCP tool/server;
- denial/audit tests.

### Phase 4: realtime app events
- task progress events through existing API realtime design;
- cancellation from client;
- safe task history/status UX.

### Phase 5: trading engineering
- read-only trading performance tools;
- backtest MCP;
- EA/strategy development workflow;
- no live trading execution.

### Phase 6: broader tools/providers when justified
- market/research/system additions;
- future xAI/Grok provider support without changing the Core architecture.

## Home Node documentation

When implemented, update Home Node docs with Codex installation, account authorization, Linux service identity, App Server lifecycle, IPC endpoint, logs, updates, health checks, disable/uninstall steps and what happens when authentication expires.

Keep Codex credentials separate from ordinary Jarvis configuration unless a documented security design requires otherwise.

## Testing

Add deterministic automated tests for adapter parsing, task state transitions, cancellation, timeout, unavailable/malformed Codex server, capability enforcement, denied MCP tools, protected paths, audit events and Core health when Codex fails.

Tests requiring a real personal ChatGPT login must remain optional and outside mandatory CI.

Run normal quality gates for implementation PRs:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo audit
```

## Definition of done for first production-capable integration

The first integration is complete when:

- Jarvis Core can create a bounded Codex engineering task through a structured adapter;
- Codex runs unprivileged and failure cannot take down Core;
- task timeout/cancellation works;
- work happens only in explicitly allowed development workspaces;
- production/Core protections remain enforced;
- Codex receives only task-scoped tool capabilities;
- at least one MCP integration demonstrates real scoped tool use;
- authorization is enforced in code, not prompts;
- safe structured progress events exist;
- audit/security events exist;
- no direct live-trading execution path is exposed;
- Home Node deployment/recovery is documented;
- mandatory CI does not depend on personal ChatGPT credentials.

## Instructions to the implementing agent

Before coding, read `AGENTS.md`, security docs, ADRs and Home Node docs, then inspect current agent/MCP/OpenAI/tool registry code. Verify current official Codex integration details instead of assuming this plan's protocol names remain exact. Produce a gap analysis and split the work into small dependency-ordered PRs.

Do not rewrite Core around Codex, introduce unrestricted shell MCPs, expose Codex publicly, add direct live-trading tools, weaken signed approvals/policy, grant root/Docker socket/blanket sudo, mutate the running production release or commit personal ChatGPT credentials.

The desired end state is simple:

> Jarvis Core remains the brain and authority. Codex becomes a powerful, replaceable and tightly scoped engineering agent that Jarvis can invoke through a structured local integration and equip with narrowly authorized MCP/native tools.
