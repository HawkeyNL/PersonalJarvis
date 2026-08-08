# Systeemarchitectuur

## Overzicht

```text
┌───────────────────────────────────────────┐
│ Tauri 2 + Vue 3                           │
│ iOS / macOS / Windows / Linux             │
│ local SQLite cache + OS keychain          │
└──────────────────┬────────────────────────┘
                   │ HTTPS + WebSocket/SSE
                   ▼
┌───────────────────────────────────────────┐
│ Jarvis API / BFF (Axum)                   │
│ auth, devices, sync, approvals, chat      │
└───────┬──────────────┬────────────────────┘
        │              │
        ▼              ▼
 Agent Orchestrator   Domain services
        │              ├─ portfolio
        │              ├─ market data
        │              ├─ research
        │              ├─ content
        │              └─ notifications
        ▼
 Tool/Policy Gateway
        ├─ internal typed tools
        ├─ MCP clients
        └─ approval + risk checks
                 │
        ┌────────┴─────────┐
        ▼                  ▼
 IBKR Adapter          MT5 native MCP
        │                  │
 IB Gateway/TWS/Web API    Windows VPS / MT5
```

## Client

### Technologie

- Tauri 2
- Vue 3 + TypeScript
- Pinia
- Vue Router
- SQLite plugin of Rust-side SQLite
- OS keychain/secure storage
- Push notification abstraction
- WebSocket/SSE voor realtime updates

### Verantwoordelijkheden

- UI en lokale cache
- apparaatregistratie
- biometrische bevestiging
- offline leesmodus
- visualisatie van portfolio, trades, research en content
- nooit brokercredentials bewaren buiten OS secure storage
- nooit zelfstandig orders ondertekenen zonder serverchallenge

## Backend

### Axum API/BFF

- authenticatie
- device sessions
- request validation
- rate limiting
- CSRF waar relevant
- command endpoints
- query endpoints
- approval challenges
- streaming van agentevents

### Orchestrator

- intentclassificatie
- agentselectie
- modelrouting
- toolbudget en tokenbudget
- workflowstate
- retries en timeouts
- contextminimalisatie
- policy checks

### Tool/Policy Gateway

Enige route naar muterende externe tools.

```text
LLM voorstel
→ JSON-schema validatie
→ bevoegdheidscontrole
→ business policy
→ risk engine
→ fresh-data check
→ user approval
→ idempotency check
→ adapter execution
→ reconciliation
→ audit event
```

### Databases

- PostgreSQL: centrale state
- SQLite: clientcache
- Redis optioneel: locks, queues, short-lived sessions
- Object storage optioneel: rapporten, charts, video-assets
- Vector store pas toevoegen wanneer document-RAG nodig is; PostgreSQL + pgvector is eerst voldoende

## Deploymentzones

### Linux VPS

- API
- orchestrator
- databases
- scheduler
- market/news ingestion
- contentplanning
- monitoring

### Windows VPS

- MT5 terminal
- native MT5 MCP
- bestaande Telegram copier
- optionele MQL5 EA/risk sentinel
- alleen via WireGuard/Tailscale of streng afgeschermde tunnel bereikbaar

### Eigen apparaten

- Tauri-clients
- optioneel Ollama op krachtige desktop
- lokale encrypted cache

## Eventmodel

Voorbeelden:

- `portfolio.snapshot.received`
- `broker.order.proposed`
- `broker.order.approved`
- `broker.order.submitted`
- `broker.order.filled`
- `risk.limit.breached`
- `market.news.ingested`
- `agent.report.completed`
- `content.idea.scored`
- `backtest.completed`

Begin met een Postgres outbox-pattern. Voeg pas later NATS/Kafka toe.
