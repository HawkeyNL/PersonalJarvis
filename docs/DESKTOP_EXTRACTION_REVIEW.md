# Desktop extraction — local implementation review

## Scope and local commits

No branch was pushed, no tag/release/PR was created or merged, and no production
configuration/service was changed. The source snapshot was taken from Core main
`89372c9c5b157361881b79b583c309c91c6f5646`. Core's public history was not rewritten.
PersonalJarvisApp existed as an empty Git repository; extraction used a documented
snapshot rather than importing/replacing unrelated existing source.

| Repository | Review branch | Implementation tip |
| --- | --- | --- |
| PersonalJarvis | `refactor/extract-desktop-app` | `1093039cef1dc0060d13addf5e3dcfb93141d9eb` |
| PersonalJarvisApp | `feat/initial-desktop-app` | `0944a8cbdd99d1cff66041b26f8472bde3a33b34` |

This report is committed separately after the implementation tip above. Earlier
review commits are Core `e36a884`, `49fec9e`, and App `37d850e`, `6fc5f26`, `7559c33`.
All remain local and reviewable; no force-push or public history rewrite occurred.

## Source ownership and migration

The former `PersonalJarvis/jarvis-app/` tracked desktop source was extracted to
the App repository root: `src/`, `src-tauri/`, `public/`, `tests/`, frontend
manifests/lockfile, Vite/TypeScript configuration and desktop scripts. No
`PersonalJarvisApp/jarvis-app/` nesting or second editable Vue/Tauri client remains.
No node_modules, Rust targets, credentials, signing keys or local environment
files were copied. Historical tracked Tauri Apple scaffolding remains labelled
legacy in the App README; it is not the native iOS product.

Server-owned `update-mirror/` moved to `tools/app-updates/update-mirror/` in Core.
Core retains manifest inspection and legacy/mobile tooling under
`tools/app-updates/update-release/`; the active desktop release generator is
App's `update-release/desktop_release.py`. Android and iOS source remain in Core.
Core Admin remains in Core. Its three PNG icons now live in its own
`src-tauri/icons/`, and the Core release builder no longer reads desktop assets.
Ignored local build/schema caches may still exist below the old directory;
they are not tracked source and were deliberately not destroyed.

The authoritative `jarvis-client-core` remains only at Core's
`crates/client-core`. The desktop Cargo dependency and lockfile pin exactly:

```text
89372c9c5b157361881b79b583c309c91c6f5646
```

There is no sibling path, floating branch or copied protocol crate. Tests reject
local dependency overrides, including target-specific path dependencies, Cargo
source overrides, workspace/patch redirection and npm/Tauri/Rust version drift.
To update the pin: review/merge the shared change in Core, select its exact SHA,
update the dependency and lockfile in App, then run App/protocol CI separately.

```text
PersonalJarvisApp/
  .github/workflows/{ci,release}.yml
  docs/GITHUB_RELEASE_SETUP.md
  src/                       Vue desktop presentation
  src-tauri/                 native credentials, protocol and updater
  update-release/            signing/manifest/artifact validation
  scripts/                   privacy checks and development helpers
  tests/                     frontend tests
  public/                    desktop assets
  package{,-lock}.json        independent app version 0.1.0
  tsconfig*.json, vite.config.ts
```

## Desktop release and update architecture

App `ci.yml` checks frontend, Linux/macOS/Windows native builds and release
tooling without production signing secrets. Manual `release.yml` is main-only,
uses immutable action references and Rust 1.97.1, and checks requested SemVer
against npm, lockfile, Rust and Tauri versions. Tags are `app-vX.Y.Z`, independent
of Core tags. It builds each platform at one source SHA, signs updater artifacts,
validates the exact artifact matrix, signs `latest.json`, stages a draft in its
own public repository, redownloads/revalidates bytes, then publishes only after
all mandatory jobs succeed. No workflow contacts a Home Node.

Linux uses AppImage; Windows uses NSIS with Tauri signatures (no Authenticode
claim); macOS requires Developer ID, hardened runtime, notarization/stapling and
Tauri signatures. Supplemental DMG size/hash is also bound into signed metadata.
The mirror downloads/verifies that installer but does not expose it as a Tauri
updater target. Its Apple OS trust is checked by the macOS release job, not by
the Linux mirror.

The public GitHub source is `HawkeyNL/PersonalJarvisApp`, with optional—not
mandatory—bearer authentication. HTTPS, strict repository/asset validation,
bounded metadata, redirect token stripping, manifest signature verification,
artifact hashes/sizes and updater signatures precede atomic activation.
Bad upstream data and rate limiting preserve current. Current plus one previous
verified generation are retained, and same-version reuse revalidates exact bytes.

Core owns authenticated `/v1/app-updates/**`. Application origin remains runtime
configuration; credentials/private keys remain native OS-secured data and never
enter Vue. Switching origins clears the prior server binding/session. Protocol
and capability metadata control compatibility, not equal Core/App SemVer.

## Mobile preservation

Native mobile CI remains in Core. The mobile release retains its existing
`private-app-release` environment and signing-secret names. Android waits for
the iOS/TestFlight job, verifies its signer, and produces an **owner-encrypted**
Actions handoff. No plaintext production APK/AAB is uploaded and no third release
repository is needed. Only a public age recipient is configured in GitHub.
Age is transport confidentiality, not release signing; existing signing keys
are unchanged. The owner decrypts/imports locally, without giving Core or CI a
decryption key. No production key was generated during implementation.

Import accepts four bounded regular USTAR members without general extraction,
verifies the root-pinned APK signer and manifest/hash, and reuses the mirror's
transactional activation/retention. It rejects symlinks, duplicate/traversal/PAX
members and non-advancing Android versionCode. Failed verification keeps current.

`JARVIS_MOBILE_APP_UPDATE_MIRROR_ROOT` separates mobile generations from the
desktop root; old combined mirrors remain supported when it is absent. Sync
refuses to discard Android through a desktop-only replacement. Existing private
HTTP-template source support remains. iOS continues through TestFlight.

## Owner handoff

The exact GitHub environment, variable/secret names, Actions permissions, branch
protection recommendations, Tauri key procedure and first `app-v0.1.0` checklist
are in **PersonalJarvisApp/docs/GITHUB_RELEASE_SETUP.md**.

The exact Home Node installation/configuration/manual-sync/timer/restart commands,
authenticated capability acceptance, private mobile handoff and safe migration
of an existing combined mirror are in
[application-update deployment](app-updates/PRIVATE_RELEASES.md).

Core must run a release containing this parser/configuration support before
enabling the new mirror; a new script alone does not upgrade a running Core.
No Home Node address is compiled into clients, manifests or workflows. Setup
instructions request the existing HTTPS origin locally and need no new ingress
port beyond the existing Caddy TCP 443 path.

## Verification evidence

### Privacy correction

The earlier uncommitted desktop test reference to the owner's public IP was
removed before the snapshot commit. The denylist regression uses SHA-256
fingerprints, not the owner's literal identifiers; its own tests use synthetic
identifiers. No production hostname, domain, public IP or LAN IP is intentionally
retained, including in deployment documentation (the origin is entered locally).
The final scans found zero occurrences across Core's 350 source paths and 2,473
history objects and App's 156 source paths and 216 history objects. These counts
precede this report's documentation commit. The final clean-clone release binary
also passed the privacy scan. No historical commit required rewriting.

Tests were run unprivileged. `minisign` and `age` test executables were downloaded
and unpacked under temporary directories, not installed on production. Disposable
test keys were created only inside automatically cleaned test directories.

| Check/command | Observed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --all` | Passed; two existing disposable-SurrealDB tests remain ignored |
| `cargo audit` | Exit 0; existing atomic-polyfill/bincode unmaintained warnings and chacha20 yanked warning |
| `cargo check --workspace --locked` after desktop removal | Passed |
| `cargo test -p jarvis-api --test surreal_api every_application_update --locked` | Passed: all five update routes reject unauthenticated HTTP requests before DB/filesystem access |
| `python3 -B -m unittest discover -s tools/app-updates/update-release/tests` | 14 passed |
| `python3 -B -m unittest discover -s tools/app-updates/update-mirror/tests` with test age/minisign executables | 31 passed, including real ephemeral-key crypto, corrupt upstream/installer, rollback retention and mobile import |
| `bash deploy/app-updates/tests/test-app-update-assets.sh` | Passed |
| `bash scripts/release/tests/test-build-linux-package.sh` | Passed |
| `bash deploy/private/tests/test-public-release-boundary.sh` | Passed |
| actionlint 1.7.7 on Core mobile/app CI and App CI/release workflows | Passed; embedded shell/Python linters disabled |
| App `python3 -B -m unittest discover -s update-release/tests` with test minisign | 28 passed |
| Privacy tests in each repository | 4 passed each |
| Both source/history privacy scans, including refs/reflogs | Zero forbidden deployment identifiers |
| Privacy scan of the locally compiled desktop release executable | Zero forbidden deployment identifiers |
| `git diff --check` | Passed |

A real local clone under a new `/tmp` parent, with no sibling Core checkout,
successfully ran these commands against the immutable client-core Git pin:

```bash
npm ci
npm run check
npm run test:unit
cargo fetch --manifest-path src-tauri/Cargo.toml --locked
cargo check --manifest-path src-tauri/Cargo.toml --locked --offline
cargo test --manifest-path src-tauri/Cargo.toml --locked --offline
cargo clippy --manifest-path src-tauri/Cargo.toml --locked --offline --all-targets -- -D warnings
npm run desktop:build -- --no-bundle --ci -- --locked
```

The initial clone was at `37d850e`; it was refreshed to the final App implementation
tip before the final frontend/native/release compile. Native tests: 15 passed;
frontend: six tests in two Node test files passed. Network-restricted dependency downloads
initially failed and were retried with approved download access. No checkout
path dependency or production Home Node connection was used.

## Explicitly not proven or performed here

- No GitHub CI/release job was dispatched, because remote workflow execution and
  signing configuration remain owner actions after review. Local checks are not
  represented as green remote check runs.
- No real Windows/macOS runner, Developer ID/notary credentials, production
  Tauri/APK signing keys, real encrypted mobile release, published app release,
  desktop GUI acceptance or end-to-end installed-client update was available.
- Two existing integration tests requiring a disposable authenticated SurrealDB
  server were not run. The added no-auth HTTP test runs without that server.
- `scripts/ci/verify-deployment-security.sh` contains destructive absolute-path
  root fixtures and was intentionally not run on this production Home Node.
  Rootless namespaces were unavailable, Docker access was denied and noninteractive
  sudo required authentication. Those fixtures must run on disposable CI, not
  against production `/opt/jarvis`.
- `systemd-analyze verify` could parse the units outside the syscall sandbox but
  reported that the new installed ExecStart path does not exist here. Nothing was
  installed to make that check green. Installed-unit/startup validation remains
  a deployment acceptance check.

These limitations do not grant permission to publish or deploy. Review/merge,
configure the documented keys/settings, run platform CI, and complete the owner
acceptance procedure before the first production application release.
