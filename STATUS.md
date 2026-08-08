# Projectstatus

## Release
Blueprint v2.6

## Huidige fase
Fase 1 (identity) gestart. Fase 0 — backend + Tauri/Vue-client op macOS én iOS — is afgerond.

## Eerstvolgende taak
`JAR-101 — Device-bound sessions` (auth-challenge + sessies) en de identity-API-endpoints.

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
- Tauri 2 + Vue 3 client (Pinia + Vue Router) — `Jarvis.app` gebouwd en geverifieerd op macOS én iOS-simulator (iPhone 17, iOS 26.3)
- JAR-100 user/device model: `users`/`devices`/`device_keys`-schema + `jarvis-identity` repository (create/register/list/revoke), geverifieerd met unit- én Postgres-integratietests

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
- Tauri 2 + Vue 3 client (`apps/client`, Pinia + Vue Router) gescaffold; macOS `Jarvis.app` gebouwd en geverifieerd. iOS-project gegenereerd; simulator-build (iOS 26.3, iPhone 17) draait en geverifieerd met screenshot. Fase 0 afgerond.
- JAR-100: identity-datamodel (`users`/`devices`/`device_keys`, migratie 0002) + `jarvis-identity`-crate (repository + tests); CI uitgebreid met Postgres-service voor `#[sqlx::test]`-integratietests. Build/clippy/fmt/test groen.
