# Private candidate import and public download retention

Native Rust importer for an owner-digest-pinned iOS candidate, plus a separate
retention planner. It does not change the authenticated-update mirror, Core
activation/rollback, GitHub releases, or installed application state.
See [private importer setup](../../deploy/app-updates/PRIVATE_DOWNLOADS.md).

## Exact policy, independently for every downloadable target

1. Keep the latest three patches of the current minor line.
2. Keep the latest patch of the two previous minor lines in the current major.
3. Keep the latest release of the two previous major lines.
4. Remove every other version from the public archive.

Missing lines are not invented: with only one major, only rules 1 and 2 apply.

| Newest release | Retained versions in the example sequence |
| --- | --- |
| 5.0.0 after 1.0.2 … 4.0.2 | 3.0.2, 4.0.2, 5.0.0 |
| 5.1.5 after 5.0.4 | 3.0.2, 4.0.2, 5.0.4, 5.1.3, 5.1.4, 5.1.5 |
| 5.6.6 after 5.2 … 5.5 | 3.0.2, 4.0.2, 5.4.6, 5.5.6, 5.6.4, 5.6.5, 5.6.6 |
| 0.1.20 | 0.1.18, 0.1.19, 0.1.20 |

## Read-only planner

```sh
cargo run -p jarvis-app-downloads -- plan < verified-public-inventory.json
```

Inventory shape (base versions, not Git tags):

```json
[
  {"target": "linux-x86_64", "version": "1.0.0"},
  {"target": "linux-x86_64", "version": "1.0.8"},
  {"target": "android-universal", "version": "1.0.8"}
]
```

Other targets are `windows-x86_64`, `macos-arm64`, and `ios-arm64` (manual-signing
candidates only). Output lists retained versions with reasons and removal
candidates. Input is bounded to 2 MiB and 10,000 records; versions to 64 bytes.
Unknown fields/targets, duplicate target/version records and noncanonical or
prerelease versions fail closed. Input strings are not echoed into errors.

The `plan` command performs no network/file mutations. The separate `sync-ios`
command requires trusted root-controlled configuration at fixed paths. It streams
GHCR files to disk, verifies the pinned digest and exact layer checksums and
updates a public index atomically. Neither command executes a subprocess or
deletes historical versions.

## Automatic retirement

`sync-release` applies the policy to the public `releases/` directory after it
activates the authenticated update. Every entry must be a canonical `vX.Y.Z`
directory owned by the expected user, or nothing is retired. Retired releases
are first renamed out of `releases/`, then the index is rebuilt, and only then
are their files deleted, so the fresh index never links a removed installer.
If a step fails, the moved releases are put back. A browser may still show a
cached page for up to five minutes. A release
directory holds every target of a version and is retired as one unit. The
authenticated update mirror and the separate `ios/` candidates are never
pruned by this step.
