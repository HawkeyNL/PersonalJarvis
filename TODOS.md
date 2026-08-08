
## P0B — Engineering Agent Workflow

### JAR-060 — Engineering Orchestrator
- [ ] dependency-aware routing and lifecycle gates
- [ ] conflict detection
- [ ] reviewer assignments

### JAR-061 — Architecture Research Agent
- [ ] design-pattern comparison
- [ ] official documentation research
- [ ] future-expansion and migration analysis
- [ ] ADR generation

### JAR-062 — Codebase Impact Agent
- [ ] dependency/change graph
- [ ] API/schema/config/security impact
- [ ] regression and rollback analysis

### JAR-063 — Independent Reviewers
- [ ] architecture
- [ ] correctness
- [ ] performance
- [ ] database
- [ ] API contract
- [ ] UX/accessibility
- [ ] operations
- [ ] security

### JAR-064 — Fix and reverification loop
- [ ] structured findings
- [ ] root-cause analysis
- [ ] regression tests
- [ ] original-reviewer recheck

### JAR-065 — Engineering Memory
- [ ] ADR/incident/benchmark retrieval
- [ ] recurring finding detection
- [ ] technical debt context

## P0C — Observability Intelligence

### JAR-070 — Structured logging taxonomy
- [ ] stable events and trace correlation
- [ ] redaction and retention
- [ ] sampling

### JAR-071 — OpenTelemetry instrumentation
- [ ] API/workers/agents/tools/database/device mesh

### JAR-072 — Performance budgets
- [ ] API, voice, memory, UI, database and trading paths

### JAR-073 — Observability Intelligence Agent
- [ ] errors and slow-path clustering
- [ ] evidence-based improvement plans
- [ ] no autonomous production changes

### JAR-074 — Incident Learning Agent
- [ ] timeline, root cause, remediation, runbook/test updates

### JAR-075 — Performance regression CI
- [ ] benchmark baselines, load tests and thresholds

---


## P0A — Code Agent and API Security

### JAR-050 — Enforce Code Agent Constitution
- [ ] mandatory rule loading
- [ ] security report in agent output
- [ ] completion blocked on failed checklist

### JAR-051 — Language agents
- [ ] Rust
- [ ] TypeScript/Vue/Tauri
- [ ] SQL/PostgreSQL
- [ ] Python Research
- [ ] MQL5
- [ ] Infrastructure
- [ ] Security Review

### JAR-052 — Access Control Matrix
- [ ] capability registry
- [ ] routes/tools/tasks mapped
- [ ] deny by default
- [ ] automated matrix tests

### JAR-053 — Public API rate limiting
- [ ] IP/user/device/token dimensions
- [ ] weighted profiles
- [ ] burst/sustained/concurrency limits
- [ ] 429, Retry-After and alerts

### JAR-054 — Credential service
- [ ] mounted/static secrets
- [ ] encrypted dynamic tokens
- [ ] hashed rotating refresh tokens
- [ ] short-lived service credentials
- [ ] rotation/revocation

### JAR-055 — Shared input-validation library
- [ ] bounded values
- [ ] Decimal money
- [ ] canonical IDs
- [ ] URL/SSRF protection
- [ ] upload/path validation

### JAR-056 — Public API security tests
- [ ] auth/access control
- [ ] rate limiting
- [ ] fuzz/property validation
- [ ] injection/SSRF/path traversal
- [ ] replay/idempotency
- [ ] secret leakage

### JAR-057 — Security CI gate
- [ ] dependency audit
- [ ] secret scan
- [ ] SAST/container scan
- [ ] block high-severity merge

---

# Centrale TODO-lijst

Status: `[ ] TODO`, `[-] IN PROGRESS`, `[x] DONE`, `[!] BLOCKED`, `[?] NEEDS DECISION`.

## P0 — fundament
- [x] JAR-001 Monorepo initialiseren
- [x] JAR-002 Docker development stack
- [x] JAR-003 Typed configuration en environments
- [-] JAR-004 Observability basis — structured logging/tracing klaar; OpenTelemetry/metrics volgt (JAR-070..075)

## P1 — identity en sync
- [x] JAR-100 User/device model
- [ ] JAR-101 Device-bound sessions
- [ ] JAR-102 Local encrypted cache
- [ ] JAR-103 Sync protocol
- [ ] JAR-104 Approval challenge en replay protection


## P1A — Home Node and Device Mesh

### JAR-150 — Buy and install Home Node
- [ ] choose ASUS NUC 14 Pro or reputable N100/N150 mini PC
- [ ] 32 GB RAM target
- [ ] 1 TB NVMe target
- [ ] verify 2.5 GbE and Ubuntu support
- [ ] measure meter-cupboard temperature
- [ ] configure power-on-after-AC-loss

### JAR-151 — Headless Ubuntu bootstrap
- [ ] Ubuntu Server LTS
- [ ] SSH key-only
- [ ] firewall
- [ ] updates
- [ ] DHCP reservation
- [ ] LAN hostname

### JAR-152 — Private remote access
- [ ] Tailscale or WireGuard
- [ ] no public SSH
- [ ] SSH from laptop/desktop
- [ ] recovery documentation

### JAR-153 — Cockpit/web management
- [ ] VPN/LAN only
- [ ] service/log/disk monitoring
- [ ] strong authentication

### JAR-154 — Device enrollment
- [ ] device keypairs
- [ ] certificates
- [ ] approve/revoke/quarantine

### JAR-155 — Device health and presence
- [ ] CPU/RAM/disk/battery
- [ ] online/offline
- [ ] Observatory integration

### JAR-156 — Typed remote tasks
- [ ] capability allowlist
- [ ] signed expiring commands
- [ ] approvals
- [ ] audit

### JAR-157 — Remote screen solution
- [?] choose RustDesk, MeshCentral or OS-native approach
- [ ] VPN-only
- [ ] explicit session approval
- [ ] session timeout/audit

### JAR-158 — Home Node recovery and resilience
- [ ] AC-loss boot test
- [ ] backup config
- [ ] temperature alert
- [ ] optional UPS
- [ ] emergency local-console method

---

## P2 — agent runtime
- [ ] JAR-200 Model provider trait
- [ ] JAR-201 Primaire cloudprovider
- [ ] JAR-202 Ollama fallback
- [ ] JAR-203 Structured output validation
- [ ] JAR-204 Tool registry
- [ ] JAR-205 Agent orchestration
- [ ] JAR-206 Model cost accounting


## P2A — Memory Platform

### JAR-250 — PostgreSQL memory schema
- [ ] Status: TODO
- Owner: Memory Agent
- Dependencies: JAR-002
- Acceptatie:
  - memories, entities, summaries en source references
  - typed fields plus JSONB metadata
  - migrations en indexes

### JAR-251 — pgvector integration
- [ ] Status: TODO
- Owner: Memory Agent
- Dependencies: JAR-250
- Acceptatie:
  - embedding storage
  - semantic retrieval
  - duplicate candidate search
  - retrieval benchmarks

### JAR-252 — Context Builder
- [ ] Status: TODO
- Owner: AI Platform Agent
- Dependencies: JAR-203, JAR-251
- Acceptatie:
  - token budget
  - conversation summary
  - relevant memories
  - provenance

### JAR-253 — Memory consolidation worker
- [ ] Status: TODO
- Owner: Memory Agent
- Dependencies: JAR-251
- Acceptatie:
  - candidate extraction
  - deduplication
  - superseding
  - archive handoff

### JAR-254 — Client encrypted SQLite cache
- [ ] Status: TODO
- Owner: Client Agent
- Dependencies: JAR-102
- Acceptatie:
  - encrypted local cache
  - sync cursor
  - no provider/broker secrets

### JAR-255 — Archive storage
- [ ] Status: TODO
- Owner: Backend Agent
- Dependencies: JAR-250
- Acceptatie:
  - JSONL/Parquet archive
  - compression
  - metadata in PostgreSQL
  - retention policy

### JAR-256 — Redis decision gate
- [?] Status: NEEDS DECISION
- Owner: Architect
- Dependencies: measured need
- Decision:
  - do not add Redis until locks, queues or distributed cache require it

### JAR-257 — Memory cost metrics
- [ ] Status: TODO
- Owner: Economics Agent
- Dependencies: JAR-252, JAR-253
- Acceptatie:
  - tokens saved
  - embedding cost
  - storage growth
  - consolidation cost

---


## P2B — Agent Observatory

### JAR-270 — Observatory event schema
- [ ] typed event envelope
- [ ] trace/run correlation
- [ ] privacy/sensitivity labels

### JAR-271 — Backend event stream
- [ ] WebSocket/SSE
- [ ] reconnect cursor
- [ ] rate limiting
- [ ] summary-only defaults

### JAR-272 — Observatory persistence
- [ ] PostgreSQL event storage
- [ ] retention
- [ ] replay queries

### JAR-273 — 2D observatory prototype
- [ ] stable graph layout
- [ ] agent/tool nodes
- [ ] timeline
- [ ] event animation

### JAR-274 — 3D solar-system view
- [ ] Three.js
- [ ] stable orbits
- [ ] agent planets
- [ ] tool perimeter
- [ ] message particles

### JAR-275 — Run replay
- [ ] play/pause
- [ ] scrub
- [ ] speed
- [ ] event inspection

### JAR-276 — Cost/performance/security modes
- [ ] tokens/cost
- [ ] latency
- [ ] errors
- [ ] approvals/scopes

### JAR-277 — Mobile and battery optimization
- [ ] quality modes
- [ ] 2D fallback
- [ ] background pause
- [ ] reduced motion

### JAR-278 — Observatory privacy tests
- [ ] secret redaction
- [ ] sensitive payload authorization
- [ ] prompt-injection-safe labels

---

## P3 — finance read-only
- [ ] JAR-300 Instrument master
- [ ] JAR-301 Market data provider
- [ ] JAR-302 News ingestion
- [ ] JAR-303 Portfolio domain
- [ ] JAR-304 Investment Analyst
- [ ] JAR-305 Deterministic allocator

## P4 — IBKR
- [ ] JAR-400 API access spike
- [ ] JAR-401 Paper connection
- [ ] JAR-402 Accounts/cash/positions
- [ ] JAR-403 Orders/executions read-only
- [ ] JAR-404 Reconciliation

## P5 — futures/orderflow/Nautilus
- [ ] JAR-500 Futures contract master
- [ ] JAR-501 Tick/trade ingestion
- [ ] JAR-502 Depth ingestion
- [ ] JAR-503 Orderbook builder
- [ ] JAR-504 Orderflow features
- [ ] JAR-505 Replay engine
- [ ] JAR-506 NautilusTrader PoC
- [?] JAR-507 Nautilus versus eigen engine besluit

## P6 — MT5/prop
- [ ] JAR-600 MT5 MCP inventory
- [ ] JAR-601 MT5 read-only proxy
- [ ] JAR-602 Prop rule schema
- [ ] JAR-603 Drawdown engine
- [ ] JAR-604 Prop dashboard
- [ ] JAR-605 TopstepX access spike
- [ ] JAR-606 MFFU platform spike

## P7 — risk/execution
- [ ] JAR-700 Risk profile model
- [ ] JAR-701 Position sizing
- [ ] JAR-702 Daily/weekly loss limits
- [ ] JAR-703 Order proposal state machine
- [ ] JAR-704 Paper submit
- [ ] JAR-705 Kill switch

## P8 — crypto/prediction markets
- [?] JAR-800 Kies eerste venue/markt
- [ ] JAR-801 Crypto market-data spike
- [ ] JAR-802 Crypto paper adapter
- [ ] JAR-803 Polymarket access/API/juridische spike
- [ ] JAR-804 Prediction-market pricing model
- [ ] JAR-805 Shadow/paper mode

## P9 — economics engine
- [ ] JAR-900 Cost ledger
- [ ] JAR-901 AI cost attribution
- [ ] JAR-902 Infrastructure cost attribution
- [ ] JAR-903 Netto trading P&L na alle kosten
- [ ] JAR-904 Agent ROI dashboard


## P9A — API Quota Guardian

### JAR-920 — Provider budget configuration
- [ ] monthly soft/hard limits
- [ ] billing period/reset time
- [ ] essential/nonessential task classes

### JAR-921 — Usage collectors
- [ ] internal token ledger
- [ ] official provider usage APIs where available
- [ ] rate-limit headers
- [ ] manual reconciliation

### JAR-922 — Automatic routing and pause
- [ ] warn at soft limit
- [ ] cheaper fallback
- [ ] pause nonessential tasks
- [ ] never auto-replay financial writes

### JAR-923 — Reset detection and resume
- [ ] verify reset
- [ ] resume eligible queued work
- [ ] user notification
- [ ] audit

### JAR-924 — Observatory quota visualization
- [ ] remaining budget arcs
- [ ] reset countdown
- [ ] provider status
- [ ] alerts

---

## P10 — content
- [ ] JAR-1000 Trend ingestion
- [ ] JAR-1001 Idea scoring
- [ ] JAR-1002 Script pipeline
- [ ] JAR-1003 Render pipeline
- [ ] JAR-1004 Human review
- [ ] JAR-1005 Analytics en cost attribution


## P8A — Event Alpha Engine

### JAR-850 — Event source registry
- [ ] primaire/secondaire bronnen
- [ ] provenance en timestamps
- [ ] bronkwaliteitspolicy

### JAR-851 — Event verification pipeline
- [ ] primaire bevestiging
- [ ] conflicthandling
- [ ] confidence logging

### JAR-852 — Event-to-market mapper
- [ ] typed market IDs
- [ ] mapping-evaluatie
- [ ] geen vrije symbol guessing

### JAR-853 — Polymarket read-only adapter
- [ ] markets/contracts/orderbook
- [ ] resolution rules
- [ ] toegang/juridische aannames

### JAR-854 — Market reaction monitor
- [ ] pre/post-event snapshots
- [ ] latency
- [ ] spread/liquiditeit

### JAR-855 — Netto EV-engine
- [ ] fees/spread/slippage
- [ ] gas/MEV waar relevant
- [ ] model- en infrastructuurkosten

### JAR-856 — Shadow fill simulator
- [ ] realistische fills
- [ ] no-fill/failure cases
- [ ] latency sensitivity

### JAR-857 — Performance report
- [ ] netto expectancy
- [ ] false positives
- [ ] edge decay
- [ ] minimaal 100 observaties

### JAR-858 — DEX/on-chain scanner spike
- [ ] read-only
- [ ] gas/slippage/MEV-model
- [ ] geen wallet signing

### JAR-859 — Event Alpha live gate
- [!] BLOCKED totdat JAR-857 positief is en alle securitygates zijn gehaald

---

## Agent session log

Voeg per sessie toe:

```text
Date:
Agent:
Tasks changed:
Files changed:
Tests:
Blockers:
Next recommended task:
```
