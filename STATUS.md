# Projectstatus

## Release
Blueprint v2.6

## Huidige fase
Fase 0 — fundament. Backend-skelet geïmplementeerd en geverifieerd; Tauri/Vue-client nog te doen.

## Eerstvolgende taak
Tauri 2 + Vue 3 client scaffolden (Fase 0 afronden), daarna `JAR-100 — User/device model`.

## Klaar
- Productvisie
- Architectuur
- Agents
- MCP/API-keuzes
- IBKR/MT5/futures/prop research
- NautilusTrader evaluatie
- Risk/backtest/securityprincipes
- Centrale TODO- en stappenstructuur
- JAR-001/002/003 gebouwd en geverifieerd: Rust workspace, Axum API (/livez, /readyz), typed config, tracing, SQLx-migraties, Docker dev-stack en CI

## Blockers/beslissingen
- Primaire LLM-provider
- Eerste marktdata-provider
- Praktische IBKR API-route
- Prop-firmkeuze
- Crypto versus prediction market
- Lokale hardware versus cloud-API

## Laatste update
8 augustus 2026

- JAR-001 geïmplementeerd: Rust workspace, Axum API (/livez, /readyz), typed config, tracing, SQLx-migraties, Docker dev-stack en CI. Build/clippy/fmt/test groen; /readyz getest tegen Postgres 17.
