# Jarvis Personal AI Operating System Blueprint

# Jarvis Blueprint v2.2

Deze repository is het centrale besturingsdocument voor mensen en coding-agents.

## Lokaal opstarten

Snelstart om de volledige keten (Postgres → API → client) op je Mac te draaien.
Uitgebreide uitleg staat in [`DEVELOPMENT.md`](DEVELOPMENT.md).

### Benodigdheden

- **Rust** (stable, via [rustup](https://rustup.rs)) — versie gepind in `rust-toolchain.toml`
- **Node 24** via [nvm](https://github.com/nvm-sh/nvm) — gepind in `.nvmrc`
- **Docker Desktop** — voor de lokale Postgres
- **Xcode 26** (volledig, niet alleen CommandLineTools) — alleen nodig voor de macOS/iOS-client

### 1. Backend (API + database)

```bash
# a. Start Postgres (wacht tot 'healthy')
docker compose -f deploy/compose/docker-compose.yml up -d --wait

# b. Kopieer de voorbeeld-config (pas aan indien nodig)
cp .env.example .env

# c. Start de API — draait de migraties automatisch bij het opstarten
cargo run -p jarvis-api
```

De API luistert op **`http://localhost:8080`** (`JARVIS_BIND_ADDR` in `.env`).
Snel testen: `curl http://localhost:8080/livez` en `curl http://localhost:8080/readyz`.

Belangrijkste endpoints:

| Methode | Pad | Doel |
|---------|-----|------|
| GET | `/livez` · `/readyz` | liveness / readiness (pingt Postgres) |
| POST | `/v1/auth/enroll` · `/challenge` · `/login` · `/logout` | device-bound auth (Ed25519) |
| GET/DELETE | `/v1/devices` · `/v1/devices/{id}` | apparatenbeheer (Bearer-token) |
| GET/POST/DELETE | `/v1/holdings` · `/v1/holdings/{id}` | portfolio (Decimal-geld) |
| GET | `/v1/broker/ibkr/status` · `/positions` | IBKR read-only (via gateway) |
| POST | `/v1/assistant/chat` | Jarvis-brein (Claude, Bearer-token) |

### 2. Client (macOS)

In een **tweede terminal**, met de API draaiend:

```bash
cd apps/client
nvm use            # Node 24 (zie .nvmrc)
npm install        # eenmalig
npm run tauri dev  # hot-reload dev-venster
```

Native `.app` bouwen: `npm run tauri build -- --bundles app`
→ `apps/client/src-tauri/target/release/bundle/macos/Jarvis.app`.

### 3. Client (iOS-simulator, optioneel)

```bash
cd apps/client
npm run tauri ios init                          # eenmalig
npm run tauri ios build -- --target aarch64-sim --ci
xcrun simctl install booted <pad/naar/Jarvis.app>
xcrun simctl launch booted com.hawkeynl.jarvis
```

### 4. IBKR live koppelen (optioneel, read-only)

De backend proxeert alleen; het inloggen (SSO + 2FA) gebeurt in de **IBKR Client
Portal Gateway** die jij lokaal draait. Zet daarna `JARVIS_IBKR_GATEWAY_URL` in
`.env` (default `https://localhost:5000/v1/api`) en herstart de API. **Paper eerst.**

### 5. Jarvis-brein aanzetten (Claude of lokaal)

Het gesprek op de SYSTEM → Jarvis-view praat met een echt LLM via de backend
(DEC-001 = Claude, zie `decisions/ADR-022`). De API-sleutel leeft **alleen** in de
backend-`.env` — nooit in de client.

- **Claude (aanbevolen):** zet `JARVIS_LLM_API_KEY=<jouw Anthropic-sleutel>` in
  `.env` en herstart de API. Modellen per tier staan in `.env.example`
  (`claude-sonnet-5` standaard, `claude-opus-5` zwaar, `claude-haiku-4-5` snel).
- **Lokaal (gratis, offline):** draai [Ollama](https://ollama.com) (`ollama serve`
  + `ollama pull llama3.2`) en zet `JARVIS_LLM_PROVIDER=ollama`. Met **beide**
  gezet valt de backend automatisch terug op Ollama als Claude niet bereikbaar is.

Zonder sleutel én zonder Ollama toont de chat een nette "brein niet
bereikbaar"-melding.

### Checks (zoals in CI)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Integratietests (`#[sqlx::test]`) hebben een draaiende Postgres + `DATABASE_URL` nodig:

```bash
DATABASE_URL=postgres://jarvis:jarvis_dev_pw@localhost:5432/jarvis cargo test --all
```

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
- [x] Tauri-client gestart
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
- [x] Tauri 2 + Vue 3 (macOS + iOS)
- [x] Axum API
- [x] PostgreSQL + SQLx
- [x] Docker Compose
- [x] CI, logging en config

### Fase 1 — Jarvis-kern
- [ ] User/device-auth
- [ ] Centrale sync
- [ ] Lokale encrypted cache
- [x] Cloudmodeladapter (Claude via `crates/llm`, DEC-001/ADR-022)
- [x] Ollama fallback
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
