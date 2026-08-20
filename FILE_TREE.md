# Repositoryindeling

```text
.
├── apps/                    # desktop- en mobiele Jarvis-clients
├── crates/                  # Rust-librarycrates
├── services/                # Rust-binaries, waaronder jarvis-api
├── deploy/                  # lokale Compose en Home-Node-systemd
├── schema/                  # versiebeheer van het SurrealDB-schema
├── core/                    # beschermde runtime-persona (`Jarvis.md`)
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
De volledige mapindeling is vastgelegd in ADR-020 en ADR-035.
