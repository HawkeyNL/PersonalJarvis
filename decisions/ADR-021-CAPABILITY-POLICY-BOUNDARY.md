# ADR-021: Capability and policy boundary for Jarvis tools

- Status: Proposed
- Date: 2026-08-15

## Context

Jarvis will eventually control a home node, databases, research tools, coding agents, market-data services and trading infrastructure. Giving an LLM arbitrary shell/root access would make prompt injection, model mistakes and compromised tools unnecessarily dangerous.

The existing architecture intentionally favors a modular monolith. This decision keeps that approach while defining a strict boundary between reasoning and execution.

## Decision

Jarvis Core owns a typed Tool Registry and Policy Engine.

Tools expose explicit capabilities such as:

- read system health
- inspect Docker service status
- read service logs
- query memory
- query market data
- create a coding task
- propose a trade

The model never receives unrestricted shell/root execution as a normal capability.

Each tool call is evaluated by policy using at least:

- authenticated user/device identity
- requested capability
- read vs write operation
- risk class
- reversibility
- target resource
- current environment (development/staging/production)
- whether explicit user approval is required

### Capability classes

| Class | Example | Default |
|---|---|---|
| Read-only | CPU/RAM, logs, positions | automatic |
| Low-risk write | restart a non-critical worker | policy-controlled |
| High-risk write | update services, modify infrastructure | explicit approval or tightly scoped automation |
| Financial execution | submit a live order | risk engine + execution policy + approval rules |
| Destructive | delete data/volumes/devices | explicit approval; never LLM-only |

## MCP usage

MCP is an adapter/protocol at the AI-tool boundary, not the internal architecture of Jarvis.

The internal Tool Registry remains native Rust with typed interfaces. MCP servers/adapters may expose selected capabilities to Claude Code, Codex or other compatible agents. An MCP server must not bypass the Policy Engine.

Conceptually:

```text
LLM / Agent
    |
    +-- MCP adapter (optional)
    |
    v
Jarvis Tool Registry
    |
    v
Policy Engine
    |
    +--> allowed --> typed service/tool
    |
    +--> denied --> audit event
```

## Trading boundary

Trading agents may research instruments and create trade proposals, but they do not call a generic broker `place_order` capability directly.

Live execution must pass through a deterministic risk/execution gateway that can enforce limits independently of the LLM.

## Home Node boundary

The Jarvis Core process may inspect infrastructure broadly, but privileged operations should be delegated to narrowly scoped services. For example, a future service-manager capability may restart an allowlisted service without giving the Core arbitrary `sudo` access.

## Consequences

### Positive

- limits blast radius from prompt injection and model mistakes
- makes permissions auditable and testable
- keeps MCP replaceable
- supports local and cloud models through the same tool interface
- creates a clean path to trading safety controls

### Negative

- more up-front Rust types and policy code
- some operations require explicit approval flows
- agents cannot simply execute arbitrary shell commands

## Implementation order

1. Define `Tool`/capability traits in a dedicated core crate/module.
2. Add `Task` and `Event` primitives so long-running work is observable.
3. Add a policy engine with deny-by-default behavior for unknown capabilities.
4. Add MCP adapters only for selected capabilities.
5. Add a dedicated trading risk/execution boundary before live order placement.
