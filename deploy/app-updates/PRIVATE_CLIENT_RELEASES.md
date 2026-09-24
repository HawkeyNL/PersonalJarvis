# Complete private client releases

The Rust `jarvis-app-downloads sync-release` command complements `sync-ios`.
They share the local read-only GHCR token but have **separate approval configs**.
Do not replace the existing iOS config with a desktop release config.

## Pipeline and trust

PersonalJarvisApp's protected `application-release` workflow builds Linux x86_64,
Windows x86_64, macOS arm64, Android and an unsigned iPhone IPA from one reviewed commit. Cross-job files
and final releases go only to private `ghcr.io/hawkeynl/jarvis-client-artifacts`.
There are no public Actions build artifacts or GitHub Release uploads.

Platform jobs independently verify Tauri updater signatures, macOS Developer ID
signing/notarization and Android APK certificate identity. Final publication
validates the complete matrix, signs `latest.json`, pulls back the exact OCI
digest and verifies every file's size/hash. Only then are `app-vX.Y.Z` and `stable`
published inside GHCR. `stable` is discovery, **not trust**. The Home Node checks
the separately pinned Tauri public key, complete metadata, every desktop
signature and the APK signing certificate before activation. Missing platforms,
unsigned metadata, changed same-version bytes and version downgrades fail closed.

The signed manifest binds the unsigned iPhone IPA as a `manual-owner-signing`
installer, never an automatic updater target or Apple-signed payload. It appears
under `/downloads/releases/vX.Y.Z/ios-arm64/`, separate from historical candidates.
The existing `ios-candidate.yml` and `sync-ios` remain available for standalone
candidates; they cannot substitute for a complete signed release. No iOS App
Store credentials or Apple distribution uploads are introduced.

## Server paths

* `/etc/jarvis/app-downloads/ghcr.token`: shared private read:packages credential.
* `/etc/jarvis/app-downloads/config.json`: existing owner-pinned iOS candidate.
* `/etc/jarvis/app-downloads/release.json`: approved complete release plus public
  signing identities; root:root 0600.
* `/var/lib/jarvis-app-updates/releases/vX.Y.Z`: protected immutable generation.
* `/var/lib/jarvis-app-updates/current`: atomic active-generation symlink.
* `/var/lib/jarvis-public-downloads/releases/vX.Y.Z`: separate public copies.

Caddy only exposes canonical AppImage, EXE, DMG, APK and existing IPA installation
paths plus the generated index. No directory listing, metadata, secrets or
protected-mirror alias. `/v1/app-updates/**` still requires enrolled-device auth.
All clients use the runtime-enrolled Home Node origin; no DNS value in builds.
The native Rust service is not a Docker container and needs no Docker socket.
The protected mirror is deliberately outside service-user-owned `/var/lib/jarvis`:
every ancestor of release state must remain root-controlled. Existing legacy
mirror data and Core directory permissions are not modified. The new directory
is root:jarvis 0750; Core can read, not replace, release generations.

## Owner activation after review and required signed approval

These are **post-review instructions**, not a claim that production is deployed.
Do not run another mirror writer against the same protected destination. Stop
the legacy `jarvis-app-update-sync.timer` first if previously enabled.

Build without sudo from the reviewed Core checkout:

```sh
cargo build --locked --release -p jarvis-app-downloads
```

Through the existing approved owner administration path:

```sh
sudo apt install apksigner
sudo bash deploy/app-updates/install-app-downloads.sh "$PWD/target/release/jarvis-app-downloads"
sudoedit /etc/jarvis/app-downloads/release.json
sudo chown root:root /etc/jarvis/app-downloads/release.json
sudo chmod 0600 /etc/jarvis/app-downloads/release.json
```

Use `release-downloads-config.example.json`. Copy the exact source SHA and final
OCI digest from the successful **complete private release** summary, not the
iOS candidate summary. Copy the **public** Tauri key and Android certificate
fingerprint matching the client release configuration. Never enter signing
private keys here. Reuse the already installed token; no need to paste it again.

Initially leave `track_stable: false` to test exactly the reviewed release.
After acceptance, an approved change to `track_stable: true` permits future
signed stable releases at/above the configured minimum. Installed app SemVer
and Android versionCode may never move backwards. The local installed version
also blocks stale upstream metadata after the first activation.

```sh
sudo systemctl daemon-reload
sudo systemctl start jarvis-app-release-sync.service
sudo systemctl status jarvis-app-release-sync.service --no-pager
```

Configure Core's existing `JARVIS_APP_UPDATE_MIRROR_ROOT` to
`/var/lib/jarvis-app-updates` and `JARVIS_APP_UPDATE_PUBLIC_BASE_URL` to the owner's
runtime HTTPS origin through the trusted configuration flow. Do not overwrite
other Core configuration. Restart Core only through approved administration.
If those settings already match, no client DNS change is needed.

Merge the reviewed `@clientDownload` Caddy block alongside the existing index and
iOS blocks; preserve the site's TLS/environment and Core proxy. Validate and
reload through the trusted administration flow. Do not replace working site
configuration blindly. No new port; existing HTTPS/443 only.

After a successful manual import and actual client test:

```sh
sudo systemctl enable --now jarvis-app-release-sync.timer
```

`enable` makes this persistent across host reboots. The timer runs about five
minutes after boot and every thirty minutes thereafter (with up to five minutes
of jitter); there is no constantly running download process. A failed network
check leaves the previous verified generation available. To follow future
signed releases rather than repeatedly checking one digest, explicitly approve
`track_stable: true` after the pinned first import has passed acceptance.

Verify activation and delivery separately:

```sh
systemctl is-enabled jarvis-app-release-sync.timer
systemctl list-timers jarvis-app-release-sync.timer --all
# Substitute the owner's configured HTTPS origin, without a trailing slash.
curl --fail --head "https://<configured-home-node>/downloads/"
```

Both `/downloads` and `/downloads/` must return `200` and `text/html`, without
`Content-Disposition: attachment`. A zero-byte `404` is not a downloadable
installer: inspect the installed Caddy routing and generated public index.
Installers alone use attachment responses. The Rust importer renders a mobile
HTML page from validated local inventory; no frontend service or embedded Home
Node hostname is needed. An iOS IPA still requires local owner signing.

Desktop and Android updaters do not follow these public links or a custom deep
link: native clients derive `/v1/app-updates/**` from the enrolled runtime HTTPS
origin, authenticate, and reject cross-origin downloads. Keep those routes
behind Core authentication. Do not fix a missing public page by exposing the
protected mirror, metadata, token, or arbitrary directory listings.

## Acceptance

1. Public `/downloads/` lists all successfully imported installers.
2. Download each installer and compare SHA-256 to the signed release manifest.
3. Unauthenticated `/v1/app-updates/capability` still returns 401.
4. `/downloads/releases/vX.Y.Z/manifest.json` and staging/secret paths are not public.
5. An enrolled desktop checks through native auth and verifies Tauri signatures.
6. Android validates APK package identity, versionCode and certificate before the
   OS package installer. AAB is not a public installation link.
7. iOS IPA still requires local signing; no automatic update claim.
8. Bad signature/hash, interrupted transfer and unavailable GHCR leave the prior
   active generation intact. Same-version substitution is refused.

The version-retention planner remains separate from destructive cleanup in this
initial full-release importer; no existing history is deleted automatically.
Real platform signing, device installation and production Caddy tests must be
reported separately from rootless cryptographic/OCI fixture tests.
