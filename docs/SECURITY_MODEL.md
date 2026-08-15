# Jarvis Security & Capability Model

This document defines the security boundary for the future Jarvis Core, Home Node, agents, tools, and trading integrations.

## Principles

1. **Least privilege:** an agent receives only the capabilities required for its task.
2. **Read and write are separate:** broad read access does not imply write or execution access.
3. **No arbitrary root shell:** LLM output must never be treated as an unrestricted shell command.
4. **Deterministic policy:** high-impact actions are checked by code, not by model instructions alone.
5. **Research before assumptions:** current or uncertain facts should be verified with an appropriate tool before being presented as fact.
6. **Auditability:** consequential tool calls should record actor, device, task, capability, target, timestamp, outcome, and risk class.
7. **Fail closed:** authentication, policy, database schema, or safety checks failing must prevent consequential actions.
8. **Secrets stay outside model context:** API keys, private keys, session tokens, and credentials must not be passed to LLM prompts unless a dedicated integration explicitly requires it.

## Capability classes

| Capability | Default | Notes |
|---|---|---|
| Read system health | allow | CPU, RAM, disk, service status |
| Read logs | allow with scope | Avoid unrestricted secret-bearing logs |
| Read application data | allow with scope | Respect user/device authorization |
| Restart low-risk service | restricted | Allowlist only |
| Restart database | approval/policy | Investigate health first |
| Install/update software | restricted | Verified source + change plan |
| Modify repository | agent workspace only | Prefer isolated worktree |
| Deploy code | policy | Tests + review + health check |
| Submit trading order | highly restricted | Risk engine + execution gateway |
| Withdraw/transfer funds | forbidden to general agents | Explicit separate security boundary |
| Arbitrary root command | forbidden | No generic `sudo` tool |

## Tool architecture

The internal Jarvis Tool Registry should be the source of truth for capabilities. MCP can expose selected capabilities to external agents such as Claude Code or Codex, but MCP should not replace the internal typed Rust capability layer.

Recommended flow:

```text
Agent / Model
    -> Jarvis Core
    -> Policy Engine
    -> Tool Registry
    -> capability implementation
    -> external system
```

## Coding agents

Claude Code and Codex should operate in isolated task workspaces/worktrees. A coding agent should not receive unrestricted access to the host filesystem or production credentials.

A coding task should follow:

```text
request
 -> architecture/research
 -> isolated worktree
 -> implementation
 -> tests
 -> reviewer
 -> security review when applicable
 -> deploy policy
 -> health check
 -> rollback on failure
```

## Trading boundary

Trading agents must never call an exchange/broker execution API directly. They produce a structured trade proposal. A deterministic Rust risk engine validates limits, instrument permissions, sizing, market/session state, and required approvals before an execution gateway can submit an order.

## Home Node

The UM890 Home Node should run Jarvis Core as a native systemd service. Stateful/supporting services such as PostgreSQL, pgvector, Redis, monitoring, and isolated workers should run in Docker. SSH should be available through a private network/VPN rather than exposing administration ports directly to the public internet.

## Required future controls

- endpoint-specific rate limiting for enrollment, challenge, and login
- explicit enrollment mode/secret
- request validation and bounded payloads
- capability-scoped authorization
- structured audit events
- secret redaction in logs and model context
- task timeouts and cancellation
- per-agent tool allowlists
- resource limits for background workers
- deployment rollback and health checks
