# Signed desktop releases and authenticated Home Node delivery

The historical filename is retained for existing links. Desktop releases now
have a public upstream, not a separate private release repository.

## Ownership and integrity

- `HawkeyNL/PersonalJarvisApp`: canonical Vue/Tauri desktop source, independent
  `app-vX.Y.Z` versions, desktop CI, signing and GitHub Releases.
- `HawkeyNL/PersonalJarvis`: Core/Home Node, Core Admin, CLI, authoritative
  `crates/client-core`, authenticated update API, mirror, Android and iOS.

Core versions remain independently `vX.Y.Z`. The desktop pins client-core to
an exact reviewed Core commit. Protocol compatibility—not equal product
versions—is the integration boundary. No third repository or sibling production
checkout is required. Public source visibility does not grant an open-source license.

Server sync is in `tools/app-updates/update-mirror/`; server manifest inspection
and legacy/mobile tooling remain in `tools/app-updates/update-release/`.
Desktop artifact generation/signing belongs to PersonalJarvisApp.

GitHub is untrusted artifact transport. The Home Node pins a Tauri public key,
verifies signed `latest.json`, release identity, sizes, hashes and updater
signatures before promotion. Clients independently verify updater signatures.
No signing private key belongs on the Home Node. Public downloads provide no
confidentiality but do not let a downloader forge future signed updates.

The public GitHub source needs no token. Optional `source.bearer_token_file`
supports protected authentication; tokens are headers, never URL parameters.
Only HTTPS redirects are allowed; Authorization is stripped permanently after
an origin change. Invalid metadata, rate limiting, missing assets, invalid
signatures and downgrades leave the current generation active.

GitHub Actions has no inbound Home Node connection or infrastructure identifiers.
The Home Node pulls outbound. Existing Caddy TCP 443 carries authenticated
`/v1/app-updates/**`. Do not open any extra port or expose Core/SurrealDB.

## Manifest and generations

`tools/app-updates/update-release/schema/manifest-v1.schema.json` describes the
shape. Runtime `manifest.py` additionally validates canonical paths, identity
and complete target matrices. Desktop manifests have `product=desktop`,
`tag=app-v<version>`, a 40-character source revision, client protocol and minimum
client protocol, and three updater targets: Linux x86_64, Windows x86_64,
macOS arm64. Legacy five-target manifests remain readable.

```text
/var/lib/jarvis/app-updates/
├── current -> releases/v0.1.1
├── manifests/latest.json -> ../current/manifest.json
└── releases/
    ├── v0.1.0/
    └── v0.1.1/
```

Staging and validation precede atomic activation. The current and one previously
active verified generation are retained. Files are root-controlled and readable,
not writable, by Core's `jarvis` group. Same-version sync verifies immutable
existing bytes before reuse.

## Owner setup after the first real desktop release

These are post-review owner actions, not migration steps to execute now.
First complete PersonalJarvisApp's `docs/GITHUB_RELEASE_SETUP.md`.
Use a reviewed Core checkout for the one-time installer; routine sync uses
installed server tooling, not the checkout.

```bash
sudo apt install python3 minisign
sudo bash deploy/app-updates/install-app-update-sync.sh
sudo install -o root -g root -m 0640 \
  deploy/app-updates/config.example.json /etc/jarvis/app-updates/config.json
sudoedit /etc/jarvis/app-updates/config.json
```

Do not overwrite an existing customized config: compare the example and edit
the existing file instead. Its source is:

```json
{
  "kind": "github-releases",
  "repository": "HawkeyNL/PersonalJarvisApp"
}
```

Replace `tauri_signing_public_key` with the reviewed public key also embedded
in the desktop release. Never put the private key here. No GitHub token or
Android configuration is needed for this desktop source.

Run one manual sync before enabling its timer:

```bash
sudo systemctl start jarvis-app-update-sync.service
sudo systemctl status --no-pager jarvis-app-update-sync.service
sudo readlink -f /var/lib/jarvis/app-updates/current
sudo jq '.release' /var/lib/jarvis/app-updates/current/manifest.json
sudo find /var/lib/jarvis/app-updates/current/ -maxdepth 3 -type f
sudo -u jarvis test -r /var/lib/jarvis/app-updates/current/manifest.json
sudo systemctl enable --now jarvis-app-update-sync.timer
```

Using `sudoedit /etc/jarvis/core.env`, configure both:

```text
JARVIS_APP_UPDATE_MIRROR_ROOT=/var/lib/jarvis/app-updates
JARVIS_APP_UPDATE_PUBLIC_BASE_URL=https://your-enrolled-home-node-origin
```

Replace the placeholder locally with the existing credential-free HTTPS origin,
without path/query/fragment. Do not commit the deployment address. Core rejects
incomplete mirror configuration and unsafe public origins.

```bash
sudo systemctl restart jarvis-core.service
read -r -p 'Existing Home Node HTTPS origin: ' jarvis_origin
curl --fail --proto '=https' "${jarvis_origin%/}/readyz"
unset jarvis_origin
```

Test capability through an authenticated enrolled native client.
Unauthenticated `/v1/app-updates/capability` must not succeed. Never paste tokens
into curl arguments, logs or Vue state. Install/pair the first signed desktop
version, then mirror a subsequent compatible version for an actual update test.
Verify incompatible releases, download verification, manual install/restart and
failure recovery. A first release alone cannot prove end-to-end upgrading.

Production clients receive the origin at runtime and accept HTTPS only.
Origin changes clear old server-specific credentials/device binding. Native Rust
performs authentication and update downloads; Vue receives safe status/progress.
One delayed post-authentication check may run without blocking startup.
Offline/update failure leaves the installed client usable.

## Mobile compatibility

Android/iOS source and CI remain in Core. `mobile-release.yml` retains the
existing `private-app-release` signing environment and canonical secret names,
Android signed APK/AAB builds and signer verification, and iOS App Store
Connect/TestFlight export. Desktop signing is separate in PersonalJarvisApp's
`application-release`. Mobile versions need not match desktop versions.

Existing authenticated Android endpoints and legacy manifest parsing remain.
APK mirroring requires explicit `android_signing_certificate_sha256` and
`android_apksigner_path`; missing settings cause refusal, not skipped verification.
iOS uses TestFlight, never a custom IPA updater. Desktop-only manifests do not
offer Android updates. Mobile distribution integration remains a migration
acceptance item: do not replace an existing mixed-platform mirror with a
desktop-only source until its mobile delivery path has been reviewed.

## Developer verification

Run as an unprivileged developer:

```bash
python3 -B -m unittest discover -s tools/app-updates/update-release/tests -v
python3 -B -m unittest discover -s tools/app-updates/update-mirror/tests -v
bash deploy/app-updates/tests/test-app-update-assets.sh
cargo test -p jarvis-api -p jarvis-client-core --locked
```

Install `minisign` to run cryptographic fixtures, which create disposable test
keys only. CI needs no production signing secrets. Real macOS notarization,
Windows packaging, published assets and Home Node/device acceptance are separate
checks; mock tests cannot truthfully prove those outcomes.
