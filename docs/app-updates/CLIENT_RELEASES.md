# Signed Jarvis client releases and Home Node delivery

## Ownership

- `HawkeyNL/PersonalJarvisApp` owns the desktop, Android, and iOS source plus
  the coordinated desktop/Android `app-vX.Y.Z` release workflow. iOS device
  signing and installation are local owner actions outside GitHub CI/CD.
- `HawkeyNL/PersonalJarvis` owns Core/Home Node, `crates/client-core`, the
  application mirror, and authenticated `/v1/app-updates/**` delivery.

Core `vX.Y.Z` and app `app-vX.Y.Z` are independent. Protocol fields, not SemVer
equality, define compatibility. No third release repository or sibling checkout
is required in production.

The public PersonalJarvisApp GitHub Release is untrusted artifact transport.
The Home Node pulls it outbound and validates the signed `latest.json`, release
identity, complete downloadable matrix, sizes, hashes, Tauri updater signatures, and
pinned Android signing identity before atomic promotion. A public downloader or
compromised mirror cannot sign a future update. Signing private keys never
belong on the Home Node.

The unified manifest contains:

- Linux x86_64, Windows x86_64, and macOS arm64 Tauri updater artifacts;
- a signed Android universal APK plus its monotonically increasing versionCode;
- supplemental notarized macOS DMG and Android store AAB metadata.

The Home Node mirrors downloadable desktop assets, the APK, DMG, AAB, and the
manifest. iOS source is simulator-tested in client CI and installed locally
from Xcode using the owner's Personal Team or registered-device development
signing. It has no IPA, manifest target, or Home Node update artifact. Only the
three desktop updater targets and Android APK can be selected by the
authenticated client API.

```text
/var/lib/jarvis/app-updates/
├── current -> releases/v0.1.1
├── manifests/latest.json -> ../current/manifest.json
└── releases/
    ├── v0.1.0/
    └── v0.1.1/
```

Staging and all verification happen before the `current` symlink changes. The
active generation and one verified predecessor are retained. Corrupt, partial,
stale, or signature-invalid upstream data leaves the current generation active.
Migration from a valid historical desktop-only or mobile-only generation to a
new unified `clients` generation is allowed; replacing a unified generation
with a partial product is refused.

## Home Node setup after the first client release

These are owner actions to perform only after a reviewed Core release contains
this mirror parser and a valid `app-vX.Y.Z` exists.

From a reviewed PersonalJarvis checkout:

```bash
sudo apt install python3 minisign apksigner
sudo bash deploy/app-updates/install-app-update-sync.sh
sudo install -o root -g root -m 0640 \
  deploy/app-updates/config.example.json \
  /etc/jarvis/app-updates/config.json
sudoedit /etc/jarvis/app-updates/config.json
```

Do not overwrite an existing customized config without reviewing it. The
source remains token-free because the upstream repository is public:

```json
{
  "kind": "github-releases",
  "repository": "HawkeyNL/PersonalJarvisApp"
}
```

Replace `tauri_signing_public_key` with the reviewed public updater key and
`android_signing_certificate_sha256` with the lowercase SHA-256 of the stable
APK signing certificate. Keep `/usr/bin/apksigner` unless the trusted package
installed it elsewhere. Never derive either trust anchor from an unverified
download and never put a private signing key in this configuration.

Run one manual sync before enabling the timer:

```bash
sudo systemctl start jarvis-app-update-sync.service
sudo systemctl status --no-pager jarvis-app-update-sync.service
sudo readlink -f /var/lib/jarvis/app-updates/current
sudo jq '.release' /var/lib/jarvis/app-updates/current/manifest.json
sudo find /var/lib/jarvis/app-updates/current -maxdepth 3 -type f -print
sudo -u jarvis test -r /var/lib/jarvis/app-updates/current/manifest.json
sudo systemctl enable --now jarvis-app-update-sync.timer
```

Configure Core locally with a credential-free production HTTPS origin entered
by the owner; do not commit that deployment address:

```text
JARVIS_APP_UPDATE_MIRROR_ROOT=/var/lib/jarvis/app-updates
JARVIS_APP_UPDATE_PUBLIC_BASE_URL=https://your-enrolled-home-node-origin
```

Then:

```bash
sudo systemctl restart jarvis-core.service
read -r -p 'Existing Home Node HTTPS origin: ' jarvis_origin
curl --fail --proto '=https' "${jarvis_origin%/}/readyz"
unset jarvis_origin
```

Test `/v1/app-updates/capability` through an enrolled native client session.
Unauthenticated update metadata/downloads must remain denied. Do not put bearer
tokens in shell arguments or expose them to Vue. Existing Caddy HTTPS on TCP 443
is sufficient; no additional port is opened.

## Security and compatibility

Production clients receive the Home Node origin at runtime and require HTTPS.
Origin changes clear server-specific session/device binding before use. Desktop
auth/update requests remain in native Rust, Android validates its package and
certificate before invoking the system installer, and iOS device installation
remains a local Xcode operation. The Home Node can fetch public GitHub Releases without a token; if an
optional token is configured later, redirects strip Authorization on origin
change and logs/URLs never contain it.

Legacy owner-import support remains readable for already staged mobile
generations, but new client releases are generated only in PersonalJarvisApp.
PersonalJarvis contains no mobile client source, mobile build workflow, or
production client signing secret references.

## Developer verification

```bash
python3 -B -m unittest discover -s tools/app-updates/update-release/tests -v
python3 -B -m unittest discover -s tools/app-updates/update-mirror/tests -v
bash deploy/app-updates/tests/test-app-update-assets.sh
cargo test -p jarvis-api -p jarvis-client-core --locked
```

Mock tests cannot prove production Tauri signing, APK signing continuity,
macOS notarization, local iPhone installation, or an actual published release.
