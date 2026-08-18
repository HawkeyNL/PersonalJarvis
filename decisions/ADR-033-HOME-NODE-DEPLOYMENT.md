# ADR-033: Home Node deployment model

- Status: Proposed
- Date: 2026-08-15

> Datastore details are superseded by ADR-034: SurrealDB 2.6 is the Core
> datastore. The native-Core/systemd deployment decision remains in force.

## Context

The PersonalJarvis project is moving toward a dedicated always-on home node (MINISFORUM UM890 Pro) running Ubuntu. The node will host Jarvis Core, databases, workers, trading connectivity and development agents while remaining reachable remotely over a private network.

The repository uses a modular Rust backend and Docker for SurrealDB during development. We want the production deployment to preserve that simplicity while avoiding a failure in one container taking the Jarvis orchestrator down with it.

## Decision

Jarvis Core remains a native Rust process managed by `systemd` on the Ubuntu host.

Stateful and isolated supporting services run in Docker Compose. Initially these include SurrealDB 2.6, and later monitoring/workers when justified.

Conceptually:

```text
Ubuntu host
|
+-- systemd
|   +-- jarvis-core
|   +-- jarvis-guardian (future)
|   +-- sshd
|   +-- docker
|
+-- Docker
    +-- SurrealDB 2.6
    +-- Redis (only when needed)
    +-- monitoring
    +-- research/backtest workers
    +-- isolated integrations
```

## Remote access

SSH must be available through a private VPN/tailnet rather than exposing SSH directly to the public internet. The same private network is the preferred path for Jarvis device APIs and administration.

## Secrets

Secrets are not stored in the repository, Docker images or ordinary source-controlled configuration. Runtime secrets should be injected through host-managed secret files/environment mechanisms with restrictive permissions. Long-lived provider keys should be avoided where short-lived credentials are available.

## Updates

Jarvis Core updates are performed by a dedicated, narrowly scoped updater workflow rather than by giving the LLM arbitrary `sudo` access. The updater should:

1. fetch a known release/commit
2. build and run tests
3. verify the resulting artifact
4. retain a rollback version
5. restart the systemd unit
6. run a health/readiness check
7. roll back automatically if the health check fails

Container updates follow the same principle: allowlisted services only, with health checks before considering the deployment successful.

## Database

SurrealDB remains the source of truth for durable state. Redis, if introduced, is for ephemeral state, caching, queues or coordination rather than irreplaceable memory.

Production database migrations must fail closed: if migrations cannot be applied, Jarvis Core must not start against an unknown schema.

## Observability

The Home Node should expose machine-readable health and metrics for at least:

- Jarvis Core
- SurrealDB
- Docker services
- CPU/RAM/disk
- temperature and throttling
- model/provider latency
- background task duration
- trading connectivity

Long-running work must be represented as tasks/events so the UI can report progress and Jarvis can reason about expected versus actual duration.

## Consequences

### Positive

- Jarvis remains available even when a supporting container fails
- Docker gives clean isolation for databases and workers
- systemd provides restart and boot-time lifecycle management
- private-network SSH avoids public management exposure
- the design fits the existing modular-monolith architecture

### Negative

- two lifecycle systems (systemd and Docker) must be documented
- deployment automation needs explicit privilege boundaries
- host-level backup and recovery must be planned
