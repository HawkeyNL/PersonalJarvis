# Private GHCR iOS candidate importer

This first importer serves **manually approved unsigned iOS candidates**, not a
complete signed application release. It does not replace the authenticated
app-update mirror. Existing clients do not automatically discover/install this
IPA. The IPA needs owner-side signing. No Apple key or GitHub token goes to a
browser. Do not claim a production URL works until the owner deploys and tests it.

## Trust and scope

The trusted configuration pins the exact OCI SHA-256 digest, source commit and
application version reviewed in the successful `ios-candidate.yml` run summary.
The registry is fixed to `ghcr.io/hawkeynl/jarvis-client-artifacts`; no arbitrary
source URL or helper execution is supported. Layers are hash/size checked and
only the IPA and its manual-signing descriptor are accepted. The owner-pinned
digest, not a mutable tag or registry availability, authorizes this candidate.
The candidate is not signed production updater metadata. Never import it into
the authenticated updater or treat it as automatic release approval.

The token is root:root 0600 at `/etc/jarvis/app-downloads/ghcr.token`, in a 0700
directory. The native importer reads it directly. The entry helper reads only
the controlling terminal with hidden, bounded input; no token in argv, logs,
inherited environment, repository or GitHub Actions. Use a classic PAT with
read:packages only. Renew it before expiration through the same helper.

The separate public archive is `/var/lib/jarvis-public-downloads`. It is outside
the protected `/var/lib/jarvis` tree, whose permissions are never broadened. Caddy gets
read-only access through ordinary file permissions. It serves only the generated
index and canonical IPA filenames. The token, approval records, staging paths
and `/var/lib/jarvis/app-updates` are never mapped to public routes.

## Owner deployment after review and required signed approval

These commands are instructions, not evidence of deployment. Apply through the
existing trusted owner administration process, including real device-signed,
action-bound approval where required. Do not run deployment from an agent or
replace a working Caddyfile blindly. No production files are read by tests.

Build as the development user in the reviewed Core checkout:

```sh
cargo build --locked --release -p jarvis-app-downloads
```

After reviewing the resulting binary and installer, install without starting:

```sh
sudo bash deploy/app-updates/install-app-downloads.sh "$(pwd)/target/release/jarvis-app-downloads"
sudo /usr/local/libexec/jarvis-app-downloads-set-token
```

The second command is the **only place to paste the token**, into hidden local
TTY input. Do not paste it in a command line or give it to an assistant.

Using the owner's trusted editor, create root-owned mode 0600
`/etc/jarvis/app-downloads/config.json` with the structure from
`downloads-config.example.json`. Replace all placeholders. Obtain the source
commit and OCI digest from the reviewed successful private candidate run, not
from an untrusted message or a changing `latest` tag. No hostname belongs in this
configuration. Never put the PAT in this JSON.

Once configuration is reviewed/approved:

```sh
sudo systemctl daemon-reload
sudo systemctl start jarvis-app-downloads.service
sudo systemctl status jarvis-app-downloads.service --no-pager
```

On success the verified IPA and generated index exist. A failed request or hash
check never changes the previous index. Existing version bytes are immutable;
different bytes require a new version, not overwriting a candidate silently.

Merge **only** the reviewed `/downloads` and `@iosDownload` blocks from
`deploy/caddy/Caddyfile` into the existing site through the trusted deployment
process. Preserve the deployed hostname, TLS, Caddy environment and fallback
Core proxy. Validate the complete resulting config with the service's existing
environment before an approved reload. No new port or Nginx is needed.

Open `https://<configured-home-node>/downloads/` to download. Verify an
unauthenticated `/v1/app-updates/capability` still returns 401. Confirm approval
metadata and directory listings are not downloadable. Install the IPA with the
chosen local signing tool and test Keychain/session preservation on real iPhone.

Optional timer, **only after a successful manual import and owner approval**:

```sh
sudo systemctl enable --now jarvis-app-downloads.timer
```

This timer rechecks the pinned candidate every six hours. It does not promote a
new version automatically. Disable it if only one manual candidate is needed.
Future releases need signed release metadata, private desktop/Android transfers,
the authenticated mirror adapter and separate reviewed activation policy.

## Retention and limitations

The Rust planner supports iOS and the previously agreed version-line rules.
Automatic deletion is deliberately not connected to this initial importer:
all approved candidates remain until the transactional retention integration is
reviewed/tested. No existing protected mirror/history is deleted. The importer
checks inventory integrity before rebuilding the index, streams downloads to
disk, bounds metadata to 1 MiB and an IPA to 2 GiB, and serializes writers.
The service has a 192 MiB memory ceiling. No Docker socket is needed or granted.
This is a hardened native oneshot service; a Docker deployment has not been
implemented or claimed.

Live GHCR authentication, production Caddy activation and iPhone installation
remain owner acceptance checks; fixture tests alone do not prove those steps.
