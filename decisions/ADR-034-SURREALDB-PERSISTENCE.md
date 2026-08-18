# ADR-034 — SurrealDB als Jarvis Core datastore

**Status:** Accepted (2026-08-17)

## Context

De huidige pre-productie Core gebruikt PostgreSQL/SQLx. De eigenaar heeft
besloten direct naar SurrealDB over te stappen vóór de eerste Home Node- of
beta-deployment. Er is geen productiegegevensset die verwijderd of live
gemigreerd moet worden.

Identity, device-signed approvals, nonce-replaypreventie, audit trails en
exacte portfolio-bedragen zijn security- en correctness-boundaries. Een halve
omschakeling, of tijdelijk terugvallen op PostgreSQL, is geen acceptabel
deploymentresultaat.

## Decision

Jarvis Core gebruikt na voltooiing uitsluitend een zelf-gehoste SurrealDB 2.6
instance als persistente datastore. De Rust SDK en server blijven op dezelfde
gepinnde 2.6-releasefamilie. Core maakt alleen een versleutelde WebSocket-
verbinding naar een private database-endpoint en gebruikt een database-scoped
service account; het SurrealDB-rootaccount is uitsluitend voor provisioning.

Het schema is versioned in `schema/surreal/`. Tabellen zijn `SCHEMAFULL` en
hebben `PERMISSIONS NONE`; clients krijgen nooit directe databasecredentials.
Geld en hoeveelheden worden als gevalideerde decimale strings opgeslagen en in
Rust met `rust_decimal` berekend — niet als IEEE-754-floats. De afzonderlijke
`llm_usage.cost_eur` blijft een bestaande best-effort budgetschatting en is
geen portfolio-geldwaarde.

De migratievolgorde is:

1. persistence boundary + gesloten schema;
2. identity, sessions, unlock en approval/replayclaim met regressietests;
3. audit, portfolio, usage, chat en voice;
4. API/startup, CI en Home Node Compose/systemd-documentatie;
5. pas daarna de PostgreSQL-migrations, SQLx en Postgres-service verwijderen.

Elke stap blijft bouwbaar en testbaar. Core start fail-closed wanneer de schema
versie onbekend is of een schema-upgrade faalt. Er wordt geen automatische
destructieve import, volumeverwijdering of rollback uitgevoerd.

## Consequences

- De minimale Rust-versie stijgt van 1.80 naar 1.82, passend bij SurrealDB 2.6.
- De eerste deployment vereist een geteste Surreal export/import-back-up en een
  herstelcontrole voordat persistente gebruikersdata wordt toevertrouwd.
- De bestaande SQLx/PostgreSQL-code blijft uitsluitend gedurende de broncode-
  portering aanwezig; hij mag niet naast SurrealDB als production runtime
  blijven bestaan.
- SurrealDB's graph/vectormogelijkheden zijn geen autorisatie- of risk-engine;
  `jarvis-policy` en de bestaande approval-boundaries blijven de autoriteit.
