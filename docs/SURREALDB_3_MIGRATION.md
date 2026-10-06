# SurrealDB 3 migration plan (2.6.5 → 3.3.x)

Status: **plan only.** No code, schema, CI or deployment change is part of
this document. It was written against `origin/main` at `76668a7` (Core
v0.0.49) on 2026-10-04.

## How to read the evidence tags

Every claim that is not plain repository inventory has one of these tags:

- **[src]** checked by reading source code. For 2.6.5 that is the
  `surrealdb` and `surrealdb-core` crates in the local Cargo registry. For
  3.3.0 it is the `surrealdb`, `surrealdb-types`, `surrealdb-core` and
  `surrealdb-syn` crates downloaded from crates.io. Reading source is not
  running it: these claims still need the runtime spike (PR0).
- **[docs]** taken from official SurrealDB documentation or release pages.
  Several pages were only available as fetched summaries, so the exact
  wording may differ.
- **[unverified]** an assumption or an open point. Do not rely on it
  until PR0 or the drill confirms it.

Line numbers come from `76668a7`.

## 1. Summary

1. **The Core and the database container must switch together.** [src]
   - The 2.6.5 SDK speaks the WebSocket subprotocol `revision`. The 2.6.5
     server offers `json`, `cbor`, `msgpack`, `bincode` and `revision`.
   - The 3.3.0 SDK asks only for `flatbuffers`. The 3.3.0 server offers
     `json`, `cbor` and `flatbuffers`.
   - So neither SDK can talk to the other server version, even though the
     docs say SDK 3.x "works with servers v2.0.0 and later". No single Core
     binary can run against both engines, which also rules out a CI matrix
     that tests one Core build against both 2.6.5 and 3.3.
   - Compatibility at the SurrealQL level can still be built ahead of time
     (PR1).
2. **The on-disk RocksDB format is not compatible.** [docs]
   - A 3.x binary cannot open the 2.x data directory.
   - The supported path is a `--v3` export from the 2.x side followed by
     `surreal import` into an empty 3.x datastore. PR0 used the pinned
     2.6.5 image's own `export --v3`; the 3.3 `surreal v2` wrapper is not
     usable (§9).
   - Existing `jarvis-backup` archives hold plain 2.x exports. They cannot
     be imported into 3.x directly.
3. **SurrealQL in Jarvis has five confirmed breaking hits and one existing
   bug**; details in §3.
   - [src] `time::from::unix` and `type::thing` no longer exist.
   - [src] `FLEXIBLE TYPE` must be rewritten as `TYPE … FLEXIBLE`.
   - [docs] A fresh install selects from a table that does not exist yet.
   - [docs] Usage logging writes three fields that the SCHEMAFULL table
     does not define.
   - [src] The existing bug: `PUT /v1/system/brain` uses
     `owner_brain_preferences:$id`, which the 2.6.5 parser already rejects,
     so it fails today.
4. **The SDK changes affect every query call site.**
   - [src] `bind()` and `take()` now require `SurrealValue` instead of serde.
   - [src] `Vec<u8>` and `time::OffsetDateTime` have no `SurrealValue`
     implementation.
   - [src] A JSON `null` in a `json!` binding becomes `NULL` on 3.x. On
     2.6.5 it becomes `NONE`.
   - [docs] `option<T>` fields reject `NULL` on 3.x. Many `json!`
     bindings carry optional values, so this is the silent semantic change
     most likely to break writes.
   - About 128 `query`, 83 `bind` and 77 `take` call sites in 22 files
     (counted with grep, so approximate).
5. **The updater has no path for changing the database engine.**
   - The SurrealDB image digest lives in host configuration
     (`/etc/jarvis/surrealdb.env`). Releases never change it.
   - `--migrate-staged` only covers schema migrations (targets 8–10), and its
     cold snapshot can only restore RocksDB files for the same engine.
6. **Urgency is moderate, not an emergency.**
   - The critical advisories fixed only in 3.3.0 are cross-tenant issues.
     The advisories say they do not affect single-tenant or root-scope
     deployments, and Jarvis uses one namespace and one database.
   - The unauthenticated RPC crash (< 3.1.0) needs network reach to port
     8000. That port is published on loopback only.
   - What raises urgency: any local workload that can reach host loopback.
     See decision 3 in §8.
   - Recommendation: do the staged plan below, with a full rehearsal on a
     copy before touching the Home Node.

## 2. Inventory

### 2.1 Schema files and migration runner

- **Schema files:** `schema/surreal/0001_baseline.surql` …
  `0010_coding_reservations.surql`, 384 lines, 23 tables.
- **Statement shape:**
  - Every file is one `BEGIN TRANSACTION; … COMMIT TRANSACTION;` block.
  - Tables are all `SCHEMAFULL PERMISSIONS NONE`.
  - Most files end with `UPDATE schema_version:baseline SET version = N`.
    0001 uses `UPSERT` instead.
- **Clauses used:** `TYPE`, `option<…>`, `array<string>`, `bytes`,
  `datetime`, `float`, `int`, `object`, `ASSERT $value …`, `UNIQUE`
  indexes (including on `bytes` columns and composite indexes), and
  `IF NOT EXISTS`.
  - `OVERWRITE` and `FLEXIBLE` each appear exactly once, in
    `0010_coding_reservations.surql:31`:
    `DEFINE FIELD OVERWRITE checkpoint ON coding_sessions FLEXIBLE TYPE option<object>;`.
  - There are no `VALUE`, `DEFAULT`, `READONLY`, futures, events,
    analyzers, search or vector indexes, relations or `DEFINE ACCESS`.
- **`id` fields:** 19 tables declare `DEFINE FIELD id ON <t> TYPE string`.
  - Records are created as `CREATE <t> SET id = $id, …` with a string UUID.
  - Lookups use `WHERE record::id(id) = $id`.
- **Runner:** `crates/store/src/lib.rs:82-337`.
  - It reads `SELECT version FROM schema_version:baseline`, then applies
    each missing file in order with `.query(include_str!(…)).check()`.
  - It refuses unknown versions (`UnsupportedSchema`).
  - A version-10 database never runs any file again. So statements without
    `IF NOT EXISTS` only run on fresh or older databases.
- **Who runs migrations:** Core, at startup, as the database-scoped
  `EDITOR` user that `deploy/surrealdb/provision-core-user.sh` creates
  (`DEFINE USER … ON DATABASE … ROLES EDITOR`).
  - All CI tests sign in as **root**, so the `EDITOR` path is never tested
    in CI.
- **Schema fingerprint:** `scripts/release/build-linux.sh:279-296` hashes
  every `.surql` file into `schema_sha256`.
  - Editing **any** schema file, comments included, changes the fingerprint.
  - It also changes which releases the updater accepts as rollback
    candidates and as migration sources.

### 2.2 Runtime SurrealQL

The statements were extracted from all Rust string literals.

- **Statements:** about 140 distinct statements.
  - Mostly single-statement `SELECT`, `CREATE`, `UPDATE`, `DELETE` and
    `UPSERT`.
  - 10 multi-statement transactions with `LET`, `IF … { THROW … }` and
    `array::len`.
- **Functions:** `time::now` (136×), `record::id` (102×), `array::len`,
  `time::format`, `time::group`, `time::max`, `math::sum`,
  `math::percentile`, `count()`, `time::from::unix` (1×) and
  `type::thing` (1×).
- **Casts:** `<bytes>`, `<datetime>`, `<duration>`.
- **Clauses:**
  - `RETURN NONE|AFTER|record::id(id) AS id`, `GROUP ALL`, `GROUP BY`,
    `ORDER BY … LIMIT`, `ONLY`, and one subquery in `FROM`.
  - About 25 conditional (compare-and-swap) `UPDATE … WHERE status = …`
    statements, which security and budget logic depend on.
- **Not used:** `SPLIT`, `FETCH`, the `~` operators, optional chaining,
  the `.id` idiom on record ids, `rand::*`, `math::min` or `math::max`,
  live queries, graph edges, record links, numeric record ids, `INFO`
  (in Rust) and SET clauses that read other fields.

### 2.3 Rust SDK surface

- **Dependency:** `Cargo.toml:48` declares
  `surrealdb = { version = "2.6.5", default-features = false, features = ["protocol-ws", "rustls"] }`.
  - Used by `jarvis-store`, `jarvis-identity`, `jarvis-portfolio`,
    `jarvis-usage` and `jarvis-api` (including the `jarvis-codex-broker`
    and `jarvis-config-broker` binaries).
- **Connection:**
  - `Surreal::new::<Ws>(endpoint)`, then `signin(opt::auth::Database { namespace, database, username, password })`
    with `&str` fields, then `use_ns().use_db()` (`crates/store/src/lib.rs:53-76`).
  - The endpoint is plain `ws://` to `127.0.0.1:8000`
    (`deploy/systemd/generate-core-env.sh:61`). `Wss` is not used anywhere,
    so the `rustls` feature is unused.
- **Queries:** `db.query(&str|String)`, `.bind(…)`, `.await`,
  `.take(n)` into `Option<T>` or `Vec<T>`, and `.check()`.
  - There are no typed CRUD methods (`db.select/create/…`), no SDK
    transactions, no live queries, and no `Thing`, `RecordId`, `sql::*` or
    `surrealdb::Value`.
- **Bindings:** two forms.
  - `serde_json::json!({…})`, about 70 sites.
  - `#[derive(Serialize)]` structs using `#[serde(with = "serde_bytes")]`,
    `uuid::serde::hyphenated` and `#[serde(flatten)] serde_json::Value`.
- **Rows:** `#[derive(Deserialize)]` structs that use `serde_bytes`,
  `time::serde::rfc3339[::option]` and `Option<f64>` for aggregates.
- **Errors:** `StoreError` boxes `surrealdb::Error`, and identity maps every
  error to an opaque `DatabaseSurreal`.
- **Tests:** about 15 `#[ignore]` tests use `opt::auth::Root { username: &…, password: &… }`
  and need a live server (`JARVIS_SURREAL_TEST_*`).

### 2.4 Containers, backup and restore

- **Production engine:** `deploy/surrealdb/docker-compose.yml`.
  - Image `${SURREALDB_IMAGE}`, a digest held in root-only
    `/etc/jarvis/surrealdb.env`.
  - `start rocksdb:/data/jarvis.db` with volume
    `/var/lib/jarvis/surrealdb:/data`, port published on `127.0.0.1:8000`
    only.
  - Root credentials come from `SURREAL_USER`/`SURREAL_PASS` in the
    environment.
  - Limits: `mem_limit: 2g`, `cpus: 2`, `cap_drop: ALL`.
  - The unit is `jarvis-surrealdb.service`, started through
    `start-production-surrealdb.sh`.
- **Digest pins:** `initialize-production-surrealdb.sh:9`,
  `surrealdb.env.example:4` and
  `tests/test-provision-core-user-shellless.sh:39` all pin
  `sha256:d653f6c8…`.
  - [unverified] That this digest is 2.6.5. Confirming it needs
    `docker image inspect`, which is outside this task.
- **Development stack:** `deploy/compose/docker-compose.yml:7` uses
  `surrealdb/surrealdb:v2.6.5`.
- **`jarvis-backup`** (`deploy/systemd/jarvis-backup.sh`):
  - It takes per-table counts before and after the export, using
    `INFO FOR DB` (`:227`) and `SELECT count() … GROUP ALL`.
  - It runs `surreal export --auth-level root` inside the production
    container (`:357`).
  - It imports the export into a disposable container (`--network none`,
    `start --unauthenticated memory`, 1 GiB) started from **the live image
    digest** (`:243-254`), then compares counts.
  - It encrypts to the owner's keys and keeps **one** archive (`keep=1`).
  - The manifest records the image digest.
- **`schema-backup`** (`deploy/systemd/schema-backup.sh`):
  - It makes a cold `cp -a` copy of `/var/lib/jarvis/surrealdb` with Core
    and SurrealDB stopped, plus sha256 checksums.
  - `restore` swaps the directories with `mv`; `commit` forbids any later
    rewind (`:104`).
  - The snapshot is raw RocksDB, so it can only be restored into the same
    engine major version.
- **Disaster recovery** (`docs/BACKUP_AND_RESTORE.md`) restores
  `/etc/jarvis` first. That brings back the old `SURREALDB_IMAGE`; the
  export is then imported with `surreal import`.

### 2.5 Updater

- **Schema migrations:** `deploy/systemd/update-core-release.sh` handles
  them only through `--stage` plus `--migrate-staged`, which
  `jarvis update --migrate <tag>` wraps. It works like this:
  - The candidate's `release.json` must declare
    `schema_migration.version == 1`, a target of 8, 9 or 10 (`:147`), and
    the active fingerprint in `from_sha256`.
  - `schema-backup create` takes a cold snapshot, then
    `activate_managed_release` runs and Core migrates itself at startup.
  - If `readyz` passes, `commit` is called. Otherwise
    `restore_release_transaction` runs (`:875-898`), which restores the
    snapshot and the previous release.
- **What it never touches:** the SurrealDB image or
  `/etc/jarvis/surrealdb.env`. The compose file at
  `/opt/jarvis/surrealdb/docker-compose.yml` is installed once by
  `prepare-home-node.sh`.

### 2.6 CI and test fixtures

- **Rust database tests:** `.github/workflows/ci.yml:76-89` and
  `release.yml:31-44` start `surrealdb/surrealdb:v2.6.5 … memory` and run
  the ignored database tests as root:
  - identity: replay and account tests
  - store: migration tests
  - portfolio
  - jarvis-api: `surreal_api`, including the realtime and account modules
  - `jarvis-codex-broker`
- **Container fixtures:** `scripts/ci/verify-deployment-security.sh` runs
  three tests against real containers:
  - `test-schema-backup-surreal.sh` (`v2.6.5`, `:19`)
  - `test-jarvis-backup-surreal.sh` (pulls `v2.6.5` and pins its digest,
    `:21-22`)
  - `test-provision-core-user-shellless.sh` (digest `d653f6c8…`)
- **Docs:** `docs/REALTIME_PROTOCOL.md` and `docs/REALTIME_VERIFICATION.md`
  mention 2.6.5.

## 3. Breaking changes against Jarvis

### 3.1 SurrealQL and schema (server side)

| # | 3.x change | Hits Jarvis? | Where | Fix | Evidence |
|---|---|---|---|---|---|
| B1 | `time::from::*` renamed to `time::from_*`. 2.6.5 has no `time::from_unix`. | **Yes** | `crates/identity/src/account.rs:33` | Dual-compatible: bind the expiry as an RFC 3339 string and compare `time::now() >= <datetime>$expires` (the cast already exists in `chat.rs`). | [src] both function tables |
| B2 | `type::thing` removed; `type::record(table, key)` replaces it. 2.6.5 has `type::record`, but with different arguments (`(rid, table?)`). | **Yes** | `account.rs:254` | Dual-compatible: bind a record id from Rust and write `UPSERT $rid SET …`. With SDK 2 use `surrealdb::RecordId::from_table_key`; with SDK 3, `surrealdb::types::RecordId::new`. | [src] |
| B3 | `FLEXIBLE` must follow `TYPE`. 3.3 errors with "FLEXIBLE must be specified after TYPE". 2.6.5 parses clauses in any order. | **Yes**: fresh installs and every database below v10 | `schema/surreal/0010_coding_reservations.surql:31` | Change to `DEFINE FIELD OVERWRITE checkpoint ON coding_sessions TYPE option<object> FLEXIBLE;`. 2.6.5 also accepts this. **This changes the schema fingerprint**, so it ships in the engine-switch release (§7). | [src] both parsers |
| B4 | Selecting from a table that does not exist now errors instead of returning `[]`. | **Yes**: fresh installs (empty database) | `crates/store/src/lib.rs:84,104,124,144,164`. The first select runs before `0001` creates `schema_version`. | Dual-compatible: check the table exists before selecting, e.g. `RETURN (INFO FOR DB).tables.schema_version != NONE;`, then select. Also check that `EDITOR` may run `INFO FOR DB` [unverified]. | [docs] |
| B5 | SCHEMAFULL rejects undefined fields. 2.x silently drops them. | **Yes**: every LLM usage write would fail | `crates/usage/src/surreal.rs:29` writes `requested_route`, `actual_provider`, `cost_estimate_classification`, which no schema file defines. They are silently dropped today. | Either stop writing them (keeps today's stored data exactly; smallest change) or add them in a new migration 0011 (decision 6 in §8: add migration 0011). | [docs]; schema checked by script |
| B6 | Record id `table:$param` is rejected. **2.6.5 already rejects it** with the same parse error. | **Existing bug** | `jarvis-api/src/routes/system.rs:62` (`UPSERT owner_brain_preferences:$id`). Setting the owner brain preference cannot work today. The result is also never `.check()`ed, and no test covers it. | Separate bug-fix PR: bind a record id (as in B2), `.check()` the result, add a test. | [src] both parsers |
| B7 | A JSON `null` binding becomes `NULL` (3.3) instead of `NONE` (2.6.5); `option<T>` rejects `NULL`. | **Probably yes, wide** | Every `json!` binding with an `Option` value, e.g. `usage/src/surreal.rs:36-44`, `audit.rs`, `routes/chat.rs`, `routes/coding.rs`, the codex broker. | In PR2, use typed binding structs: `Option::None` serialises to `NONE` (3.3 `serialize_none` → `Value::None`, [src]). Or check every `json!` site. Covered by the full database test suite on 3.3. | [src] mapping; [docs] NULL rejection |
| B8 | 3.3: `UPDATE`/`UPSERT` evaluate `WHERE` before applying data. | No breakage expected: our compare-and-swap updates already rely on pre-state `WHERE` | about 25 conditional updates (identity, agent approvals, budget, broker) | Keep the existing replay and compare-and-swap tests; they must pass on 3.3. | [docs] |
| B9 | In `SET`, every expression reads the record's state from before the statement. | No hit: no `SET` right-hand side reads another field (checked by script) | none | none | inventory |
| B10 | The `id` field is validated against its declared kind when the record id is generated. | No hit expected: ids stay strings | 19 `DEFINE FIELD id … TYPE string` | Keep binding ids as **strings**. A `uuid::Uuid` bound through SDK 3 becomes a SurrealDB `uuid` value and would break the `string` id kind and every `record::id(id) = $id` lookup. | [src] 3.3 `doc/field.rs` |
| B11 | `UPSERT <table>` with no id and no `WHERE`. Semantics in 3.x are not documented for this shape. | Unknown | `jarvis-api/src/routes/voice.rs:100` | Test it in PR0. If it differs, bind `voice_profiles` by a deterministic record id (this needs a data fix, because existing rows have random ids). | PR0: see §9 |
| B12 | `RETURN <param> AS alias` on `DELETE`. | Unknown | `routes/chat.rs:778` | Test it in PR0. | PR0: see §9 |
| B13 | `math::min`/`max` on empty arrays, the `.id` idiom, optional chaining, `GROUP`+`SPLIT`, `LIKE` operators, `MTREE`/`SEARCH`, futures, `rand::guid`, compulsory `LET`, numeric id ordering, `<set>`. | No hit | none (all variables already use `LET`) | none | inventory |
| B14 | Field evaluation follows dependency order (3.3). | No hit: no `VALUE`/`DEFAULT`, and `ASSERT`s only read `$value` | none | none | [docs] |
| B15 | `DEFINE` statements inside `BEGIN … COMMIT`, and implicit creation of namespace and database on first use. | Unknown | every schema file; tests that use random namespaces | Covered by the 3.3 migration test (fresh path) in PR2. | PR0: see §9 |
| B16 | `math::percentile` and `time::max` on empty or all-`NONE` groups. | Unknown | `usage/src/surreal.rs:161,188` | Covered by the existing usage statistics tests on 3.3. | PR0: see §9 |

### 3.2 Tooling and infrastructure

| # | Change | Where | Fix |
|---|---|---|---|
| T1 | The 2.x RocksDB directory cannot be opened by 3.x. [docs] | `/var/lib/jarvis/surrealdb` | Export with `--v3` into a fresh volume (§5). |
| T2 | 2.x exports (without `--v3`) do not import into 3.x. [docs] | All existing `jarvis-backup` archives | Disaster recovery for older archives (§5.7). |
| T3 | Output shape of `INFO FOR DB`, of `surreal sql --json`, and the `export --auth-level root` flag. [unverified] | `jarvis-backup.sh:214-232,357` | Run `test-jarvis-backup-surreal.sh` on 3.3 (PR3). |
| T4 | 3.3 startup migrates the data format; a 3.2 binary misreads 3.3 data. [docs] | Future 3.x minor upgrades | Pin the exact 3.3.x digest. Every later 3.x minor needs an export first plus its own drill. |
| T5 | Image pins. | `ci.yml:78`, `release.yml:34`, `deploy/compose/docker-compose.yml:7`, `test-schema-backup-surreal.sh:19`, `test-jarvis-backup-surreal.sh:21-22`, `initialize-production-surrealdb.sh:9`, `surrealdb.env.example:4`, `test-provision-core-user-shellless.sh:39`, two realtime docs | Move them together in the engine-switch PRs. |
| T6 | The updater cannot change the engine (§2.5). | `update-core-release.sh` | New engine-migration path (§5.4). |
| T7 | `DEFINE USER … ROLES EDITOR` and its permissions on 3.x (can it still define tables and run `INFO`?). [unverified] | `provision-core-user.sh` | Run the 3.3 migration test as a database `EDITOR`, not as root (PR2). |
| T8 | Server start flags and environment variables (`SURREAL_USER`, `SURREAL_PASS`, `SURREAL_BIND`), `--unauthenticated memory`, `isready`, `/health`. The `start` docs list `--user`/`--pass`, `--unauthenticated` and these backends. [docs] The environment variable names are [unverified]. | compose files, CI, test fixtures | Test in PR0 with the pinned image. |

## 4. Rust SDK 2 → 3

The following was checked against the 3.3.0 crate source [src] unless it is
tagged otherwise.

- **Version and MSRV:**
  - Latest is `surrealdb 3.3.0`, released 2026-09-28. The crate uses
    `edition = "2024"` and declares no `rust-version`.
  - The SDK docs state Rust ≥ 1.95 [docs].
  - Our toolchain is 1.97.1 (`rust-toolchain.toml`), so it is fine.
- **Features:** defaults are `protocol-ws`, `rustls` and `parse`. We
  should keep `default-features = false` with `protocol-ws`, and
  **drop `rustls`**:
  - On native targets `rustls` pulls in `aws-lc-rs`/`aws-lc-sys`, a
    C-built crypto library that is not in today's `Cargo.lock`.
  - Jarvis only uses plain `ws://` on loopback.
  - Check `parse`: it is needed only for client-side parsing, and our
    queries are strings [unverified whether `query()` needs it].
- **Dependency tree:** measured with `cargo tree -e normal`, counting
  unique crates, so approximate.
  - The 2.6.5 subtree is about 324 crates, out of about 446 for the whole
    workspace, because it pulls in all of `surrealdb-core` even for a
    remote client.
  - A probe of 3.3.0 with `protocol-ws`+`rustls` resolves about 199 crates
    and no longer depends on `surrealdb-core`.
  - Expect a smaller tree and shorter builds. `cargo audit` still has to
    run on the new lock file.
- **Connection:** `Surreal::new::<Ws>`, `engine::remote::ws::{Ws, Client}`,
  `use_ns`/`use_db` and `signin` still exist.
  - `opt::auth::{Root, Database}` fields are now **`String`**, not `&str`.
    That affects `crates/store/src/lib.rs:63` and about 15 test sites.
  - `surrealdb::Error` is now a re-export of `surrealdb_types::Error`.
    `StoreError` keeps compiling if it only boxes it [unverified until
    compile].
- **Bindings and results:**
  - `bind(impl IntoVariables)` is implemented for `T: SurrealValue`.
  - `take()` requires `T: SurrealValue` for `Option<T>` and `Vec<T>`.
  - **Serde derives no longer work**: each binding and row struct needs
    `#[derive(SurrealValue)]`.
  - `serde_json::Value` implements `SurrealValue`, so `json!` bindings
    still compile, but see B7 about `null`.
  - `SerdeWrapper<T>` exists as an escape hatch. However, when
    serialisation fails it **returns `NONE` in release builds** (only a
    `debug_assert!` fires).
  - So `SerdeWrapper` must not be used for security-relevant bindings
    (ids, nonces, status values): a silent `NONE` turns a predicate into a
    no-match, or a write into a `NONE` write.
- **Type mapping:**
  - `Vec<u8>` intentionally has **no** `SurrealValue` implementation; use
    `surrealdb::types::Bytes` or `bytes::Bytes` (affects every
    `serde_bytes` field).
  - `uuid::Uuid` maps to a native SurrealDB `uuid`. Keep `String` for ids
    (B10).
  - `chrono::DateTime<Utc>` maps to `datetime`.
  - `time::OffsetDateTime` has **no** implementation. Either convert at
    the boundary (to chrono or RFC 3339 strings) or wrap it in a small
    newtype. Deciding this is part of PR2.
- **Responses:** `query().await` returns `IndexedResults` (which has
  `take`, `take_errors` and `check`). Our code never names the response
  type.
- **3.3 internals:** the engine is now a typed `SurrealEngine` trait, and
  futures are `Send` but not `Sync` [docs].
  - We only `.await` inside axum handlers and tokio tasks.
  - Handlers whose futures must be `Sync` would fail to compile; there
    are none known [unverified until compile].
- **Live queries and SDK transactions:** not used, so not affected.
- **WebSocket protocol:** `flatbuffers` only; see §1.1. This forces Core
  and the engine to switch in lockstep.

## 5. Data migration design for the Home Node

### 5.1 Principles

- **Never modify the 2.6.5 data directory.** All work runs on copies, and
  the original stays in place, untouched, until the owner deletes it.
- **Credentials:**
  - No credential goes into any argv, log or new file.
  - The export and import run inside **networkless, disposable,
    `--unauthenticated`** containers started from copies, so no database
    password is needed for the data move.
- **Plaintext:** the export exists only on `/run` (tmpfs, root 0700) and
  is removed on exit, as `jarvis-backup` already does.
- **Lockstep:** the engine image, the data directory and the Core release
  switch together, and roll back together.

### 5.2 Procedure (helper `surrealdb-engine-migrate`, shipped in the release and checksum-bound like `schema-backup`)

0. **Preconditions**, checked by the helper:
   - The active release is the last 2.x release.
   - A `jarvis-backup create` from today exists, and the owner confirms
     an off-host copy (decision 4 in §8).
   - The 3.3 image digest named in the candidate's `release.json` is
     already pulled.
   - Free space is at least 3 × `du /var/lib/jarvis/surrealdb` + 1 GiB,
     and `/run` has at least 2 × export size.
1. **Quiesce:** stop Core and the brokers, then run
   `schema-backup create <old> <new>`. This produces the cold copy T0 with
   checksums, and the 2.6.5 container is stopped.
2. **Source engine:** reflink-copy T0 to a scratch directory S. Start
   `<2.6.5 digest> start --unauthenticated rocksdb:/data/jarvis.db` on S
   with `--network none`, `--cap-drop ALL` and a memory limit.
3. **Export:** run `<2.6.5 digest> export --v3 --endpoint http://127.0.0.1:8000 --namespace <ns> --database <db> /run/…/export-v3.surql`
   in a container **sharing S's network namespace**
   (`--network container:<S>`).
   - This needs no host network and no new port.
   - PR0 verified these flags against an unauthenticated 2.6.5 source (§9).
   - Do not use the 3.3 `surreal v2 export`: it downloads a 2.7.0 binary
     from the internet after an interactive prompt.
4. **Target engine:** create `/var/lib/jarvis/surrealdb.v3` (root, 0700).
   Start `<3.3 digest> start --unauthenticated rocksdb:/data/jarvis.db` on
   it, networkless.
5. **Import:** run `surreal import` of `export-v3.surql` into the same
   namespace and database. The export includes the database `EDITOR` user
   with its password hash [unverified that passhash survives the format
   change].
6. **Verify** (§5.3). On any mismatch, abort: delete `.v3`, delete S,
   restart 2.6.5 on the untouched original, start the old Core.
7. **Swap**, all as atomic renames on one filesystem:
   - `mv surrealdb surrealdb.v2-<txn>`, then `mv surrealdb.v3 surrealdb`.
   - Rewrite only the `SURREALDB_IMAGE` key in `/etc/jarvis/surrealdb.env`
     (write a temp file, rename it, keep `surrealdb.env.v2-<txn>` at 0600).
   - On the first 3.3 start, root is created from the existing
     `SURREAL_USER`/`SURREAL_PASS` environment [unverified for 3.x].
8. **Activate:** activate the candidate release with
   `activate_managed_release`, then wait for `readyz`.
9. **Smoke test:** run Core-level checks (§5.3). If they fail, roll back
   automatically (§5.6).
10. **Commit:**
    - Mark the transaction committed.
    - Keep `surrealdb.v2-<txn>`, the T0 snapshot and the old env file until
      the owner removes them explicitly (decision 5 in §8).
    - Tell the owner to run `jarvis-backup create` immediately and copy
      the first 3.x archive off the host.

### 5.3 Verification (abort before the swap if anything differs)

1. **Row counts per table:** compare S (2.6.5) with the new 3.3 datastore
   by reusing `jarvis-backup`'s `table_counts` (`INFO FOR DB` plus
   `count()`). Both are static copies, so counts must be **equal**, not
   within a range.
2. **Schema shape:**
   - The table set must match, and every field and index must exist, per
     `INFO FOR TABLE`.
   - Compare names and kinds after normalising the output; the format
     differs between majors [unverified].
   - `schema_version:baseline.version` must equal 10.
3. **Content checksums per table:**
   - Run `SELECT * FROM <t> ORDER BY id` on both sides with `--json`.
   - Normalise with jq: sort keys, canonical datetimes in UTC RFC 3339,
     bytes to hex, record ids as `table:key` strings.
   - Compare sha256 per table.
   - Expected representation differences must be fixed during the drill,
     then frozen. Any remaining difference aborts the migration.
4. **Authentication:** the Core `EDITOR` user signs in to the 3.3 engine
   with the existing password through Core's normal startup. This is the
   real test; `readyz` fails otherwise.
5. **App smoke tests** (read-only, local, after activation):
   - `readyz`
   - an existing device session validates
   - list conversations
   - read the usage month total
   - list coding sessions
   - list pending approvals
   - `jarvis-backup create` produces an archive whose restore test passes
     on 3.3

### 5.4 How the updater and `jarvis update --migrate` orchestrate it

- **Manifest:** the release that carries SDK 3 declares in `release.json`:
  - `"engine": {"surrealdb": "3.3.x", "image": "surrealdb/surrealdb@sha256:…"}`
  - `"schema_migration": {"version": 2, "engine_from": "2.6.5", …}` with
    `from_sha256` holding the **full** fingerprint of the last 2.x release.
    Today's build only lists the 6–9 prefix hashes, which is not enough
    once 0010 is edited (B3).
- **Staging:** `--stage` accepts it only when the updater supports
  `schema_migration.version == 2` and the active engine digest (read as
  one key, not by sourcing the file) is the declared source digest.
- **Running it:** `--migrate-staged` (= `sudo jarvis update --migrate vX`):
  - It replaces the `schema-backup create → activate → commit` steps with
    the §5.2 helper.
  - It reuses the existing traps (ignore INT and HUP, drained output),
    locks and receipt checks.
- **The timer never runs it** (unchanged rule).
- **Rollback candidates:** after the migration, every 2.x release
  automatically stops being a rollback candidate, because its schema
  fingerprint differs. Keep that behaviour, and add a test that asserts
  it.

### 5.5 Downtime, disk and memory

- **Downtime:** Core is down from step 1 to step 8.
  - Expected: minutes for a small single-owner database. Measure it in the
    drill; this estimate is [unverified].
- **Disk:** the original stays, plus T0 (1×), S (1× or a reflink), the new
  3.3 datastore (about 1–2× logical size [unverified]) and the export on
  tmpfs. That is roughly 4× the database plus the export; the helper
  checks before stopping anything.
- **Memory:**
  - Production keeps `mem_limit: 2g`.
  - Disposable containers get 1 GiB, matching `jarvis-backup`.
  - [unverified] Whether 3.3 sizes RocksDB caches from host RAM instead of
    the container limit. Watch for OOM during import in the drill; set an
    explicit cache size if needed.

### 5.6 Rollback, tested in the drill

- **Before commit**, automatic:
  1. Stop Core and the engine.
  2. `mv surrealdb surrealdb.v3-failed-<txn>`, then
     `mv surrealdb.v2-<txn> surrealdb`.
  3. Restore `surrealdb.env` from `surrealdb.env.v2-<txn>`.
  4. Re-point `/opt/jarvis/current` to the previous release and restore its
     units (existing `restore_release_transaction`).
  5. Start the engine and Core, then check `readyz`.
  - The 2.6.5 directory was never opened by 3.3, so this is a pure rename.
- **After commit**, manual, owner only. Any 3.x writes since the upgrade
  are lost:
  1. Stop everything.
  2. Rename the directories back to the retained `surrealdb.v2-<txn>`.
  3. Restore the old env file.
  4. Run `jarvis update --rollback-version <last 2.x tag>`, which needs an
     explicit override flag because the fingerprint differs.
  - This must be written down as a runbook in PR3, and practised once in
    the drill.
- **Fallback of last resort:** the pre-upgrade encrypted `jarvis-backup`
  archive. Restore `etc-jarvis.tar` (which brings back the 2.6.5 digest)
  and import into an empty 2.6.5 datastore, following today's runbook.

### 5.7 Disaster recovery after the upgrade

- **3.x archives** restore as today. `jarvis-backup` already runs its
  restore test with the live digest, so it will test 3.3 automatically.
- **2.x archives** created before the upgrade:
  1. Restore `/etc/jarvis` from the archive (old digest).
  2. Import into a disposable 2.6.5 datastore.
  3. Export with the 2.6.5 image's `export --v3`.
  4. Import into 3.3.
  - PR3 documents this two-step path in `BACKUP_AND_RESTORE.md`, and
    `jarvis-backup verify` should print the archive's engine version.
- **Image availability:** we depend on the 2.6.5 image digest staying
  pullable. Keep a `docker save` tarball of it next to the off-host
  backups (decision 7 in §8).

## 6. Risk register

Likelihood (L) and impact (I) are rated 1–3. The score is L×I.

| Rank | Risk | L | I | Score | Mitigation | Detection |
|---|---|---|---|---|---|---|
| 1 | **Data loss or corruption during export or import** (e.g. a field silently dropped by `--v3` rewriting, bytes or datetime encoding drift, the `checkpoint` flexible object) | 2 | 3 | 6 | Original directory never touched; T0 cold snapshot; encrypted pre-upgrade archive off host; drill on real-shaped data | §5.3 equal counts and per-table content checksums; abort before the swap |
| 2 | **Silent `NULL` vs `NONE` change** in `json!` bindings (B7) breaks writes or changes `= NONE` predicates | 3 | 2 | 6 | Typed `SurrealValue` binding structs; full database test suite on 3.3 | CI on 3.3; write errors in Core logs during smoke tests |
| 3 | **Version skew or partial upgrade**: new Core on the old engine (or the reverse), env digest switched without the directory swap, half-renamed directories | 2 | 3 | 6 | One helper owns the swap; ordered, rename-only steps with a receipt per step; restart resumes from the receipt or rolls back; WS mismatch fails closed at connect | `readyz` fails; helper status command; the drill includes kill-at-each-step |
| 4 | **Authentication or permission change**: `EDITOR` cannot `DEFINE`/`INFO`, imported passhash rejected, root not created from the environment | 2 | 3 | 6 | Run 3.3 migration tests as a database `EDITOR`; drill with real provisioning | Core fails to start; automatic rollback |
| 5 | **Backup format incompatibility**: old archives cannot be restored into 3.x; `keep=1` overwrites the only 2.x archive on the next run | 3 | 2 | 6 | Owner keeps the pre-upgrade archive off host; two-step DR documented; `docker save` of 2.6.5 | `jarvis-backup verify` shows the engine version |
| 6 | **Compare-and-swap semantics drift** (WHERE-before-data, transaction isolation) weakens one-use challenges, approvals or budget reservations | 1 | 3 | 3 | Existing replay and compare-and-swap tests must pass on 3.3; opus review of the PR2 diff | CI |
| 7 | **Fresh-install breakage** (B3, B4) leaves new Home Nodes unable to bootstrap | 3 | 1 | 3 | Fixes in PR1 and the engine-switch release; migration test from an empty database on 3.3 | CI |
| 8 | **Memory or disk exhaustion** during import (cache sizing, 2 GiB limit, `/run` tmpfs) | 2 | 2 | 4 | Space checks before stopping services; limits on disposable containers; measure in the drill | Helper aborts before the swap; OOM kill shows up as an import failure |
| 9 | **Performance regression** (new engine; our `WHERE record::id(id) = $id` lookups are table scans, not index lookups) | 1 | 2 | 2 | Single owner, small tables; time the smoke tests and compare against 2.6.5 in the drill | p95 of the drill smoke run |
| 10 | **New native dependency** (`aws-lc-sys` through `rustls`) breaks reproducible release builds | 2 | 1 | 2 | Drop the `rustls` feature (unused) | `cargo tree`, release build log |
| 11 | **CI flakiness** from the new image start time or health endpoint | 2 | 1 | 2 | Keep the `isready` and `/health` polling with bounds | CI |
| 12 | **Future 3.x minor upgrades** auto-migrate the data format on first start (3.2 → 3.3 precedent) | 2 | 2 | 4 | Pin by digest; each minor engine bump is a planned migration with export-first | Release checklist |
| 13 | **Live queries** | — | — | — | Not used | — |

## 7. Staged rollout

All PRs target an integration branch (`feat/surrealdb-3`) except PR1 and
the bug-fix PR. The reason: once SDK 3 lands, `main` can no longer be
released to a 2.6.5 Home Node (§1.1). The alternative is to freeze `main`
releases while PR2–PR4 are open; decision 1 in §8 rejects that.

- **PR0, spike (CI only, throwaway; no production change).** One workflow
  job with a 3.3 container runs the full schema chain and a corpus of
  every runtime statement shape from §2.2 through `/surreal sql`. It
  resolves every [unverified] item:
  - B11, B12, B15 and B16
  - T3, T7 and T8
  - JSON `null`
  - `v2 export --v3` flags against an unauthenticated 2.6.5 source
  - passhash survival
  - `INFO FOR DB` shape

  Output: a short findings comment in the PR; then delete the job.
- **Bug fix (separate PR to `main`): B6, the owner brain preference
  write.**
  - Bind a record id (`surrealdb::RecordId` in SDK 2), `.check()` the
    result, and add a database test.
  - It goes to `main` on its own because it is broken today, independent
    of the upgrade.
- **PR1 (to `main`, releasable on 2.6.5): dual-compatible SurrealQL.**
  Covers B1, B2 and B4; merged as #89. B5 moves to PR2, which adds
  migration 0011 for the three usage fields (decision 6 in §8).
  - No schema file changes, so the fingerprint is unchanged and this is
    a normal `--version` release.
  - Verification: the existing CI database suite on 2.6.5; the PR0 corpus
    shows the new forms also parse on 3.3.
- **PR2 (integration branch): SDK 3.3 and the CI switch.**
  - Bump `surrealdb` to `=3.3.x` with `protocol-ws` only. Use
    `SurrealValue` derives and typed bindings that remove `null`s (B7).
  - Use `Bytes`, the time conversion, and `String` auth fields.
  - Edit B3 in `0010`, and add migration `0011` for the B5 usage fields.
  - Switch every CI and test image to the pinned 3.3 digest, and add a
    3.3 migration test that runs as a database `EDITOR` from an empty
    database and from v6–v9.
  - Needs an opus review (auth, approvals, budget compare-and-swap).
- **PR3 (integration branch): engine migration tooling, backup and DR.**
  - Add the `surrealdb-engine-migrate` helper (§5.2–5.6) with
    `--rehearse`, which runs steps 1–6 on copies, restarts the old stack
    and swaps nothing.
  - Update `jarvis-backup` and `schema-backup` for 3.3, and update the
    `BACKUP_AND_RESTORE.md` two-step DR.
  - Add a CI fixture that seeds a real-shaped 2.6.5 database (every table,
    bytes, all `option` fields both set and `NONE`, flexible
    `checkpoint`, unicode, about 10k usage rows), migrates it and checks
    §5.3.
  - Test kill-at-every-step recovery.
- **PR4 (integration branch → `main` as one release): updater support and
  release.**
  - Add `schema_migration.version 2` and the `engine` manifest fields;
    staging and migration checks; the `jarvis update --migrate`
    confirmation text naming the engine change and the downtime; the
    rollback runbook; the docs; and the digest pins.
  - Release notes say explicitly: "requires `jarvis update --migrate`;
    one-way after commit".

### 7.1 Drill on a throwaway copy, before the Home Node

1. **CI:** the PR3 fixture passes on every run, including kill-at-each-step
   and the full rollback.
2. **Throwaway VM, optional** (not the Home Node; owner's workstation or
   a cloud VM; see decision 2 in §8):
   - Install the last 2.x release with the production compose and digest.
   - The owner decrypts the latest real `jarvis-backup` archive **on that
     VM** (the Home Node cannot decrypt) and restores it with today's
     runbook.
   - Run `jarvis update --migrate <candidate>`.
   - Verify §5.3, log in from a real client against the VM, and run the
     smoke tests.
   - Then test **both** rollbacks: before commit (inject a failure) and
     after commit (the manual runbook).
   - Record downtime, peak disk and peak memory.
   - Destroy the VM and the decrypted data afterwards.
3. **Home Node rehearsal:** `surrealdb-engine-migrate --rehearse`.
   - It stops Core for the cold snapshot only, then exports, imports and
     verifies on copies while the old stack runs again.
   - It swaps nothing.

### 7.2 Go/no-go checklist for the owner

- [ ] PR0 findings: no open [unverified] item affects data or authentication.
- [ ] PR1, the bug fix, PR2, PR3 and PR4 merged and green, including the
      3.3 database suite as `EDITOR`.
- [ ] CI drill fixture and VM drill passed: equal counts and checksums,
      client login works, both rollbacks worked. Measured downtime is
      acceptable.
- [ ] Home Node rehearsal passed within the measured time and space.
- [ ] Fresh `jarvis-backup` archive from today is copied **off host**, and
      `jarvis-backup verify` passes there.
- [ ] `docker save` of the 2.6.5 image is stored with the off-host
      backups.
- [ ] Free disk ≥ 4× the database size + 2 GiB; `/run` has room for the
      export.
- [ ] Maintenance window agreed. No coding run or long LLM task is active,
      the updater timer is quiet, and Rivetlink is unaffected (separate
      containers and ports, not touched).
- [ ] Rollback command and runbook printed or at hand.
- [ ] After commit: new `jarvis-backup` taken and copied off host; the
      `surrealdb.v2-*` directory kept for 30 days (decision 5 in §8).

## 8. Decisions (owner delegated them on 2026-10-05)

1. **Integration branch, no release freeze.** `main` keeps shipping on
   2.6.5. PR2–PR4 land on `feat/surrealdb-3` and reach `main` together as
   one release.
2. **Drill location: CI plus the Home Node rehearsal.** No off-host VM is
   available, so the VM step in §7.1 is optional. The CI fixture covers
   every failure and rollback path. `surrealdb-engine-migrate --rehearse`
   covers the real data: it runs in disposable containers on copies, swaps
   nothing, and also exercises both rollbacks on those copies. The live
   database is never the drill target.
3. **Port 8000 exposure: assume other local workloads can reach it.** Fail
   closed: treat GHSA-wjjj-24cx-f28g as a reason to do this upgrade soon,
   not as a reason to skip steps.
4. **There is no off-host backup today.** This is a hard gate. Before the
   swap, `jarvis-backup` must be configured, a fresh archive must be copied
   off host (for example to the owner's workstation) and `jarvis-backup
   verify` must pass there. The database size is measured at go time with
   `du -sh` and `df -h` only.
5. **Old data is kept for 30 days** after commit: `surrealdb.v2-<txn>`,
   the T0 snapshot and the old env file. Deleting them stays a manual owner
   step.
6. **Usage fields are persisted.** PR2 adds migration 0011 that defines
   `requested_route`, `actual_provider` and `cost_estimate_classification`
   as optional strings, so 3.x SCHEMAFULL accepts the writes and the usage
   view keeps them.
7. **Keep a `docker save` of the 2.6.5 image** next to the off-host backup.
8. **Production image digest** is checked at go time with
   `docker image inspect` (no secrets involved) instead of asking the owner.
9. **Skip SurrealDB 2.7.0.** It removes no step and lacks the 3.3.0-only
   advisory fixes.

## 9. PR0 spike findings (2026-10-06)

CI-only spike (draft PR #93, never merged) against
`surrealdb/surrealdb@sha256:681c6c22c287421b5c7d99e0fde79b6e0d32c36c1ddeaab2762a1661cb04cd20`
(3.3.0) and the production 2.6.5 digest `sha256:d653f6c8…`, which the spike
confirmed is v2.6.5. The databases were in-memory; RocksDB on-disk
behaviour, sizes and memory use stay for the drill.

| Item | Result on 3.3 | Action |
|---|---|---|
| B3 | Confirmed: unmodified `0010` fails with "FLEXIBLE must be specified after TYPE". With `TYPE option<object> FLEXIBLE` the whole chain applies. | As planned (PR2). |
| B4 | Confirmed: a `SELECT` on a missing table errors on 3.3 and returns `[]` on 2.6.5. | Handled by #89 (`INFO FOR DB` check); the PR2 test suite on 3.3 covers the rest. |
| B7 | `NULL` into `option<string>` is rejected by **both** 2.6.5 and 3.3; `NONE` works on both. The risk is only the SDK mapping of JSON `null`. | As planned: typed bindings in PR2. |
| B11 | `UPSERT voice_profiles SET …` without id behaves the same on both versions. | None. |
| B12 | `DELETE … RETURN $id AS id` inside a transaction works on 3.3. | None. |
| B15 | **New breaking hit:** 3.3 no longer creates a namespace or database on first use ("The database 'core' does not exist"). Nothing in the repo runs `DEFINE NAMESPACE` or `DEFINE DATABASE`; production and the tests rely on implicit creation. `DEFINE` inside `BEGIN … COMMIT` works. | Fixed in #94 for `provision-core-user.sh` and the `jarvis-backup` restore test (both also valid on 2.6.5). PR2: the Rust test fixtures do the same once CI runs 3.3; the migration helper defines both before `import`. |
| B16 | **New difference:** `math::percentile` with `GROUP ALL` on no matching rows returns one row with `p50: []` on 3.3 instead of no row. `latency_p50_ms: Option<f64>` would fail to deserialize, so the monthly statistics break in a month without measured latency. `time::max` with `GROUP BY` returns no rows on both. | Fixed in #94: the row type reads an empty percentile array as "not measured", with a unit test. PR2 must keep this covered by the database test on 3.3. |
| Re-apply | Re-running `0010` on a migrated database errors ("The field 'id' already exists"). The runner checks `schema_version` first, so this only matters for manual runs. | None. |
| T3 | `INFO FOR DB` returns an object whose `tables` is a map (plus `accesses`, `analyzers`, `apis`, `buckets`, `configs`, `functions`, `models`, `modules`, `params`, `sequences`, `users`). `surreal sql --json` prints `[[{"version":10}]]`. `export --auth-level root` works. | PR3: run `test-jarvis-backup-surreal.sh` on 3.3 for the exact parsing. |
| T7 | A database `EDITOR` can apply the whole chain and run `INFO FOR DB`. | None. |
| T8 | Root is created from `SURREAL_USER`/`SURREAL_PASS`; `--unauthenticated memory`, `isready` and `/health` (HTTP 200) work. | None. |
| Export | The 2.6.5 image's `export --v3` works against an unauthenticated source; the 3.3 `surreal v2` subcommand wants to download a 2.7.0 binary and fails non-interactively. | §5.2 now uses the 2.6.5 image. |
| Import | `surreal import` into 3.3 succeeds after the database is defined. Bytes, datetime, duration, nested `NONE`, `NULL`, a record link, float and non-ASCII text round-trip exactly. | None. |
| Passhash | The `EDITOR` user's passhash survives; sign-in works with the right password and is refused with a wrong one. | None. |

## Appendix: sources

- Migration guide 2.x → 3.x: https://surrealdb.com/docs/build/migrating/from-old-surrealdb-versions/2x-to-3x
- 3.2 → 3.3 notes: https://surrealdb.com/docs/build/migrating/from-old-surrealdb-versions/32-to-33 and https://surrealdb.com/releases/3.3
- Upgrades: https://surrealdb.com/docs/manage/self-hosted/upgrades-and-patching
- CLI: https://surrealdb.com/docs/surrealdb/cli/export and https://surrealdb.com/docs/surrealdb/cli/start
- SurrealQL: https://surrealdb.com/docs/surrealql/statements/define/field and https://surrealdb.com/docs/surrealql/functions/database/type
- Rust SDK: https://surrealdb.com/docs/sdk/rust and https://surrealdb.com/docs/reference/rust/concepts/surrealvalue-attributes
- Crate source (read, not built): crates.io `surrealdb`, `surrealdb-types`, `surrealdb-core` and `surrealdb-syn` at 3.3.0; local registry `surrealdb` and `surrealdb-core` at 2.6.5.
