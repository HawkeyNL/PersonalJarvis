# Jarvis Core on the Ubuntu Home Node

This is the operator runbook for the first Home Node deployment. It runs Jarvis
Core as a native, unprivileged `systemd` service. PostgreSQL is a separately
operated Docker service; Core is deliberately not part of the Docker stack and
does not receive Docker, root, or arbitrary-shell access.

Do these steps from a trusted administrator session on the Ubuntu Desktop LTS
Home Node. Remote access must already use RivetLink or another verified private
network; do not expose SSH, the API, PostgreSQL, RDP/VNC, or the Docker socket to
the public internet.

## 0. Do not deploy before this gate is green

Build only a reviewed commit and run these from its repository checkout:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
# Test-only database. Do not point #[sqlx::test] at the production database.
docker compose -f deploy/compose/docker-compose.yml up -d --wait
DATABASE_URL=postgres://jarvis:jarvis_dev_pw@localhost:5432/jarvis cargo test --all
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
- Install Docker Engine with the Compose plugin for PostgreSQL. Keep its management
  socket local; do not add the `jarvis` account to the `docker` group.
- Install `curl` and `jq`; the updater uses them to fetch and validate release
  metadata and assets. Ubuntu's standard `tar`, `sha256sum` and `flock` tools are
  also required.
- Create a separate PostgreSQL 17 instance with durable storage and backups. It
  must be reachable only from the Home Node (for example, published on
  `127.0.0.1` only). Do **not** use `deploy/compose/docker-compose.yml` as a
  production manifest: it is explicitly a development stack.
- Confirm the database is healthy and create a database/user dedicated to Jarvis.
  Store its strong password only in the root-managed Core environment file below.

Create the service identity and required directories:

```bash
sudo useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
sudo install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis
sudo install -d -o root -g root -m 0755 /opt/jarvis/releases /etc/jarvis
```

## 2. Stage an immutable, tagged release

For a node that will receive automatic updates, bootstrap from the verified
GitHub release archive, not a locally built directory. The archive includes the
binary, `core/Jarvis.md`, an immutable tag/revision/migration manifest and its
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
  '.tag == $tag and (.revision | test("^[0-9a-f]{40}$")) and (.migrations_sha256 | test("^[0-9a-f]{64}$"))' \
  "/opt/jarvis/releases/${tag}/release.json"
sudo ln -sfn "/opt/jarvis/releases/${tag}" /opt/jarvis/current
```

Verify that the service account cannot alter release contents:

```bash
sudo -u jarvis test ! -w /opt/jarvis/current/jarvis-api
sudo -u jarvis test ! -w /opt/jarvis/current/core/Jarvis.md
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
JARVIS_DATABASE_URL=postgres://<jarvis-user>:<strong-password>@127.0.0.1:5432/<jarvis-db>

# First deployment: agents and code execution remain off.
JARVIS_AGENT_ENABLED=false
JARVIS_AGENT_CLAUDE_CODE_ENABLED=false
JARVIS_AGENT_WORKSPACE_ROOT=

# No reverse proxy configured: ignore all X-Forwarded-For headers.
JARVIS_TRUSTED_PROXY_HOPS=0
JARVIS_TRUSTED_PROXY_IPS=
```

Add an LLM provider key only if it is needed. Empty provider keys are safer than
copying development credentials. Confirm its absence from shell history and logs.

RivetLink is not automatically a trusted HTTP proxy. Leave the proxy settings at
zero unless a specific reverse proxy directly connects to Core. In that case, set
the exact number of hops and direct proxy IPs; that proxy must overwrite any
incoming `X-Forwarded-For` header. Core refuses to start when hops are enabled
without an IP allowlist.

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

The installer deliberately does not provision Docker/PostgreSQL, create secrets,
change firewall rules, or roll back database migrations. Those actions require
separate operator verification.

The unit runs as `jarvis`, restarts only after failures, uses a restrictive umask,
has no Linux capabilities, cannot gain privileges, and cannot write the release or
read ordinary home directories.

## 5. Verify immediately

Run these on the Home Node. `readyz` validates PostgreSQL connectivity; a failure
after a migration problem must keep the Core from serving production traffic.

```bash
sudo systemctl status jarvis-core --no-pager
sudo journalctl -u jarvis-core -b --no-pager
curl --fail http://127.0.0.1:8080/livez
curl --fail http://127.0.0.1:8080/readyz
sudo systemctl show jarvis-core -p User -p NoNewPrivileges -p CapabilityBoundingSet -p ProtectSystem
```

Confirm externally only through the authorised private-network path. Do not solve
connectivity failures by binding `0.0.0.0`, disabling the firewall, or trusting
forwarding headers.

## 6. Automatic, tag-based Core updates

Pushing a stable semantic-version tag such as `v0.1.0` starts the `Release Jarvis
Core` GitHub workflow. It runs the full format, Clippy, test and dependency-audit gate again,
then publishes an Ubuntu x86_64 release archive containing only `jarvis-api`,
`core/Jarvis.md`, a tag/revision manifest with a database-migration fingerprint,
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
  PostgreSQL health after reboots and updates.
- Back up PostgreSQL independently from the container lifecycle and test a restore
  before holding irreplaceable data.
- Review `journalctl -u jarvis-core` and Jarvis security/agent audit events without
  copying secrets into tickets or chat.
- Keep agent execution disabled until its sandbox root, signed approvals, audit
  retention, and operator recovery path have been separately exercised.
