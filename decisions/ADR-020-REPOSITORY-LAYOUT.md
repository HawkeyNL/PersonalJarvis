# ADR-020 — repository layout (code naast blueprint-docs)

## Status

Superseded in part by ADR-035 — 20 augustus 2026. De code-layout blijft
leidend; de rootlocatie van blueprint-documentatie is vervangen door
`docs/blueprint/`.

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
/services/<bin>/       # binaire crates (jarvis-api, later orchestrator, broker-gateway)
/crates/<lib>/         # library-crates (jarvis-config, jarvis-observability, later domain-core)
/apps/<client>/        # Tauri 2 + Vue 3 clients
/migrations/           # SQLx migraties (Postgres)
/deploy/compose/       # Docker Compose voor lokale ontwikkeling
/.github/workflows/    # CI
```

`services/` en `crates/` volgen de naamgeving uit
`infra/DOCKER_AND_DEPLOYMENT.md` (`services/api`, `services/orchestrator`,
`services/broker-gateway`). De crate-indeling volgt de modular-monolith
uit ADR-003 en `backend/crates/`: begin met één workspace, splits later.

## Reden

- Geen naamconflict tussen docs en code.
- Docs blijven op de root vindbaar voor mens en coding-agents.
- `services/` + `crates/` sluit aan op de bestaande blueprint-conventies.

## Gevolgen

- Nieuwe domeincrates komen onder `crates/` (bv. `crates/domain-core`).
- Productie-compose (reverse proxy, interne netwerken) blijft zoals in
  `infra/DOCKER_AND_DEPLOYMENT.md`; `deploy/compose/` is alleen voor dev.
