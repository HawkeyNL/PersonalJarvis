# Jarvis Personal AI Operating System Blueprint

# Jarvis Blueprint v2.2

Deze repository is het centrale besturingsdocument voor mensen en coding-agents.

## Voortgang bijhouden

- `README.md`: globale fasen en afvinkbare roadmap.
- `TODOS.md`: levende centrale backlog.
- `STEPS.md`: aanbevolen bouwvolgorde.
- `STATUS.md`: actuele projectstatus.
- `DECISIONS_PENDING.md`: open architectuurkeuzes.
- `CHANGELOG.md`: belangrijke wijzigingen.

Iedere coding-agent moet na werk taken afvinken, nieuwe taken toevoegen, tests vermelden en `STATUS.md` bijwerken.

## Huidige status

- [x] Productvisie
- [x] Hoofdarchitectuur
- [x] MCP-versus-API-strategie
- [x] IBKR-, MT5-, futures- en prop-firmonderzoek
- [x] NautilusTrader evaluatie toegevoegd
- [x] Agents, risk, execution en backtesting beschreven
- [x] Monorepo aangemaakt
- [ ] Tauri-client gestart
- [x] Rust/Axum API gestart
- [x] PostgreSQL-migraties
- [ ] Auth en sync
- [ ] Agent runtime
- [ ] IBKR paper read-only
- [ ] MT5 MCP read-only
- [ ] Futures market-data pipeline
- [ ] NautilusTrader PoC
- [ ] Risk engine
- [ ] Paper/demo execution
- [ ] Economics/cost engine
- [ ] Crypto/prediction-market spike
- [ ] Content-engine MVP
- [ ] Assisted-live gate

## Bouwfasen

### Fase 0 — fundament
- [x] Rust workspace
- [ ] Tauri 2 + Vue 3
- [x] Axum API
- [x] PostgreSQL + SQLx
- [x] Docker Compose
- [x] CI, logging en config

### Fase 1 — Jarvis-kern
- [ ] User/device-auth
- [ ] Centrale sync
- [ ] Lokale encrypted cache
- [ ] Cloudmodeladapter
- [ ] Ollama fallback
- [ ] Agent orchestration
- [ ] Tool registry
- [ ] Cost tracking
- [ ] Audit

### Fase 1A — Home Node en Device Mesh

- [ ] headless mini-pc kiezen en installeren
- [ ] Ubuntu Server + SSH
- [ ] private VPN/tunnel
- [ ] trusted-device enrollment
- [ ] health/presence
- [ ] capability-based remote tasks
- [ ] remote-screen solution
- [ ] Infrastructure Galaxy integration

## Fase 1B — memory platform

- [ ] PostgreSQL memory schema
- [ ] pgvector
- [ ] Context Builder met tokenbudget
- [ ] Memory consolidation
- [ ] Versleutelde SQLite-clientcache
- [ ] JSONL/Parquet-archief
- [ ] Redis alleen toevoegen bij gemeten noodzaak
- [ ] Memorykosten en tokenbesparing meten

## Fase 1C — Agent Observatory

- [ ] typed observability-events
- [ ] live WebSocket/SSE stream
- [ ] 2D graph prototype
- [ ] 3D solar-system view
- [ ] agent-agent communication animation
- [ ] replay mode
- [ ] cost/latency/security modes
- [ ] mobile battery saver en 2D fallback

## Fase 2 — read-only finance
- [ ] Instrument master
- [ ] Marktdata
- [ ] Nieuws
- [ ] Portfolio
- [ ] Investment Analyst
- [ ] Allocator

### Fase 3 — IBKR
- [ ] API proof
- [ ] Paperaccount
- [ ] Cash/positions
- [ ] Orders/executions
- [ ] Reconciliation

### Fase 4 — futures/orderflow
- [ ] Contract master
- [ ] Trades/depth
- [ ] Orderbook builder
- [ ] Features
- [ ] Replay
- [ ] NautilusTrader PoC

### Fase 5 — MT5/prop
- [ ] MT5 MCP inventory
- [ ] Read-only proxy
- [ ] Prop rule schema
- [ ] Drawdown/cutoff alerts

### Fase 6 — paper execution
- [ ] Order proposals
- [ ] Risk engine
- [ ] Approval flow
- [ ] Idempotency
- [ ] Submit/cancel/reconcile
- [ ] Kill switch

### Fase 7 — backtesting
- [ ] Strategy specs
- [ ] Versioning
- [ ] Costs/slippage
- [ ] OOS/walk-forward
- [ ] Promotion gates

### Fase 8 — crypto/prediction markets
- [ ] Kies één eerste experiment
- [ ] Read-only data
- [ ] Shadow mode
- [ ] Paper/sim
- [ ] Kosten en expectancy meten

### Fase 9 — economics engine

- [ ] API Quota Guardian
- [ ] provider soft/hard limits
- [ ] reset detection en automatische hervatting
- [ ] AI-kosten
- [ ] VPS/data/prop/brokerkosten
- [ ] Lokale GPU/stroomschatting
- [ ] Netto P&L en ROI per agent

### Fase 10 — content engine
- [ ] Trends
- [ ] Scripts
- [ ] Assets/render
- [ ] Review
- [ ] Analytics en kosten per video

### Fase 11 — assisted live
- [ ] Paperperiode voltooid
- [ ] Risk/kill-switch/reconciliation getest
- [ ] Kleine notional
- [ ] Iedere order handmatig bevestigd

## Coding-agentregels

1. Lees eerst `STATUS.md`, `TODOS.md` en `STEPS.md`.
2. Kies alleen taken waarvan afhankelijkheden klaar zijn.
3. Zet taken eerst op IN PROGRESS.
4. Voeg tests toe.
5. Markeer niets als klaar zonder bewijs.
6. Leg architectuurwijzigingen vast in een ADR.
7. Activeer nooit live trading vanuit development.


## Mandatory code-agent security baseline

All coding agents must read:

- `coding/CODE_AGENT_CONSTITUTION.md`
- `coding/LANGUAGE_AGENT_ARCHITECTURE.md`
- `coding/SECURITY_REVIEW_CHECKLIST.md`
- `security/PUBLIC_API_SECURITY_STANDARD.md`
- `security/ACCESS_CONTROL_MATRIX.md`
- `security/INPUT_VALIDATION_STANDARD.md`

No public API is complete without rate limiting, secure credentials, server-side access control, input validation, audit and security tests.


## Engineering agent lifecycle

Major changes follow:

```text
Research → Impact Analysis → Design/ADR → Security Review → Code
→ Tests → Independent Reviews → Fix Loop → Release Gate
→ Observability → Improvement Planning
```

The Observability Intelligence Agent converts telemetry into improvement plans but cannot change production automatically.
