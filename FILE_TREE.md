# Repositoryindeling

```text
.
├── jarvis-api/              # native Rust API-binary (Home Node/systemd)
├── jarvis-core/             # beschermde persona + Core-librarygrens
├── jarvis-app/              # Tauri/Vue desktop- en mobiele clients
├── crates/                  # Rust-librarycrates
├── deploy/                  # lokale Compose en Home-Node-systemd
├── schema/                  # versiebeheer van het SurrealDB-schema
├── decisions/               # architecture decision records
├── security/                # verplichte securitystandaarden
├── docs/
│   ├── blueprint/           # product-, domein- en architectuurblueprints
│   ├── AI_*.md              # actuele implementatie- en securityrichtlijnen
│   └── HOME_NODE_DEPLOYMENT.md
├── README.md                # productkaart en lokale snelstart
├── AGENTS.md                # verplichte agentregels
├── STATUS.md                # actuele projectstatus
├── TODOS.md                 # centrale backlog
└── STEPS.md                 # aanbevolen bouwvolgorde
```

Voor de blueprint begin je bij
[`docs/blueprint/00-start/02-reading-order.md`](docs/blueprint/00-start/02-reading-order.md).
De volledige mapindeling is vastgelegd in ADR-020, ADR-035 en ADR-036.
