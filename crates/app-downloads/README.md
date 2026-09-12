# Private candidate import and public download retention

Native Rust importer for an owner-digest-pinned iOS candidate, plus a separate
retention planner. It does not change the authenticated-update mirror, Core
activation/rollback, GitHub releases, or installed application state.
See [private importer setup](../../deploy/app-updates/PRIVATE_DOWNLOADS.md).

## Exact policy, independently for every downloadable target

1. Keep the latest three stable releases in semantic version order.
2. Additionally keep the actual `MAJOR.0.0` baselines of the latest three distinct
   major lines present for that target. Do not fabricate a missing baseline.
3. Within the newest major only, keep the latest patch of each of its newest
   three distinct minor lines, including the current minor.
4. Remove remaining versions from the public archive's retention set. Once
   major 4 exists after majors 1, 2 and 3, no major-1 version is retained.

All rules form one union, not three independent duplicate copies. Thus there
can be more than three retained versions per target. A minor baseline such as
`2.1.0` has no special lifetime; it is not retained merely for ending in `.0`.
An old major retains its `X.0.0`, not its final patch indefinitely. At a major
transition the previous major's final patches naturally age out of the rolling
latest-three window.

| Newest release | Retained versions in the example sequence |
| --- | --- |
| 1.0.8 | 1.0.0, 1.0.6, 1.0.7, 1.0.8 |
| 2.0.0 | 1.0.0, 1.0.7, 1.0.8, 2.0.0 |
| 2.0.5 | 1.0.0, 2.0.0, 2.0.3, 2.0.4, 2.0.5 |
| 2.1.5 | 1.0.0, 2.0.0, 2.0.5, 2.1.3, 2.1.4, 2.1.5 |
| 4.0.0 after 3.0.5 | 2.0.0, 3.0.0, 3.0.4, 3.0.5, 4.0.0 |

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
updates a public index atomically. No subprocess execution or deletion of
historical versions is exposed by either command.

## Integration status and requirements

**Automatic cleanup is not connected yet.** The initial IPA importer preserves
all previously approved versions. Its index and Caddy route serve only the
separate root-controlled public archive. A later retention integration must
atomically activate an index before retiring obsolete public files. Never delete
from the authenticated mirror or expose its filesystem as a public Caddy root.
Never apply a client-supplied inventory as filesystem authority. Group all
installer/signature files for a target/version under the same retention decision.

Tests cover the owner's sequence, per-target independence, numeric sorting,
missing baselines, minor expiry, bounded invalid input and incremental cleanup
equivalence over 180 releases. Full integration still needs transactional
publication, verified-import and symlink/path-safety tests before automatic
deletion can be enabled.
