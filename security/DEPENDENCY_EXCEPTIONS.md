# Dependency exceptions

`cargo audit` ignores the advisories below through `.cargo/audit.toml`. Each
exception has an expiry date and guards that CI (`.github/workflows/ci.yml`)
and the release workflow (`.github/workflows/release.yml`) enforce. When a
guard fails or the date passes, re-review the advisory instead of extending it.

| Advisory | Crate | Expires | Guard |
|---|---|---|---|
| RUSTSEC-2026-0235 | rkyv 0.7 | 2026-12-31 | date |
| RUSTSEC-2026-0381 | webpki-roots 0.26.11 | 2026-12-31 | date, allowed versions |
| RUSTSEC-2026-0453 | surrealdb 2.x (affinitypool) | 2026-12-07 | date, `affinitypool` absent, surrealdb lines unchanged |
| RUSTSEC-2026-0454 | surrealdb-core (reblessive 0.4.3) | 2026-12-07 | date, surrealdb lines unchanged |

## RUSTSEC-2026-0235 (rkyv 0.7)

Locked through an optional dependency of `rust_decimal`, but compiled for no
target or feature set.

## RUSTSEC-2026-0381 (webpki-roots 0.26.11)

`surrealdb` pulls 0.26.11 through `tokio-tungstenite`. That version is a shim
that re-exports webpki-roots 1.x. The guard allows only that shim and patched
1.x releases, and fails when webpki-roots leaves the lockfile.

## RUSTSEC-2026-0453 (surrealdb, affinitypool)

The use-after-free sits in the embedded key-value engines that use
`affinitypool`. Core builds `surrealdb` with `default-features = false` and
only `protocol-ws` and `rustls`, so `affinitypool` is not in `Cargo.lock` and
the affected code is not compiled. Patched in surrealdb 3.3.0. The guard
fails if `affinitypool` enters the lockfile, the root `surrealdb` line changes
or a workspace member declares `surrealdb` other than `workspace = true`.

## RUSTSEC-2026-0454 (surrealdb-core, reblessive 0.4.3)

`reblessive` is compiled in and drives the query parser inside
`surrealdb-core`. Core never uses reblessive types itself, and connects only
through the remote WebSocket engine, never a local `Datastore`. Triggering the
unsound `Send`/`Sync` needs code that copies driver futures or moves non-`Send`
state across threads: local API misuse, not crafted input. No patched 2.x or
3.x release exists; the fix is surrealdb-core 4.0.0. The guards are the same
as for RUSTSEC-2026-0453.

These exceptions cover the Core client only. The SurrealDB 2.6.5 server is
affected by both advisories until it runs 4.0.0 or later.
