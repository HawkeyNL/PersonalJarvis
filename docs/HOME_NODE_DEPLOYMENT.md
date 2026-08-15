# Home Node Deployment

The UM890 Pro is the planned always-on Jarvis Home Node. The node is a private server, not the user's primary development workstation.

## Host responsibilities

Run these directly on Ubuntu:

- `jarvis-core` as a native systemd service
- SSH
- Docker Engine + Compose plugin
- private VPN/overlay networking
- host firewall
- hardware/OS monitoring
- Claude Code and Codex CLI for controlled engineering tasks

Jarvis Core should remain available when an individual Docker service fails. It is the orchestrator, not another application container.

## Docker responsibilities

Use Docker for stateful/supporting workloads and isolated workers:

- PostgreSQL
- pgvector (with PostgreSQL)
- Redis when measured need exists
- Prometheus/Grafana
- research workers
- backtesting workers
- other isolated application services

PostgreSQL is the durable source of truth. Redis is for ephemeral state, caching, coordination, and realtime fan-out; it must not become the canonical memory store.

## Network model

```text
Internet
   X  no direct SSH/admin exposure

Private VPN / trusted network
   |
   +--> Jarvis API / Core
   +--> SSH
   +--> observability

Jarvis Core
   |
   +--> Docker services on private networks
   +--> IBKR gateway
   +--> research providers
   +--> Claude Code / Codex
```

Only the interfaces that are explicitly required should be reachable. Database ports should not be published publicly.

## Service ownership

| Service | Runtime | Restart policy | Data |
|---|---|---|---|
| Jarvis Core | systemd | automatic | PostgreSQL/files |
| PostgreSQL | Docker | unless-stopped | Docker volume |
| Redis | Docker | unless-stopped | ephemeral/persistent only when justified |
| Workers | Docker | task-specific | temporary + durable results in PostgreSQL |
| Monitoring | Docker | unless-stopped | metrics volumes |
| Claude Code | host task runner | task-scoped | isolated worktree |
| Codex | host task runner | task-scoped | isolated worktree |

## Updates

Updates should be transactional and observable:

1. fetch a known revision/release
2. build/validate
3. run tests
4. create a backup/rollback point
5. deploy
6. restart only the affected service
7. run health checks
8. automatically roll back when a health check fails

Jarvis should request privileged operations from a narrow updater/guardian capability rather than receiving unrestricted `sudo` access.

## Backups

Back up PostgreSQL independently from Docker container lifecycle. Keep at least one recent local backup and one separate backup target. Never rely on a container volume alone as the backup strategy.

## Future local AI

Ollama may run as an optional local model provider. It should not be required for the Core to function. The model router can choose between local inference, Claude, OpenAI/Codex, DeepSeek, or other providers according to task, latency, cost, privacy, and availability.
