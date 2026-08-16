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
DATABASE_URL=postgres://... cargo test --all
cargo audit
```

At the time this guide was added, `cargo audit` reports findings for `rkyv` and
`rsa`. Resolve them or record an explicit, time-bounded security exception before
the first production deployment. Do not treat a passing build/test as a substitute
for this gate.

## 1. Prepare the host

Before installing Jarvis:

- Install Ubuntu security updates, enable time synchronisation, and configure the
  host firewall with default-deny inbound rules.
- Install Docker Engine with the Compose plugin for PostgreSQL. Keep its management
  socket local; do not add the `jarvis` account to the `docker` group.
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

## 2. Build and stage an immutable release

Build in a clean checkout of the reviewed commit. Prefer a trusted build machine
or CI artifact; never let the running Jarvis process update itself.

```bash
git rev-parse HEAD
cargo build --release -p jarvis-api
```

Copy the release to a versioned directory. Substitute `<commit>` with the exact
reviewed commit ID. The release must contain both the binary and `core/Jarvis.md`:

```bash
sudo install -d -o root -g root -m 0755 /opt/jarvis/releases/<commit>/core
sudo install -o root -g root -m 0755 target/release/jarvis-api /opt/jarvis/releases/<commit>/jarvis-api
sudo install -o root -g root -m 0644 core/Jarvis.md /opt/jarvis/releases/<commit>/core/Jarvis.md
sudo ln -sfn /opt/jarvis/releases/<commit> /opt/jarvis/current
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
files, verifies the unit, and checks both health endpoints:

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

## 6. Upgrade and rollback

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

## 7. Operational checks to schedule

- Check `systemctl status jarvis-core`, `readyz`, disk capacity, temperature, and
  PostgreSQL health after reboots and updates.
- Back up PostgreSQL independently from the container lifecycle and test a restore
  before holding irreplaceable data.
- Review `journalctl -u jarvis-core` and Jarvis security/agent audit events without
  copying secrets into tickets or chat.
- Keep agent execution disabled until its sandbox root, signed approvals, audit
  retention, and operator recovery path have been separately exercised.
