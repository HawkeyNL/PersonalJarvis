# Development

How to build and run the Jarvis backend locally. Architecture and design live in
the blueprint docs (start at `00-start/02-reading-order.md`); the code layout is
described in `decisions/ADR-020-REPOSITORY-LAYOUT.md`.

## Prerequisites

- **Rust** stable via rustup (pinned in `rust-toolchain.toml`)
- **Node 24** via nvm (`nvm use`, pinned in `.nvmrc`) — needed later for the client
- **Docker** (local Postgres)

## Run the backend

```bash
# 1. Start Postgres (waits until healthy)
docker compose -f deploy/compose/docker-compose.yml up -d --wait

# 2. Configure environment
cp .env.example .env          # adjust if needed

# 3. Run the API — applies migrations on startup
cargo run -p jarvis-api
```

Endpoints:

| Method | Path      | Purpose                          |
|--------|-----------|----------------------------------|
| GET    | `/livez`  | liveness — no external deps      |
| GET    | `/readyz` | readiness — pings Postgres       |
| GET    | `/`       | service info                     |

## Checks (identical to CI)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Layout

- `services/api` — Axum HTTP API (`jarvis-api`)
- `crates/config` — typed configuration (`jarvis-config`)
- `crates/observability` — logging/tracing (`jarvis-observability`)
- `migrations/` — SQLx migrations (Postgres)
- `deploy/compose/` — local Docker stack

## Notes

- Secrets never go in code or logs. `.env` is git-ignored and `database_url` is
  redacted from the config `Debug` output.
- The desktop client (`apps/desktop`, Tauri 2 + Vue 3) is not scaffolded yet.
