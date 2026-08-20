# Language Agent Architecture

All agents obey `CODE_AGENT_CONSTITUTION.md`.

## Rust Agent
Axum, domain logic, risk, gateways and device agents.
Rules: deny unsafe by default, typed errors, no production unwraps, Decimal for money, timeouts, cancellation, backpressure and clippy warnings denied.

## TypeScript/Vue/Tauri Agent
UI, local cache and Observatory.
Rules: strict TypeScript, runtime response validation, no secrets/localStorage tokens, CSP, narrow Tauri capabilities, safe IPC and escaped external content.

## SQL/PostgreSQL Agent
Schemas and migrations.
Rules: parameterized queries, least-privilege roles, constraints, migration/backfill plans, encrypted/hashed sensitive data and no critical invariants hidden only in JSONB.

## Python Research Agent
Notebooks, evaluations and experiments.
Rules: no production path by default, pinned dependencies, typed boundaries, safe parsing and promotion review.

## MQL5 Agent
EAs, indicators and MT5 tests.
Rules: demo default, unique magic IDs, account/symbol validation, deterministic sizing, no hidden auto-trading and separate signed live configuration.

## Infrastructure Agent
Docker, Linux, CI, VPN, monitoring and backups.
Rules: non-root, pinned images, private networks, mounted secrets, healthchecks, limits, restore tests and no public management ports.

## Security Review Agent
Reviews threat model, permissions, secrets, public APIs, validation, dependencies and abuse tests. High-risk changes require its approval.
