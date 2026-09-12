# Public client download retention

Native Rust policy for a future dedicated public installation archive. It does
not change the Python authenticated-update mirror, Core activation/rollback,
GitHub release retention, or installed application state.

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

Other targets are `windows-x86_64` and `macos-arm64`. iOS is not an installable
public-download target. Output lists retained versions with reasons and removal
candidates. Input is bounded to 2 MiB and 10,000 records; versions to 64 bytes.
Unknown fields/targets, duplicate target/version records and noncanonical or
prerelease versions fail closed. Input strings are not echoed into errors.

No network client, web server, background runtime, artifact loading, subprocess
execution or filesystem deletion is present. This avoids holding binaries in
memory and permits validating the rule before any destructive integration.
Rust alone is not a guarantee about a future server's memory consumption.

## Integration status and requirements

**Automatic storage/cleanup is not connected yet.** The current Caddy page links
to public GitHub Releases; it has no local artifact archive to prune. A later
trusted exporter must copy only verified installation artifacts into a separate
root-controlled public generation, generate its index from this plan, atomically
activate the new index and only then retire obsolete public files. Never delete
from the authenticated mirror or expose its filesystem as a public Caddy root.
Never apply a client-supplied inventory as filesystem authority. Group all
installer/signature files for a target/version under the same retention decision.

Tests cover the owner's sequence, per-target independence, numeric sorting,
missing baselines, minor expiry, bounded invalid input and incremental cleanup
equivalence over 180 releases. Full integration still needs transactional
publication, verified-import and symlink/path-safety tests before automatic
deletion can be enabled.
