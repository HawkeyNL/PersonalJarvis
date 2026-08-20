# Jarvis Personal AI Operating System Blueprint

# Jarvis Blueprint v2.2

Deze repository is het centrale besturingsdocument voor mensen en coding-agents.

## Lokaal opstarten

Snelstart om de volledige keten (SurrealDB → API → client) op je Mac te draaien.
Uitgebreide uitleg staat in [`DEVELOPMENT.md`](DEVELOPMENT.md).

### Benodigdheden

- **Rust** (stable, via [rustup](https://rustup.rs)) — versie gepind in `rust-toolchain.toml`
- **Node 24** via [nvm](https://github.com/nvm-sh/nvm) — gepind in `.nvmrc`
- **Docker Desktop** — voor de lokale SurrealDB
- **Xcode 26** (volledig, niet alleen CommandLineTools) — alleen nodig voor de macOS/iOS-client

### 1. Backend (API + database)

```bash
# a. Start SurrealDB (wacht tot de init-service klaar is)
docker compose -f deploy/compose/docker-compose.yml up -d --wait

# Eenmalig lokaal: maak de database-scoped Core-account aan.
# Gebruik in productie een uniek wachtwoord uit een root-only secretbestand.
printf '%s\n' "DEFINE USER core ON DATABASE PASSWORD 'replace-with-a-strong-secret' ROLES EDITOR;" \
  | docker compose -f deploy/compose/docker-compose.yml exec -T surrealdb \
      /surreal sql --hide-welcome --endpoint ws://127.0.0.1:8000 \
      --username root --password "$SURREAL_ROOT_PASS" --auth-level root \
      --namespace jarvis --database core

# b. Kopieer de voorbeeld-config (pas aan indien nodig)
cp .env.example .env

# c. Start de API — draait de migraties automatisch bij het opstarten
cargo run -p jarvis-api
```

De API luistert op **`http://localhost:8080`** (`JARVIS_BIND_ADDR` in `.env`). De
Core gebruikt uitsluitend de database-scoped SurrealDB-account uit `.env`; geef
hem nooit het SurrealDB-rootwachtwoord.
Snel testen: `curl http://localhost:8080/livez` en `curl http://localhost:8080/readyz`.

Belangrijkste endpoints:

| Methode | Pad | Doel |
|---------|-----|------|
| GET | `/livez` · `/readyz` | liveness / readiness (pingt SurrealDB) |
| POST | `/v1/auth/enroll` · `/challenge` · `/login` · `/logout` | device-bound auth (Ed25519) |
| GET/DELETE | `/v1/devices` · `/v1/devices/{id}` | apparatenbeheer (Bearer-token) |
| GET/POST/DELETE | `/v1/holdings` · `/v1/holdings/{id}` | portfolio (Decimal-geld) |
| GET | `/v1/broker/ibkr/status` · `/positions` | IBKR read-only (via gateway) |
| POST | `/v1/assistant/chat` | Jarvis-brein (Claude, Bearer-token) |

#### API-codestructuur (`jarvis-api`)

De API is opgesplitst in samenhangende modules; `lib.rs` is nog slechts de
compositieroot (router-bedrading + health-probes). Handlers zijn per concern
gegroepeerd, elk met zijn eigen request-DTO's:

| Module | Verantwoordelijkheid |
|--------|----------------------|
| `lib.rs` | `build_router`, health-probes, persona-loader, router-infra |
| `state.rs` | `AppState` — gedeelde, goedkoop-kloonbare dependency-bundle |
| `extract.rs` | `Authed` — de Bearer-token auth-poort (elke beschermde handler) |
| `error.rs` | opake HTTP-error/response-helpers (details alleen in logs) |
| `rate_limit.rs` | per-IP rate-limiting + login-lockout + `X-Forwarded-For` trust-model |
| `metering.rs` | LLM-kosten/budget bijhouden (ADR-027) |
| `audit.rs` | append-only security- en agent-audit-trail |
| `validation.rs` | invoer-bounds (lengtes, hex-vormen) |
| `mcp.rs` | read-only MCP-server voor Claude-tooling (ADR-031) |
| `routes/auth.rs` | enroll/challenge/login/logout, apparaten, unlock-flow |
| `routes/chat.rs` | chat + gesprekken + orchestrate (ADR-028/030) |
| `routes/agent.rs` | agentische uitvoering + device-getekende goedkeuring (ADR-029) |
| `routes/system.rs` | usage/registry/self-improve (advisory-only) |
| `routes/portfolio.rs` | holdings (cost-basis + allocatie) |
| `routes/broker.rs` | IBKR read-only (status/positions) |
| `routes/voice.rs` | STT + speaker-verificatie (gemaks-signaal, nooit het slot) |

Unit-tests van privé-interne functies staan in de modules zelf; de HTTP-round-trip
integratietests staan in `jarvis-api/tests/surreal_api.rs` en spreken de crate alleen via
haar publieke API aan.

### 2. Client (macOS)

In een **tweede terminal**, met de API draaiend:

```bash
cd jarvis-app
nvm use            # Node 24 (zie .nvmrc)
npm install        # eenmalig
npm run tauri dev  # hot-reload dev-venster
```

Native `.app` bouwen: `npm run tauri build -- --bundles app`
→ `jarvis-app/src-tauri/target/release/bundle/macos/Jarvis.app`.

### 3. Client (iOS-simulator, optioneel)

```bash
cd jarvis-app
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

### 6. "Hey Jarvis" aanzetten (optioneel)

Handsfree activeren met **"Hey Jarvis"**, en alleen jouw stem (on-device via
Picovoice — geen audio verlaat je toestel; zie `decisions/ADR-024`).

```bash
cd jarvis-app
npm run fetch-models   # haalt de Porcupine/Eagle-modellen (niet in git)
```

Daarna in de app onder **Settings → Stem-activatie**: plak een gratis
[Picovoice AccessKey](https://console.picovoice.ai), neem je stem op, en zet de
toggle aan. Stem is een *gemaks*-laag: vergrendeld start "Hey Jarvis" alleen de
Touch ID-prompt — de biometrie blijft het slot.

### Checks (zoals in CI)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo audit
```

De opt-in SurrealDB wire-protocoltests vereisen een wegwerp-SurrealDB-instance;
CI start deze service en geeft alleen testcredentials door:

```bash
JARVIS_SURREAL_TEST_ENDPOINT=127.0.0.1:8000 \
JARVIS_SURREAL_TEST_USER=root \
JARVIS_SURREAL_TEST_PASS=<test-root-password> cargo test --all -- --ignored
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
- [x] SurrealDB schema (SCHEMAFULL, versioned baseline)
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
- [x] SurrealDB + typed Rust repositories
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
- [ ] Ubuntu Desktop LTS + SSH
- [ ] private VPN/tunnel
- [ ] trusted-device enrollment
- [ ] health/presence
- [ ] capability-based remote tasks
- [ ] remote-screen solution
- [ ] Infrastructure Galaxy integration

## Fase 1B — memory platform

- [ ] SurrealDB memory schema/vector index
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

- `docs/blueprint/coding/CODE_AGENT_CONSTITUTION.md`
- `docs/blueprint/coding/LANGUAGE_AGENT_ARCHITECTURE.md`
- `docs/blueprint/coding/SECURITY_REVIEW_CHECKLIST.md`
- `docs/blueprint/security/PUBLIC_API_SECURITY_STANDARD.md`
- `docs/blueprint/security/ACCESS_CONTROL_MATRIX.md`
- `docs/blueprint/security/INPUT_VALIDATION_STANDARD.md`

No public API is complete without rate limiting, secure credentials, server-side access control, input validation, audit and security tests.


## Engineering agent lifecycle

Major changes follow:

```text
Research → Impact Analysis → Design/ADR → Security Review → Code
→ Tests → Independent Reviews → Fix Loop → Release Gate
→ Observability → Improvement Planning
```

The Observability Intelligence Agent converts telemetry into improvement plans but cannot change production automatically.
