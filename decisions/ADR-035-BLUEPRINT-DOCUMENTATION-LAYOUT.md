# ADR-035 — Blueprint-documentatie onder `docs/blueprint`

## Status

Accepted — 20 augustus 2026.

Supersedes the documentation-location portion of ADR-020.

## Context

De repository groeide vanuit een Markdown-blueprint. Daardoor stonden tientallen
documentatiemappen op de repository-root naast runtime-code en operationele
mappen. Namen als `api/`, `client/`, `database/` en `risk/` waren uitsluitend
blueprint-documentatie, maar leken op productcode. Dat maakt navigatie en de
latere introductie van duidelijke productgrenzen onnodig verwarrend.

## Besluit

Alleen blueprint-documentatie verhuist naar `docs/blueprint/<onderwerp>/`.
Runtime- en operationele mappen blijven op de root:

```text
apps/       # clients
crates/     # Rust-librarycrates
services/   # Rust-binaries, waaronder de API
deploy/     # Compose en systemd
schema/     # versioned SurrealDB-schema
core/       # beschermde Jarvis-persona/runtime-configuratie
docs/blueprint/
            # product-, architectuur-, domein- en securityblueprints
```

`README.md`, `AGENTS.md`, `STATUS.md`, `TODOS.md`, `STEPS.md` en
`CHANGELOG.md` blijven op de root als navigatie- en governance-ingangspunten.

`core/Jarvis.md` blijft eveneens op zijn bestaande pad. De API en de
Home-Node-release bundelen dit bestand als runtime-persona; het valt bovendien
onder de beschermde `core/**`-grens. Het is dus geen gewone blueprintmap.

## Gevolgen

- Nieuwe ontwerpdocumentatie hoort onder `docs/` of `docs/blueprint/`, niet op
  de root.
- Deze wijziging hernoemt of verplaatst geen productcode. Een toekomstige
  product-topologie zoals `jarvis-app` of `jarvis-core` vereist een afzonderlijk
  ADR en een getest migratieplan voor Cargo-, Tauri-, CI- en deploymentpaden.
- Verwijzingen naar de oude blueprintlocaties moeten naar `docs/blueprint/`
  worden bijgewerkt.
