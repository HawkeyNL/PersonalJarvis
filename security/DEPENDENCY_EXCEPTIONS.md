# Dependency security exceptions

Exceptions in this document are deliberately narrow, time-bounded, and paired
with an enforced CI expiry. They do not authorise a vulnerable API or feature at
runtime. Remove an exception as soon as the upstream dependency graph no longer
contains the affected package.

## RUSTSEC-2026-0235 — `rkyv` 0.7.46

- **Scope:** the optional `rkyv` feature declared by `rust_decimal` 1.42.1.
  Jarvis uses `rust_decimal` only with `serde-str`; it does not enable or import
  `rust_decimal`'s `rkyv` feature. `cargo tree --all-features` confirms that
  `rkyv` is not in Jarvis's resolved build graph.
- **Risk decision:** the affected archive-validation code is not compiled into
  Jarvis Core, and Core accepts no `rkyv` archives. The package remains in
  `Cargo.lock` because Cargo records optional dependencies declared upstream,
  which `cargo audit` intentionally scans.
- **Compensating control:** all other RustSec advisories remain blocking. The
  exception applies only to this advisory ID; CI fails automatically on or after
  2026-10-01.
- **Owner and exit condition:** upgrade or replace the upstream dependency as
  soon as `rust_decimal` no longer declares vulnerable `rkyv` 0.7, then remove
  both this entry and `.cargo/audit.toml`.
