# Projectstatus

## Release
Blueprint v2.6

## Huidige fase
Fase 0 — fundament. Backend én Tauri/Vue-client (macOS) gebouwd en geverifieerd.

## Eerstvolgende taak
iOS-target voor de Tauri-client (vereist volledige Xcode), daarna `JAR-100 — User/device model`.

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
- Tauri 2 + Vue 3 client (Pinia + Vue Router) — macOS-app `Jarvis.app` gebouwd en geverifieerd

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
- Tauri 2 + Vue 3 client (`apps/client`, Pinia + Vue Router) gescaffold; macOS `Jarvis.app` gebouwd en geverifieerd. iOS volgt.
