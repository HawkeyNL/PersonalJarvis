# Jarvis Core on the Ubuntu Home Node

This is the operator runbook for the first Home Node deployment. It runs Jarvis
Core as a native, unprivileged `systemd` service. SurrealDB is a separately
operated Docker service; Core is deliberately not part of the Docker stack and
does not receive Docker, root, or arbitrary-shell access.

Do these steps from a trusted administrator session on the Ubuntu Desktop LTS
Home Node. SSH, SurrealDB, RDP/VNC, Codex and the Docker socket remain private.
The only approved public application ingress is Caddy on TCP 443, as documented
in [ADR-038](../../decisions/ADR-038-PUBLIC-HTTPS-INGRESS.md).

## 0. Do not deploy before this gate is green

Build only a reviewed commit and run these from its repository checkout:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
# Test-only database. Do not point wire-protocol tests at the production database.
docker compose -f deploy/compose/docker-compose.yml up -d --wait
cargo test --all
cargo audit
```

`cargo audit` must pass. The repository currently carries one narrow, expiring
exception for an uncompiled optional `rust_decimal` → `rkyv` feature; CI rejects
it automatically on 2026-10-01. Do not add new ignores or extend that exception
without a separate security review. A passing build/test is not a substitute for
this gate.

## 1. Prepare the host

Before installing Jarvis:

- Install Ubuntu security updates, enable time synchronisation, and configure the
  host firewall with default-deny inbound rules.
- Install Docker Engine with the Compose plugin for SurrealDB. Keep its management
  socket local; do not add the `jarvis` account to the `docker` group.
- Install `curl` and `jq`; the updater uses them to fetch and validate release
  metadata and assets. Ubuntu's standard `tar`, `sha256sum` and `flock` tools are
  also required.
- Create a separate pinned SurrealDB 2.6 instance with durable storage and backups. It
  must be reachable only from the Home Node (for example, published on
  `127.0.0.1` only). Do **not** use `deploy/compose/docker-compose.yml` as a
  production manifest: it is explicitly a development stack.
- Confirm the database is healthy and create a namespace/database plus a database-scoped `EDITOR` user dedicated to Jarvis. The root account is provisioning-only.
  Provision it from the isolated database container or a trusted administrator
  session; then discard the root credential from the Core environment. Core's
  `/etc/jarvis/core.env` contains only the database-scoped account below.
  Store its strong password only in the root-managed Core environment file below.

Create the service identity and required directories:

```bash
sudo useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
sudo install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis
sudo install -d -o root -g root -m 0755 /opt/jarvis/releases /etc/jarvis
```

## Optional Codex engineering isolation

Codex App Server is **not part of the first Home Node deployment**. Do not
enable this unit until the signed-approval API path for engineering tasks is
implemented and reviewed.

When it is enabled later, it runs as a separate non-login
`jarvis-codex` account. Jarvis Core is only a supplementary member of the
`jarvis-codex` group, so it can connect to the local Unix socket at
`/run/jarvis-codex/app-server.sock`. Codex credentials and state remain in
`/var/lib/jarvis-codex` with mode `0700`; Core must never read, write, or
inherit them. The App Server has no TCP listener and must never be exposed
through a public proxy.

Prepare the account and install the optional files as root:

```bash
sudo useradd --system --user-group --home-dir /var/lib/jarvis-codex \
  --shell /usr/sbin/nologin jarvis-codex
sudo install -d -o root -g jarvis-codex -m 0770 /var/lib/jarvis-engineering
sudo install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
sudo install -o root -g root -m 0755 deploy/systemd/prepare-codex-worktree.sh \
  /usr/local/libexec/jarvis/prepare-codex-worktree
sudo install -o root -g root -m 0644 deploy/systemd/jarvis-codex.service \
  /etc/systemd/system/jarvis-codex.service
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/jarvis-codex.service
```

The root-only worktree helper accepts only a primary checkout, UUID task ID,
and immutable commit revision. It rejects secret-like files (the reviewed,
non-secret `.env.example` template is the sole environment-file exception) and makes
`jarvis-core` and `.git` root-owned, non-writable, and immutable with
`chattr +i`. This prevents the Codex account from changing, deleting, or
renaming those paths. It fails closed when the filesystem does not support
immutable attributes; deploy this optional boundary only on a supported local
filesystem such as the default Ubuntu ext4 installation.

The helper is deliberately not setuid, an API endpoint, or an agent tool. An
operator invokes it with `sudo` only after an approved task has been
materialized by the future engineering-task control plane. Never put Codex
credentials in `/etc/jarvis/core.env`, the Core systemd unit, source control,
or an agent prompt.

## Optional OpenSandbox execution isolation

OpenSandbox is a supporting Docker service, not part of Jarvis Core and not a
public API. Install it only after completing the adversarial verification gate
in [`../opensandbox/README.md`](../opensandbox/README.md). The unit runs as
root because its **trusted control plane** needs Docker; `jarvis-core` remains
the unprivileged `jarvis` user and never receives Docker-group membership or a
socket mount.

From a reviewed release/check-out, install the fixed deployment directory and
unit as root:

```bash
sudo install -d -o root -g root -m 0755 /opt/jarvis/opensandbox
sudo cp -a deploy/opensandbox/. /opt/jarvis/opensandbox/
sudo install -o root -g root -m 0755 deploy/opensandbox/validate-opensandbox-env.sh \
  /usr/local/libexec/jarvis/validate-opensandbox-env
sudo install -o root -g root -m 0644 deploy/systemd/jarvis-opensandbox.service \
  /etc/systemd/system/jarvis-opensandbox.service
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/jarvis-opensandbox.service
# Do not enable it before the OpenSandbox runtime gate is complete.
sudo systemctl enable --now jarvis-opensandbox.service
sudo systemctl status jarvis-opensandbox.service --no-pager
```

The service only reads `/etc/jarvis/opensandbox.env` (owner `root:root`, mode
`0600`) and never exposes its API key to Jarvis Core or sandbox workloads. Verify
`127.0.0.1:8090` and the dynamic 41000–41150 ports are loopback-only after
every upgrade; if any listen publicly, stop and disable this unit immediately.

## 2. Stage an immutable, tagged release

For a node that will receive automatic updates, bootstrap from the verified
GitHub release archive, not a locally built directory. The archive includes the
binary, `jarvis-core/Jarvis.md`, an immutable tag/revision/migration manifest and its
published SHA-256 checksum. It is produced only after the release workflow's
format, Clippy, test and audit gates pass.

After the reviewed changes are on `main`, create and push the first stable tag
from a trusted developer workstation:

```bash
git switch main
git pull --ff-only origin main
git tag -a v0.1.0 -m "Jarvis Core v0.1.0"
git push origin v0.1.0
```

Wait for **Release Jarvis Core** to succeed in GitHub Actions. Then, on the
Home Node, download and verify that exact release. Replace `v0.1.0` only with a
stable tag that completed its release workflow:

```bash
tag=v0.1.0
archive="jarvis-core-${tag}-linux-x86_64.tar.gz"
base_url="https://github.com/HawkeyNL/PersonalJarvis/releases/download/${tag}"

cd /var/tmp
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  -O "${base_url}/${archive}"
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  -O "${base_url}/${archive}.sha256"
sha256sum --strict --check "${archive}.sha256"

sudo install -d -o root -g root -m 0755 /opt/jarvis/releases
sudo tar -xzf "$archive" --no-same-owner --no-same-permissions -C /opt/jarvis/releases
sudo mv "/opt/jarvis/releases/jarvis-core-${tag}" "/opt/jarvis/releases/${tag}"
sudo chown -R root:root "/opt/jarvis/releases/${tag}"
sudo chmod -R go-w "/opt/jarvis/releases/${tag}"
```

Validate the extracted manifest before setting it active:

```bash
sudo jq -e --arg tag "$tag" \
  '.tag == $tag and (.revision | test("^[0-9a-f]{40}$")) and (.schema_sha256 | test("^[0-9a-f]{64}$"))' \
  "/opt/jarvis/releases/${tag}/release.json"
sudo ln -sfn "/opt/jarvis/releases/${tag}" /opt/jarvis/current
```

Verify that the service account cannot alter release contents:

```bash
sudo -u jarvis test ! -w /opt/jarvis/current/jarvis-api
sudo -u jarvis test ! -w /opt/jarvis/current/jarvis-core/Jarvis.md
```

## 3. Create the production secret/configuration file

Create `/etc/jarvis/core.env` with a root-owned editor, then restrict it:

```bash
sudoedit /etc/jarvis/core.env
sudo chown root:jarvis /etc/jarvis/core.env
sudo chmod 0640 /etc/jarvis/core.env
```

Use this minimum baseline. Replace only the marked values; never put secrets in
the unit, release directory, repository, shell history, or screenshots.

```dotenv
JARVIS_ENVIRONMENT=production
JARVIS_LOG_JSON=true
JARVIS_BIND_ADDR=127.0.0.1:8080
JARVIS_SURREAL_ENDPOINT=127.0.0.1:8000
JARVIS_SURREAL_NAMESPACE=jarvis
JARVIS_SURREAL_DATABASE=core
JARVIS_SURREAL_USERNAME=core
JARVIS_SURREAL_PASSWORD=<strong-password>

# First deployment: agents and code execution remain off.
JARVIS_AGENT_ENABLED=false
JARVIS_AGENT_CLAUDE_CODE_ENABLED=false
JARVIS_AGENT_WORKSPACE_ROOT=

# Caddy is the sole public ingress and directly connects over loopback. Core
# trusts forwarding headers only from those exact direct peers.
JARVIS_TRUSTED_PROXY_HOPS=1
JARVIS_TRUSTED_PROXY_IPS=127.0.0.1,::1
JARVIS_PUBLIC_HOSTNAME=api.example.com

# First owner only: generate the raw value locally as root, show it once to the
# owner, and put only its SHA-256 hex verifier here. Bootstrap is LAN-only.
JARVIS_BOOTSTRAP_SECRET_SHA256=<sha256-of-one-time-secret>
JARVIS_BOOTSTRAP_ALLOWED_CIDRS=192.168.1.0/24

# Per-device controls complement the anonymous per-IP auth throttles.
JARVIS_AUTHENTICATED_RATE_PER_MIN=300
JARVIS_LLM_RATE_PER_MIN=20
```

Add an LLM provider key only if it is needed. Empty provider keys are safer than
copying development credentials. Confirm its absence from shell history and logs.

RivetLink is not an HTTP proxy. Do not add it to the trusted-proxy allowlist.
For this deployment Caddy is the only trusted direct peer; do not add a CDN or
another proxy without a separate forwarding-header review. Core refuses startup
when production attempts to bind a non-loopback socket or proxy hops lack an IP
allowlist.

## 3a. Public HTTPS ingress (Caddy, TCP 443 only)

Do this only after Core and SurrealDB pass the local checks. The owner, not the
repository, controls the router and DNS:

1. Reserve a fixed LAN IP for the Home Node.
2. Check whether the connection has a real public IPv4 address (not CGNAT).
   For IPv6, publish AAAA only when the host firewall is equally strict.
3. Create an A (and optional AAAA) record for `api.<owner-domain>` pointing to
   that address. A dynamic connection may use any provider's DDNS updater; run
   that updater as its own root-managed service with a low-TTL record, never in
   Jarvis and never with DNS credentials in `core.env`.
4. Forward **TCP 443 only** on the router to the Home Node. Do not forward 80,
   8080, 8000, SSH, Docker, Codex, broker, metrics, RDP or VNC ports. Do not
   enable UPnP/NAT-PMP for Jarvis.

Install the normal Ubuntu Caddy package, then install the reviewed template and
its public (non-secret) hostname file:

```bash
sudo apt update
sudo apt install caddy
sudo install -d -o root -g root -m 0755 /etc/systemd/system/caddy.service.d
sudo install -o root -g root -m 0644 deploy/caddy/caddy.service.d-jarvis.conf \
  /etc/systemd/system/caddy.service.d/jarvis.conf
sudo install -o root -g root -m 0644 deploy/caddy/jarvis-public.env.example \
  /etc/jarvis/public.env
sudoedit /etc/jarvis/public.env
sudo install -o root -g root -m 0644 deploy/caddy/Caddyfile /etc/caddy/Caddyfile
sudo systemctl daemon-reload
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo systemctl enable --now caddy
```

The template uses TLS-ALPN-01 and disables HTTP-01, so Caddy obtains and renews
certificates over port 443. Do not test repeatedly against production ACME while
DNS or port forwarding is incomplete. Caddy's certificate storage is persistent
system state; include it in the host configuration backup, not in a repository.

Apply a minimal UFW baseline (adapt the private management subnet before
allowing SSH; omitting the SSH rule is preferred):

```bash
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 443/tcp comment 'Jarvis Caddy HTTPS'
# Optional private administration only; never use a broad public SSH rule.
sudo ufw allow from 192.168.1.0/24 to any port 22 proto tcp comment 'LAN SSH'
sudo ufw enable
sudo ufw status verbose
```

The public probes `/livez` and `/readyz` deliberately return only generic
status. All detailed diagnostics stay behind device-bound authentication.

## 4. Install and start the systemd unit

Install the versioned unit and inspect its effective configuration before enabling
it:

```bash
sudo install -o root -g root -m 0644 deploy/systemd/jarvis-core.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/jarvis-core.service
sudo systemctl enable --now jarvis-core
```

Alternatively, after you have created the environment file and staged the
release, run the guarded installer. It validates the release layout, refuses a
non-production environment or initially enabled agents, protects the release
files, installs the Core and updater unit files (but does not enable automatic
updates), verifies the units, and checks both health endpoints:

```bash
sudo bash deploy/systemd/install-home-node-core.sh /opt/jarvis/releases/<commit>
```

The installer deliberately does not provision Docker/SurrealDB, create secrets,
change firewall rules, or roll back database migrations. Those actions require
separate operator verification.

The unit runs as `jarvis`, restarts only after failures, uses a restrictive umask,
has no Linux capabilities, cannot gain privileges, and cannot write the release or
read ordinary home directories.

## 5. Verify immediately

Run these on the Home Node. `readyz` validates SurrealDB connectivity; a failure
after a migration problem must keep the Core from serving production traffic.

```bash
sudo systemctl status jarvis-core --no-pager
sudo journalctl -u jarvis-core -b --no-pager
curl --fail http://127.0.0.1:8080/livez
curl --fail http://127.0.0.1:8080/readyz
sudo systemctl show jarvis-core -p User -p NoNewPrivileges -p CapabilityBoundingSet -p ProtectSystem
```

Then verify Caddy locally and from an external mobile connection:

```bash
curl --fail --proto '=https' "https://api.example.com/livez"
curl --fail --proto '=https' "https://api.example.com/readyz"
sudo ss -ltnp '( sport = :443 or sport = :8080 or sport = :8000 )'
sudo systemctl reboot
```

After reboot, repeat both health checks. With phone Wi-Fi disabled, log in with
an already trusted device, send a chat, then verify a bad signature, revoked
device and revoked session fail. Check `journalctl -u jarvis-core -b` and the
authenticated security audit. External port scans must show 443 only; check
SSH independently from a network outside the LAN before considering it private.

### Enrollment before public exposure

`POST /v1/auth/enroll` is intentionally disabled in production. Generate a
fresh 32-byte bootstrap secret locally as root (`openssl rand -hex 32`), put
only `printf %s "$secret" | sha256sum` in `JARVIS_BOOTSTRAP_SECRET_SHA256`, and
set an explicit private LAN range in `JARVIS_BOOTSTRAP_ALLOWED_CIDRS`. Use the
raw secret once with `/v1/auth/bootstrap` from that LAN; never place it in git,
logs, shell history, or client storage. Once one active owner device exists,
the bootstrap latch remains closed. Each later device uses remote pairing and
requires biometric-gated, Ed25519-signed approval from an active device.

If every trusted device is lost, recovery is a local/root Home Node operation:
rotate the bootstrap verifier and explicitly reset the fail-closed bootstrap
latch after confirming there are no active devices. There is no public recovery
endpoint and no reason to re-enable development enrollment.

Confirm externally only through the authorised private-network path. Do not solve
connectivity failures by binding `0.0.0.0`, disabling the firewall, or trusting
forwarding headers.

## 6. Automatic, tag-based Core updates

Pushing a stable semantic-version tag such as `v0.1.0` starts the `Release Jarvis
Core` GitHub workflow. It runs the full format, Clippy, test and dependency-audit gate again,
then publishes an Ubuntu x86_64 release archive containing only `jarvis-api`,
`jarvis-core/Jarvis.md`, a tag/revision manifest with a database-migration fingerprint,
and a SHA-256 checksum. The workflow
has no SSH key, Home Node token or production secret, and never connects to the
Home Node.

The Home Node pulls the latest stable GitHub release over outbound HTTPS every
five minutes. It uses a lock, downloads into a root-only staging directory,
checks the checksum and archive layout, atomically switches `/opt/jarvis/current`,
restarts only `jarvis-core`, then requires `/readyz` to succeed. A failed
readiness check immediately restores the preceding binary release. It never
builds from source, executes release-provided scripts, prunes older releases, or
touches the database schema.

Core runs SQLx migrations during startup. A binary rollback cannot reverse a
schema migration, so the timer accepts an update **only when its migration
fingerprint matches the active tagged release**. A release that adds, removes or
changes a migration is deliberately refused by the updater. Take a tested backup
and deploy such a release manually, including its database recovery procedure.

The initial Home Node release must therefore be a verified archive produced by
this tag workflow, not an arbitrary `cargo build` directory. Extract it into
`/opt/jarvis/releases`, verify the published checksum first, and install it with
the guarded installer. That gives the active baseline its tag, revision and
migration fingerprint before the timer is enabled.

If you chose the manual Core-unit installation above rather than the guarded
installer, install the updater files before enabling its timer:

```bash
sudo install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
sudo install -o root -g root -m 0755 deploy/systemd/update-core-release.sh /usr/local/libexec/jarvis/update-core-release
sudo install -o root -g root -m 0644 deploy/systemd/jarvis-updater.service /etc/systemd/system/
sudo install -o root -g root -m 0644 deploy/systemd/jarvis-updater.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/jarvis-updater.service
```

Create the updater configuration after the initial Core installation:

```bash
sudoedit /etc/jarvis/updater.env
sudo chown root:root /etc/jarvis/updater.env
sudo chmod 0600 /etc/jarvis/updater.env
```

For this repository's public releases, the file contains only:

```dotenv
JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis
```

For a future private repository, do not place a token in this file. Create a
root-only curl netrc instead and add its path to `updater.env`:

```bash
sudoedit /etc/jarvis/github-release.netrc
sudo chown root:root /etc/jarvis/github-release.netrc
sudo chmod 0600 /etc/jarvis/github-release.netrc
```

```netrc
machine api.github.com login token password <fine-grained-read-only-token>
machine github.com login token password <fine-grained-read-only-token>
```

```dotenv
JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis
JARVIS_GITHUB_CURL_NETRC=/etc/jarvis/github-release.netrc
```

The token, if ever needed, must be fine-grained, read-only and limited to this
repository. Do not reuse a personal administration token. Enable and inspect the
timer only after the configuration has been reviewed:

```bash
sudo systemctl enable --now jarvis-updater.timer
sudo systemctl list-timers jarvis-updater.timer
sudo systemctl start jarvis-updater.service
sudo journalctl -u jarvis-updater -n 100 --no-pager
```

The checksum protects against corrupted transfer, while the GitHub repository,
tag protection, required reviews and the release workflow remain the trust root.
Before enabling automatic production updates, protect the `v*` tag namespace and
require the normal CI checks and trusted maintainers in GitHub. This updater does
refuse automatic version downgrades and does not make database migrations
reversible: review schema compatibility and restore plans separately before
publishing a release tag.

To disable automatic updates without affecting Core:

```bash
sudo systemctl disable --now jarvis-updater.timer
```

## 7. Manual upgrade and rollback

For every update, repeat the CI gate, stage a new immutable release, and retain
the previous release directory. Then switch the symlink and restart only Core:

```bash
sudo ln -sfn /opt/jarvis/releases/<new-commit> /opt/jarvis/current
sudo systemctl restart jarvis-core
curl --fail http://127.0.0.1:8080/readyz
```

If readiness fails, switch `/opt/jarvis/current` back to the previous release and
restart Core. Investigate the journal and database migration state before retrying;
never roll back database schema blindly.

## 8. Operational checks to schedule

- Check `systemctl status jarvis-core`, `readyz`, disk capacity, temperature, and
  SurrealDB health after reboots and updates.
- Back up SurrealDB independently from the container lifecycle and test a restore
  before holding irreplaceable data.
- Review `journalctl -u jarvis-core` and Jarvis security/agent audit events without
  copying secrets into tickets or chat.
- Keep agent execution disabled until its sandbox root, signed approvals, audit
  retention, and operator recovery path have been separately exercised.
