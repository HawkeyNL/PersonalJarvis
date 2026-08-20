# ADR-036 — Root-productgrenzen voor API, Core en app

## Status

Accepted — 20 augustus 2026.

## Context

Na ADR-035 is blueprint-documentatie uit de root verplaatst. De resterende
productcode zat echter nog in generieke paden (`services/api` en `apps/client`),
terwijl de Home Node, release-artefacten en gebruikerscommunicatie spreken over
Jarvis API, Jarvis Core en Jarvis App.

## Besluit

De repository gebruikt expliciete productmappen op de root:

```text
jarvis-api/     # Axum transport/BFF, pakket en binary `jarvis-api`
jarvis-core/    # native Core-runtimegrens en beschermde Jarvis-persona
jarvis-app/     # Tauri/Vue-clients voor macOS, iOS en toekomstige platforms
crates/         # gedeelde domein-, policy- en infrastructuurlibraries
deploy/         # Compose en Home-Node-systemd/release-assets
```

`jarvis-core` begint als kleine Rust-library voor de canonieke persona en als
beschermd releasebestand. De bestaande `jarvis-api`-binary blijft het native
systemd-proces tijdens deze migratie. Er wordt geen tweede daemon toegevoegd en
geen policy, signed-approval, sandbox of executor naar Core gekopieerd:
`jarvis-policy` en de bestaande gespecialiseerde crates blijven autoritatief.

## Gevolgen

- Cargo-workspaceleden en lokale ontwikkelpaden volgen de nieuwe mappen.
- De release blijft een `jarvis-api`-binary plus
  `jarvis-core/Jarvis.md` publiceren; updater- en rollbackgedrag blijven gelijk.
- Agent-sandbox en Claude Code beschermen voortaan `jarvis-core/**` en
  `.git/**` tegen lezen en schrijven.
- Een latere extractie van chat/orchestratie naar `jarvis-core` vereist een
  aparte, gedragsgeteste wijziging. Deze ADR verandert geen HTTP-contracten,
  risicobeslissingen of executierechten.
