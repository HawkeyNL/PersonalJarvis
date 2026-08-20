# ADR-020 — repository layout (code naast blueprint-docs)

## Status

Superseded in part by ADR-035 en ADR-036 — 20 augustus 2026. De code-layout
blijft leidend; de rootlocatie van blueprint-documentatie is vervangen door
`docs/blueprint/` en productgrenzen staan onder `jarvis-api/`, `jarvis-core/`
en `jarvis-app/`.

## Context

De repository bevatte de volledige blueprint als markdown op de root
(`architecture/`, `backend/`, `client/`, `api/`, `infra/`, ...). Die
mapnamen zijn tegelijk logische plekken voor code, wat botst. Code moet
duidelijk gescheiden blijven van de ontwerpdocumenten.

## Besluit

Code komt in nieuwe, niet-botsende mappen. De blueprint-documentatie staat
sinds ADR-035 onder `docs/blueprint/`:

```text
/Cargo.toml            # Rust workspace (root)
/rust-toolchain.toml   # stable, met rustfmt + clippy
/jarvis-api/           # native API-binary voor de Home Node
/jarvis-core/          # beschermde persona + toekomstige orchestratiegrens
/jarvis-app/           # Tauri 2 + Vue 3 clients
/crates/<lib>/         # library-crates (jarvis-config, jarvis-observability, later domain-core)
/schema/               # versiebeheer van het SurrealDB-schema
/deploy/compose/       # Docker Compose voor lokale ontwikkeling
/.github/workflows/    # CI
```

`jarvis-api/`, `jarvis-core/` en `crates/` volgen de modular-monolith uit
ADR-003: één Rust-workspace met duidelijke productgrenzen, verder opgesplitst
wanneer daar een concrete runtimeverantwoordelijkheid voor is.

## Reden

- Geen naamconflict tussen docs en code.
- Docs blijven op de root vindbaar voor mens en coding-agents.
- Productgrenzen zijn direct herkenbaar vanaf de repositoryroot.

## Gevolgen

- Nieuwe domeincrates komen onder `crates/` (bv. `crates/domain-core`).
- Productie-compose (reverse proxy, interne netwerken) blijft zoals in
  `infra/DOCKER_AND_DEPLOYMENT.md`; `deploy/compose/` is alleen voor dev.
