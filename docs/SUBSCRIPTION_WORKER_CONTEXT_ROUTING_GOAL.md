# Goal: subscription workers, bounded context routing and owner-only provider authentication

Implement the next routing/authentication layer for PersonalJarvis on CURRENT `main`.

This goal extends, and must not weaken:

- `docs/CODEX_OPENSANDBOX_CONTRACT.md`
- `docs/OWNER_BRAIN_CODEX_AGENT_SYNC_GOAL.md`
- `docs/MODEL_ACCESS_AND_BUDGET_CONTROL_GOAL.md`
- `docs/INTELLIGENT_MODEL_ROUTER_GOAL.md`
- `docs/PRIVATE_AGENT_RUNTIME_GOAL.md`
- the current Laya/Jev System-1 intent routing
- the existing Core Admin/polkit trusted administration boundary

## Product outcome

Jarvis should route work through the cheapest reliable compute class while keeping account lifecycle under direct owner control.

Conceptually:

```text
user
  ↓
Jarvis Core
  ↓
Laya/Jev advisory intent classification
  ↓
bounded Context Compiler
  ↓
Core policy/router
  ├─ local compute
  ├─ owner-linked subscription worker
  └─ owner-enabled metered API
  ↓
agent/tool/runtime
```

The owner may link Claude and Codex subscriptions from **Jarvis Core Admin**. Jarvis chat, agents, prompts, MCP servers and model output may use an already-linked runtime but must never be able to connect, reconnect, disconnect, log out, switch accounts or export credentials.

## 1. Compute classes

Represent at least:

```text
local
subscription
metered_api
```

Examples:

- Laya / local Ollama → local
- Claude Code / Claude CLI using the owner's subscription → subscription
- Codex using the owner's ChatGPT/Codex entitlement → subscription
- OpenAI/Anthropic/DeepSeek/etc API → metered_api

Do not pretend unknown billing is free. If a runtime unexpectedly authenticates as PAYG/API, classify it correctly.

Subscription-backed calls do not consume the metered API EUR ledger, but may record non-monetary telemetry such as model, calls/runs, tokens if exposed, latency, plan-limit errors and availability.

## 2. Owner-only account linking belongs in Core Admin

Add/evolve a Core Admin `AI Accounts` view for Claude and Codex.

Core Admin may expose only non-secret status:

- connected / disconnected / unavailable / plan limit;
- worker identity;
- billing class;
- runtime version;
- last successful health test;
- optional safe masked account hint if the official client exposes one.

The normal Jarvis app may show read-only connection state and direct the owner to Core Admin, but must not start or complete login.

Account mutation operations are owner administration only:

```text
connect
reconnect
disconnect
logout
switch_account
write/export_credentials
```

They MUST NOT exist in the Jarvis agent/tool/MCP capability surface.

## 3. Dedicated worker identities

Use dedicated system identities rather than the generic Core user:

```text
jarvis          → Core
jarvis-claude   → Claude subscription worker
jarvis-codex    → Codex worker/broker
```

Requirements:

- no interactive shell;
- no sudo;
- no Docker group;
- no access to the owner's home, SSH, GitHub credentials or private agent source;
- no cross-reading of the other worker's auth store;
- minimal state directories;
- provider auth files mode/ownership hardened and never logged.

Do not copy the owner's complete `~/.claude`, `~/.codex`, browser profile or home directory.

## 4. Official provider login only

Core Admin initiates a typed trusted admin action, then the official installed provider client performs its own supported login flow under the dedicated worker identity.

Do not build a token-paste field in Vue/Tauri. Do not reimplement or reverse-engineer OAuth endpoints.

The provider token must never enter:

- Tauri frontend state or events;
- Jarvis API responses;
- SurrealDB memory;
- chat/agent context;
- command-line arguments;
- logs/audit text;
- OpenSandbox artifacts.

Where a provider flow uses browser authorization, PKCE, device code or loopback callback, preserve the official flow.

## 5. Claude subscription worker

Migrate the current direct `claude -p` Core path toward a local bounded worker/broker under `jarvis-claude`.

The worker may support runtime operations conceptually equivalent to:

```text
claude.status
claude.run
claude.cancel
```

It accepts typed/bounded task input only. No arbitrary executable, cwd, shell or environment fields.

For the subscription-only route, do not provide `ANTHROPIC_API_KEY` or another credential that silently changes billing class. If the runtime is logged out, exhausted or PAYG-only, return structured status instead of automatically accepting paid usage.

For pure brain/chat use, keep the worker in a neutral workspace with tools disabled/restricted according to the installed official CLI capabilities.

## 6. Codex subscription worker

Preserve the existing production path:

```text
Jarvis Core
→ signed owner-approved coding request
→ Codex broker
→ OpenSandbox Codex profile
→ bounded result
```

Use official Codex/ChatGPT subscription authentication where supported by the installed current Codex runtime. Do not turn this feature into an OpenAI Platform API-key shortcut.

Runtime operations may be conceptually:

```text
codex.status
codex.start
codex.resume
codex.cancel
```

Account login/reconnect/logout is not a runtime operation.

Do not mount the persistent Codex auth directory into disposable coding sandboxes. Keep task-scoped broker capability as required by `CODEX_OPENSANDBOX_CONTRACT.md`.

If the existing production activation gate is still incomplete, finish the narrow broker/task-proxy mechanism. Never fall back to a host `codex` process.

## 7. No silent paid fallback

Default invariant:

```text
subscription unavailable/exhausted
!=
silently enable PAYG/API billing
```

Return a structured failure/availability reason. The router may consider another independently owner-enabled route, such as local compute or an already-enabled metered API model subject to normal budget policy.

A subscription worker failure never grants new provider/model permission.

## 8. Context Compiler

Do not send the complete historical Jarvis chat to Claude, Codex or an agent on every run.

Add/reuse a bounded context compiler producing a factual envelope such as:

```text
TaskEnvelope
- objective / intent
- owner constraints
- relevant recent turns/deltas
- selected memory facts + provenance
- agent id/version
- repo/file/source references
- latest factual checkpoint
- prior logical session/run ref
- context/output/time budget
```

Prefer references and worker-side source/repository reading over large duplicated raw text.

Persist concise factual checkpoints, decisions, tests/results, blockers and artifact refs. Never persist hidden chain-of-thought.

## 9. Resume and long tasks

Logical sessions can persist while actual workers/sandboxes remain disposable.

Resume reconstructs from:

1. original objective;
2. latest trusted checkpoint;
3. current repository/source state;
4. relevant recent owner delta;
5. current owner constraints.

Do not replay the complete conversation merely because a session is long.

## 10. Laya/Jev and router integration

Laya/Jev remain advisory System-1 classifiers. They may help identify task/work kind, but do not choose a concrete provider, billing class, capability or account.

Routing order should remain conceptually:

```text
understand/classify
→ minimum capability/quality
→ compile bounded context
→ owner allowlist/policy
→ privacy + health + availability
→ qualifying local/subscription route
→ metered API budget evaluation if needed
→ approval/reservation where needed
→ run + checkpoint + verify
```

Cost optimization happens after minimum quality/safety determination.

## 11. Agent integration

Private agents continue to request provider-neutral model policies (`fast`, `default`, `strong/coding/trading`, `research`).

An agent may not specify or mutate:

- provider credentials;
- provider account;
- exact billing mode bypass;
- account lifecycle;
- budget override.

Core stays authoritative.

## 12. Core Admin security boundary

Reuse the existing unprivileged Tauri + fixed typed Rust command + polkit/root-broker design.

Do not give the frontend a shell/process/filesystem primitive.

Provider setup actions must be fixed typed operations. Secrets and provider tokens are never returned through the privileged broker protocol.

Account connect/reconnect/disconnect should require explicit owner interaction and be audited with non-secret metadata only.

## 13. Required tests

Add tests covering at least:

1. Core can distinguish local/subscription/metered API compute.
2. Jarvis/agent runtime has no account connect/reconnect/disconnect capability.
3. Tauri frontend never receives provider access/refresh tokens.
4. provider token strings do not appear in logs or structured status.
5. Claude subscription worker runs as `jarvis-claude`, not `jarvis`.
6. Claude subscription route does not inherit `ANTHROPIC_API_KEY`.
7. plan-limit state cannot silently opt into PAYG.
8. Codex subscription auth remains outside disposable OpenSandbox workloads.
9. Codex runtime does not fall back to a host process.
10. worker request protocol has no arbitrary command/cwd/env field.
11. one worker cannot read the other worker's auth store.
12. Core cannot read/export worker credentials.
13. normal Jarvis app is read-only for account state.
14. Core Admin account actions require the trusted owner-admin boundary.
15. failed/replayed account setup sessions fail closed.
16. full conversation history is not required for a worker continuation.
17. checkpoint + delta resume reconstructs bounded factual context.
18. hidden chain-of-thought is never stored.
19. subscription failure does not auto-enable a disabled API model.
20. existing model allowlist, budget, OpenSandbox and protected-path tests remain green.

## Definition of done

The feature is complete only when an owner can deliberately connect Claude/Codex from Core Admin, normal Jarvis can safely use the already-connected subscription worker, Jarvis cannot alter that account connection, subscription failure cannot silently create API spend, and long agent/coding tasks operate on bounded factual context rather than repeatedly replaying the owner's full chat history.
