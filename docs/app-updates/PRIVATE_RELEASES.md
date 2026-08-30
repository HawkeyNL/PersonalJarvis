# Private Jarvis application releases

Jarvis application binaries are built from the public source repository but
are never published on its public GitHub Releases page. A trusted, explicitly
dispatched workflow stages a complete release in private storage. The Home Node
then pulls desktop and Android artifacts outbound; GitHub never connects to the
Home Node.

## Platform distribution

| Target | Build | Distribution | Update authority |
| --- | --- | --- | --- |
| Windows x86_64 | Tauri NSIS plus updater bundle | Home Node mirror | Tauri updater signature; Authenticode not yet configured |
| macOS arm64 | Developer ID signed/notarized Tauri app/DMG plus updater bundle | Home Node mirror | Apple OS trust plus Tauri updater signature |
| Ubuntu x86_64 | Tauri AppImage plus updater bundle | Home Node mirror | Tauri updater signature |
| Android | Native signed APK and optional AAB | Home Node APK mirror | pinned Android signing certificate |
| iOS arm64 | Native signed Xcode archive | App Store Connect/TestFlight | Apple code signing |

iOS deliberately has no custom self-updater. TestFlight/App Store Connect is
the installation and update mechanism. The Home Node manifest records that
distribution outcome but does not mirror or install an IPA.

## Trust and secret boundaries

The Home Node is a mirror, not a signing authority. Tauri embeds only the
public updater key in trusted release builds and verifies the updater bundle
before installation. A compromised mirror therefore cannot replace a desktop
package with an unsigned package. Android requires the same long-lived signing
key on every release; the Home Node additionally pins its certificate SHA-256.

macOS has two independent trust layers. Developer ID Application signing,
hardened runtime, Apple notarization and stapling establish operating-system
trust. The Tauri updater signature separately establishes that Jarvis approved
the update payload. Notarization failure aborts the platform job before any
macOS artifact is uploaded to the private draft. Windows artifacts currently
have mandatory Tauri updater signatures but are not claimed to have
Authenticode signatures; a future certificate provider can be added to the
isolated Windows matrix leg without changing the manifest or Home Node flow.

The protected GitHub environment `private-app-release` must require owner
approval and contain only these release secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
- `PRIVATE_RELEASE_REPO_TOKEN`, scoped to release contents in the private repo
- `MACOS_DEVELOPER_ID_CERTIFICATE_P12_BASE64`
- `MACOS_DEVELOPER_ID_CERTIFICATE_PASSWORD`
- `MACOS_DEVELOPER_ID_APPLICATION`
- `MACOS_NOTARY_API_ISSUER_ID`
- `MACOS_NOTARY_API_KEY_ID`
- `MACOS_NOTARY_API_PRIVATE_KEY_BASE64`
- `ANDROID_RELEASE_KEYSTORE_BASE64`
- `ANDROID_RELEASE_KEY_ALIAS`
- `ANDROID_RELEASE_STORE_PASSWORD`
- `ANDROID_RELEASE_KEY_PASSWORD`
- `APPLE_DISTRIBUTION_CERTIFICATE_P12_BASE64`
- `APPLE_DISTRIBUTION_CERTIFICATE_PASSWORD`
- `APPLE_APP_STORE_PROVISIONING_PROFILE_BASE64`
- `APPLE_TEAM_ID`
- `APP_STORE_CONNECT_API_KEY_ID`
- `APP_STORE_CONNECT_API_ISSUER_ID`
- `APP_STORE_CONNECT_API_PRIVATE_KEY_BASE64`

Repository variables:

- `PRIVATE_RELEASE_REPO`, for example `HawkeyNL/PersonalJarvisReleases`
- `TAURI_SIGNING_PUBLIC_KEY`, the public half of the Tauri updater key

Never add Home Node hosts, IP addresses, DNS names, SSH users, SSH keys or
private artifact tokens to GitHub Actions. Ordinary PR and `App CI` workflows
have no production environment and receive none of the secrets above.

Generate the Tauri keypair and Android keystore outside the repository. Keep
both private keys in owner-controlled backup storage. Losing either key blocks
updates to installed clients; replacing the Android key also prevents Android
from installing an APK over the existing application.

## Trusted release procedure

1. Merge a reviewed version bump to `main`. Desktop `tauri.conf.json` and
   `src-tauri/Cargo.toml`, Android's default `versionName`, and iOS
   `MARKETING_VERSION` must match. Android `versionCode` and the iOS build
   number must never decrease.
2. From the Actions page on `main`, dispatch **Private Application Release**
   with application SemVer, Android version code and iOS build number.
3. Approve the protected `private-app-release` environment.
4. The workflow creates or resumes a source-revision-bound private draft,
   builds all platforms with locked Cargo/npm dependency state, signs them,
   Developer ID-signs/notarizes/staples the macOS output, uploads the iOS archive
   to App Store Connect and stages every other artifact in the private draft.
5. Only after all platform jobs succeed does the final job create
   `latest.json`, publish the draft and mark it as the private latest release.

No job calls SSH, knows a Home Node address or publishes to the public source
repository. A failed platform job leaves an inactive private draft and cannot
change the Home Node's active release.

## Manifest contract

`jarvis-app/update-release/schema/manifest-v1.schema.json` documents the JSON
shape. Runtime validation in `manifest.py` is stricter: it enforces the exact
five-target matrix, canonical relative object paths, SemVer, RFC 3339 time,
distribution method, SHA-256, artifact size and signature scheme. Manifests
contain no hostname, credential or local path. The Home Node supports a private
GitHub Releases adapter and a generic credential-free HTTPS URL template, so
storage can later move without changing clients or the manifest contract.

## Home Node installation and configuration

The first production deployment is intentionally manual and owner-reviewed:

```bash
sudo deploy/app-updates/install-app-update-sync.sh
sudo install -o root -g root -m 0640 \
  deploy/app-updates/config.example.json \
  /etc/jarvis/app-updates/config.json
sudo install -o root -g root -m 0600 /secure/input/token \
  /etc/jarvis/app-updates/private-release-token
```

For a private GitHub release repository, configure its identifier locally:

```json
{
  "kind": "github-releases",
  "repository": "OWNER/PRIVATE_REPO",
  "bearer_token_file": "/etc/jarvis/app-updates/private-release-token"
}
```

These values belong only in the root-owned local config; replace the example
owner/repo locally. The adapter resolves release assets through GitHub's
authenticated Releases API and never puts the token in a URL. Use a read-only
fine-grained token for the private release repository. Install Android SDK
Build Tools `apksigner`, set its absolute path, and set the real lowercase
Android signing certificate SHA-256 in `android_signing_certificate_sha256`.

Test one pull before enabling the timer:

```bash
sudo systemctl start jarvis-app-update-sync.service
sudo systemctl status jarvis-app-update-sync.service
sudo systemctl enable --now jarvis-app-update-sync.timer
```

The local layout is:

```text
/var/lib/jarvis/app-updates/
├── current -> releases/v0.1.6
├── manifests/latest.json
└── releases/
    ├── v0.1.5/
    └── v0.1.6/
```

Every pull validates the complete manifest, pinned Android identity, every
mirrored file's exact size and SHA-256, path safety and allowed targets in a
temporary staging directory. It atomically replaces `current` only after all
checks succeed, then retains the active version and one previous verified
version. Failure leaves `current` unchanged.

Authenticated source redirects are restricted as well: only HTTPS targets are
accepted, Authorization remains attached only for the exact same
scheme/hostname/effective-port origin, and a redirect chain never restores a
credential after an origin change.

## Authenticated client delivery

Enable Core serving only after the mirror exists by setting both root-operated
runtime values in the Home Node service configuration:

```text
JARVIS_APP_UPDATE_MIRROR_ROOT=/var/lib/jarvis/app-updates
JARVIS_APP_UPDATE_PUBLIC_BASE_URL=https://the-enrolled-private-core-origin
```

The origin is deployment-local configuration, never CI or manifest data. Core
fails startup if only one value is present or the origin is not credential-free
HTTPS. `/v1/app-updates/**` requires the same enrolled-device bearer session as
other protected Jarvis APIs. Artifact routes expose identifiers, never raw
filesystem paths.

The desktop stores the enrolled Home Node origin at runtime as ordinary local
metadata. Production accepts credential-free HTTPS only; loopback HTTP is an
explicit debug-build option. There is no `VITE_JARVIS_API_BASE` release input
or implicit production localhost fallback. All API requests resolve the active
origin at call time.

After authentication, Rust calls `GET /v1/app-updates/capability` with the
OS-stored bearer token. Core returns a credential-free Tauri check template and
the active release's `minimum_client_protocol`. The current desktop updater
protocol is `1`; newer requirements are shown as incompatible and are never
offered. Capability, artifact and redirect origins are bound to the exact
enrolled origin before native Authorization is used. Vue receives only version,
state, notes and progress; it does not receive the updater endpoint, signature
or bearer token. A single delayed check runs after authenticated startup without
blocking the app; download, installation and restart remain manual.

Android does not use Tauri updater semantics. An enrolled device calls
`GET /v1/app-updates/android/{versionCode}?client_protocol=1`; Core returns
metadata only for a newer APK in the atomically active mirror generation.
`GET /v1/app-updates/android/download` is authenticated and parameterless, so
no filename or filesystem path can be selected by the client. The native app
disables redirects, binds the URL to its enrolled origin, bounds the download,
checks size/SHA-256/package/version and requires the APK certificate to equal
both the installed Jarvis signer and the manifest signer before opening the
Android package installer. Unknown-app installation permission is explained
and opened explicitly; Jarvis never enables it silently.

Offline, stale, corrupt, incompatible and bad-signature failures leave the
currently installed desktop or Android app usable. iOS remains TestFlight-only
and has no custom update endpoint or installer.

## Verification

Run without production secrets:

```bash
cd jarvis-app
npm ci
npm run check
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s update-release/tests -v
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s update-mirror/tests -v
cargo test --manifest-path src-tauri/Cargo.toml --locked

cd ../jarvis-android
./gradlew testDebugUnitTest lintDebug assembleDebug --no-daemon
```

Production signing and the first real Home Node pull cannot be truthfully
validated without owner-provisioned protected secrets and configuration. They
must be reviewed from the trusted workflow and real devices before declaring a
release ready.
