# Development

How to build and run the Jarvis backend locally. Architecture and design live in
the blueprint docs (start at `00-start/02-reading-order.md`); the code layout is
described in `decisions/ADR-020-REPOSITORY-LAYOUT.md`.

## Prerequisites

- **Rust** stable via rustup (pinned in `rust-toolchain.toml`)
- **Node 24** via nvm (`nvm use`, pinned in `.nvmrc`) — for the client (`apps/client`)
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

## Run the client (macOS)

```bash
cd apps/client
nvm use                          # Node 24 (see .nvmrc)
npm install
npm run tauri dev                # hot-reload dev window
# or build a native .app bundle:
npm run tauri build -- --bundles app
# -> apps/client/src-tauri/target/release/bundle/macos/Jarvis.app
```

Stack: Tauri 2 + Vue 3 + TypeScript, Pinia, Vue Router. The `greet` command
(`src-tauri/src/lib.rs`) demonstrates the JS<->Rust bridge.

iOS also builds (needs full Xcode + an iOS simulator runtime):

```bash
npm run tauri ios init                       # once
npm run tauri ios build -- --target aarch64-sim --ci
xcrun simctl install booted <path/to/Jarvis.app> && \
  xcrun simctl launch booted com.hawkeynl.jarvis
```

## Checks (identical to CI)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Integration tests (`#[sqlx::test]`) need Postgres running and `DATABASE_URL` set
(they create throwaway databases):

```bash
docker compose -f deploy/compose/docker-compose.yml up -d --wait
DATABASE_URL=postgres://jarvis:jarvis_dev_pw@localhost:5432/jarvis cargo test --all
```

## Layout

- `services/api` — Axum HTTP API (`jarvis-api`)
- `crates/config` — typed configuration (`jarvis-config`)
- `crates/observability` — logging/tracing (`jarvis-observability`)
- `crates/identity` — user/device model + repository (`jarvis-identity`)
- `migrations/` — SQLx migrations (Postgres)
- `deploy/compose/` — local Docker stack
- `apps/client` — Tauri 2 + Vue 3 client (macOS + iOS)

## Notes

- Secrets never go in code or logs. `.env` is git-ignored and `database_url` is
  redacted from the config `Debug` output.
- The client (`apps/client`, Tauri 2 + Vue 3) builds a native macOS app; iOS is
  the next target. Its `src-tauri` crate is excluded from the root Cargo
  workspace (see ADR-020) so backend CI stays fast.
