# Implementatieroadmap

## Fase 0 — fundament

- Monorepo aanmaken.
- Rust workspace.
- Tauri 2 + Vue 3 skeleton.
- Axum API.
- PostgreSQL + migrations.
- Auth/device model.
- Docker Compose.
- OpenTelemetry/logging.
- CI met fmt, clippy, tests en dependency audit.

## Fase 1 — read-only persoonlijke app

- Watchlist.
- Portfolio handmatig.
- Nieuwsfeed.
- AI-chat met read-only tools.
- Centrale sync.
- Pushmeldingen.
- Ollama fallback.
- Geen brokerorders.

## Fase 2 — IBKR read-only

- Paper account.
- Contract/instrument mapping.
- Posities, cash, transacties, open orders.
- Reconciliation.
- Portfoliohistorie.
- Session health.
- Geen submit.

## Fase 3 — MT5 native MCP read-only

- MT5 Build 6060+.
- Beveiligde tunnel.
- Tool inventory vastleggen.
- Read-only policy.
- Marktdata, account, posities, historie.
- Audit.
- Bestaande copier ongemoeid laten.

## Fase 4 — research en investeringsassistent

- Filings/transcripts/news.
- RAG.
- Allocation targets.
- Maandelijkse deterministic allocator.
- AI-uitleg.
- Orderproposal zonder execution.

## Fase 5 — paper execution

- Risk engine.
- Approval flow.
- Idempotency.
- IBKR paper submit.
- MT5 demo submit.
- Partial fills/cancels.
- Reconciliation.
- Emergency kill switch.

## Fase 6 — backtesting

- Strategy DSL/spec.
- MT5 Strategy Tester workflow.
- Result import.
- Metrics.
- Walk-forward.
- Strategy registry en promotion gates.

## Fase 7 — content-engine

- Trend ingestion.
- Idea scoring.
- Scripts.
- Voice/assets/render.
- Handmatige publicatie.
- Analyticsfeedback.

## Fase 8 — assisted live

Alleen na langdurige paperfase:

- kleine notional;
- harde daglimiet;
- iedere order bevestigen;
- allowlisted symbols;
- verplichte stop;
- automatische freeze bij afwijking.

## Fase 9 — gecontroleerde automatisering

Alleen vaste, geteste strategieën. Geen open-ended “AI scalper”.
