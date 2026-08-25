# Codex Goal — Production Home Node Bootstrap

Implement the missing production bootstrap/deployment path for the Ubuntu 26.04 UM890 Home Node so the owner can install PersonalJarvis safely and reproducibly from the repository without hand-assembling production infrastructure.

## Current real-machine state

The target Home Node is already prepared with:

- Ubuntu 26.04 LTS
- Docker Engine 29.x + Docker Compose v5
- RivetLink installed and working
- GitHub SSH access configured
- repository cloned at `/home/gus-jarvis-home/PersonalJarvis`
- fixed LAN IP managed by the router

Current `main` contains:

- `deploy/systemd/*`
- `deploy/caddy/*`
- development compose stack
- OpenSandbox deployment foundation
- SurrealDB-backed Core

The problem is that the current runbook still requires too much manual production setup, especially for SurrealDB provisioning, service identities, first release bootstrap, secret generation and verification.

Do not turn the development compose file into a production manifest by accident. Build an explicit production bootstrap path.

## Goal

After this change, a fresh supported Ubuntu Home Node with Docker installed should be able to follow one guarded, documented sequence that prepares:

1. dedicated service accounts/directories;
2. a pinned production SurrealDB service bound to loopback only;
3. durable storage and backup-ready paths;
4. a database-scoped Jarvis user with least privilege;
5. root-owned Jarvis configuration templates;
6. one-time LAN-only owner bootstrap secret generation;
7. immutable Jarvis release staging;
8. systemd Core installation;
9. local `/livez` and `/readyz` verification;
10. reboot verification;
11. later/optional Caddy and OpenSandbox steps without exposing them early.

## Production SurrealDB deployment

Add a dedicated production manifest or equivalent deployment assets under a clearly named path such as:

`deploy/surrealdb/`

Requirements:

- pin the exact supported SurrealDB 2.6 image by immutable digest where practical;
- do not use `latest`;
- bind the database listener only to `127.0.0.1:8000`;
- never expose SurrealDB publicly or on the LAN;
- use durable host/volume storage with explicit ownership;
- define a healthcheck;
- use automatic restart appropriate for a persistent Home Node service;
- use sane memory/CPU/process limits where practical without starving migrations;
- do not mount the Docker socket;
- do not run privileged;
- document upgrade and rollback semantics;
- make the production manifest unmistakably different from the development compose stack.

## SurrealDB provisioning

Create an idempotent root/operator provisioning helper that can safely initialize:

- namespace: configurable, default `jarvis`
- database: configurable, default `core`
- dedicated Core database user: configurable, default `core`
- minimum required permissions for normal Jarvis operation

The root/admin database credential must be provisioning-only.

After successful provisioning:

- Core must use only the scoped database user;
- the root credential must not be written into `/etc/jarvis/core.env`;
- the helper must not print passwords to logs;
- repeated provisioning must not destroy existing data or rotate credentials silently;
- existing production data must never be reset by a normal installer rerun.

Provide a secure operator flow for supplying/generating credentials without placing them in shell history.

## Home Node preparation helper

Create a guarded setup helper for host preparation, for example:

`deploy/systemd/prepare-home-node.sh`

or another location consistent with the repository.

It should idempotently prepare only repository-owned requirements such as:

- `jarvis` system account;
- `/var/lib/jarvis`;
- `/opt/jarvis/releases`;
- `/etc/jarvis`;
- required ownership/modes;
- required systemd helper directories.

Do not:

- add `jarvis` to the Docker group;
- give Jarvis sudo;
- modify unrelated user accounts;
- open router ports;
- automatically expose SSH;
- enable OpenSandbox during the first Core bootstrap.

## Production configuration generation

Add a safe config-generation/bootstrap helper or documented command flow for `/etc/jarvis/core.env`.

It must:

- generate a strong one-time first-owner bootstrap secret;
- display the raw bootstrap secret exactly once to the operator;
- store only its SHA-256 verifier in `core.env`;
- default bootstrap CIDR to an operator-confirmed LAN CIDR rather than guessing silently;
- generate or request a strong SurrealDB Core-user password securely;
- set production-safe defaults;
- bind Core to `127.0.0.1:8080`;
- keep agents and code execution disabled initially;
- configure trusted proxy peers only for loopback Caddy;
- create the file as `root:jarvis` mode `0640`;
- never commit secrets;
- never print persistent secrets in logs after initial presentation.

Do not require a public domain for the first local deployment.

Allow `JARVIS_PUBLIC_HOSTNAME` to remain intentionally unset/local until Caddy is configured, if the production validation permits that. If current Core validation requires a public hostname too early, refactor validation so local-first production bootstrap is possible without weakening public-ingress rules.

## Release bootstrap

The current repository has older release artifacts while `main` may be newer.

Improve the release/runbook path so the owner cannot accidentally install a stale binary merely because it is the latest published release.

Provide a clear supported flow for:

- using a reviewed tagged release;
- verifying SHA-256;
- verifying `release.json`;
- staging under `/opt/jarvis/releases/<tag-or-revision>`;
- immutable/root-owned release contents;
- atomically switching `/opt/jarvis/current`;
- keeping the previous release available for rollback.

If no release exists for the currently reviewed commit, the deployment docs must explicitly instruct the operator to create/wait for a release rather than silently build an unreviewed production binary on the Home Node.

Do not automatically create Git tags from the Home Node installer.

## Core systemd integration

Review `jarvis-core.service` and `install-home-node-core.sh` against the actual production layout.

Ensure:

- Core runs as the unprivileged `jarvis` service account;
- no Docker-group access;
- no root execution;
- no unnecessary capabilities;
- restrictive filesystem access;
- release tree is read-only to Core;
- `/etc/jarvis/core.env` is readable but not writable by Core;
- restart policy has sane backoff;
- startup fails closed when SurrealDB is unavailable or credentials are invalid;
- `/livez` and `/readyz` behavior remains correct;
- systemd verification is part of installation.

## Local-first deployment

The first supported production milestone should work entirely on the Home Node without opening any router port.

Required local architecture:

```text
127.0.0.1:8080  jarvis-core/api
      |
      +----> 127.0.0.1:8000 SurrealDB

No Caddy required yet
No public DNS required yet
No public router forwarding
OpenSandbox disabled
Agents disabled
```

The owner should be able to verify Core locally before moving on to public ingress.

## Public ingress remains a separate phase

Do not automatically install/configure Caddy as part of first Core bootstrap.

Keep the existing security rule:

- only TCP 443 public;
- Caddy is the only public HTTP ingress;
- Core remains loopback-only;
- SurrealDB remains loopback-only;
- no 8000/8080/SSH/Docker/Codex/OpenSandbox public exposure;
- no UPnP/NAT-PMP automation.

Update docs so public ingress clearly happens only after local Core readiness succeeds.

## OpenSandbox remains gated

Do not enable OpenSandbox during this goal.

Preserve ADR-040 fail-closed behavior.

The production bootstrap may install files needed later, but must not enable workload execution until the real Ubuntu adversarial isolation gate is completed.

## Backup readiness

Add/document the minimum production backup surface before the Home Node is considered ready:

- SurrealDB durable data;
- `/etc/jarvis` configuration;
- device/trust state required for recovery;
- release metadata/current symlink state where useful.

Do not copy plaintext secrets to an unencrypted backup target.

Provide a restore verification procedure that restores into an isolated instance before replacing production data.

## Reboot safety

Provide an explicit real-machine reboot test.

After reboot, verify:

- Docker active;
- SurrealDB healthy;
- Jarvis Core active;
- Core still loopback-only;
- SurrealDB still loopback-only;
- `/livez` returns success;
- `/readyz` returns success;
- no agent/OpenSandbox service became enabled unexpectedly.

## Verification helper

Add a read-only verification script if useful, e.g.:

`deploy/systemd/verify-home-node.sh`

It may report:

- service account presence;
- directory ownership/modes;
- listening sockets;
- Docker/SurrealDB health;
- Core health;
- effective systemd hardening basics;
- whether forbidden services/ports are exposed.

It must not mutate the host.

## Tests and CI

Add tests for deployment assets where practical:

- production SurrealDB manifest binds loopback only;
- image is pinned/non-floating;
- no Docker socket mount;
- no privileged container;
- generated config permissions/defaults are safe;
- installer idempotency;
- installer refuses unsafe ownership/modes;
- release verification fails on bad checksum/manifest;
- Core service stays unprivileged;
- local-first mode does not require Caddy/public hostname unnecessarily;
- OpenSandbox remains disabled by default.

Keep existing fmt/clippy/test/audit gates.

## Documentation

Update at minimum:

- `docs/HOME_NODE_DEPLOYMENT.md`
- `deploy/systemd/README.md`
- relevant ADR/status/roadmap docs

The runbook should have a concise operator sequence for a real Ubuntu 26.04 Home Node:

1. host prerequisites
2. host preparation
3. production SurrealDB start
4. SurrealDB scoped-user provisioning
5. verified release staging
6. `core.env` bootstrap
7. Core install/start
8. local health verification
9. reboot verification
10. only then optional public Caddy phase
11. only later optional OpenSandbox gate

Commands must be copy/pasteable and must clearly mark placeholders/secrets.

## Definition of done

Do not finish with documentation only.

This milestone is complete when:

- there is a real production SurrealDB deployment path separate from dev compose;
- SurrealDB is loopback-only and durable;
- Core uses a scoped DB user rather than root;
- Home Node service account/directories can be prepared idempotently;
- production configuration can be created safely;
- first-owner bootstrap secret is one-time and only hashed at rest;
- a reviewed release can be staged and verified safely;
- Core runs natively as an unprivileged systemd service;
- Core + SurrealDB work locally before Caddy exists;
- local health and readiness checks pass;
- reboot brings both services back safely;
- no public ports are opened by the installer;
- OpenSandbox remains disabled;
- rollback/backup guidance exists;
- deployment security tests pass;
- exact real-machine verification commands are provided.

Do not weaken existing security boundaries merely to make installation easier.
