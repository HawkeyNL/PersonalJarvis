# OpenSandbox — private Home Node control plane

This is **not** a public service and must never be added to Caddy. Jarvis Core
remains outside OpenSandbox. The internal `jarvis-sandbox` provider may use
`http://127.0.0.1:8090`; it must send `OPEN-SANDBOX-API-KEY` and keep the value
out of task inputs, logs, artifacts and sandbox environments. The provider
rejects any non-loopback endpoint and unpinned task image.

## Why this build carries a patch

At upstream commit `6b2023e9b7eb80a940d88e6ae05fcbc0eb0cf23f`, OpenSandbox
publishes dynamically allocated workload ports to `0.0.0.0`, leaves
single-tenant proxy routes unauthenticated, and can promote a private DNS answer
to its dynamic egress allowlist. The reviewed patches add `docker.publish_host`,
require the control-plane API key for every proxy request, and reject private or
special-use resolved addresses before they become egress allow rules. This
deployment fixes published ports to `127.0.0.1`; it applies to ordinary execd,
HTTP and egress-sidecar ports. The port patch was checked against upstream's
Docker/config test selection (`235 passed`); the proxy-auth patch has its
focused upstream middleware tests (`9 passed`). The egress patch applies cleanly
to the pinned upstream commit and is Go-formatted; its runtime test remains part
of the Ubuntu activation gate because this workstation does not have upstream's
Go 1.25 toolchain or Docker runtime.

Do not replace this image with an unpatched `opensandbox/server` image.

## Install (operator only)

1. Install Docker and a supported Kata Docker runtime. OpenSandbox currently
   rejects its egress network policy with gVisor, so do not configure gVisor for
   this profile. Confirm the exact runtime name with `docker info`.
2. Replace both image placeholders in `opensandbox.toml` with verified official
   image **digests**. Also record a verified immutable
   `python:3.10-slim@sha256:...` reference as `OPENSANDBOX_PYTHON_BASE_IMAGE`
   and a verified `ghcr.io/astral-sh/uv:...@sha256:...` reference as
   `OPENSANDBOX_UV_IMAGE` in the restricted environment file below. Keep
   `network_mode = "bridge"`, `publish_host = "127.0.0.1"`,
   `sandbox_binds = []`, `mode = "dns+nft"`, and IPv6 disabled.
3. Create the secret file without putting it in shell history:

   ```bash
   sudo install -d -o root -g jarvis -m 0750 /etc/jarvis
   sudo sh -c 'umask 077; openssl rand -hex 32 > /etc/jarvis/opensandbox.key'
   sudo sh -c 'printf "OPENSANDBOX_SERVER_API_KEY=%s\n" "$(cat /etc/jarvis/opensandbox.key)" > /etc/jarvis/opensandbox.env'
   sudo sh -c 'printf "%s\n" "OPENSANDBOX_PYTHON_BASE_IMAGE=python:3.10-slim@sha256:<verified-digest>" >> /etc/jarvis/opensandbox.env'
   sudo sh -c 'printf "%s\n" "OPENSANDBOX_UV_IMAGE=ghcr.io/astral-sh/uv:<verified-version>@sha256:<verified-digest>" >> /etc/jarvis/opensandbox.env'
   sudo chown root:root /etc/jarvis/opensandbox.env
   sudo chmod 0600 /etc/jarvis/opensandbox.env
   sudo rm /etc/jarvis/opensandbox.key
   sudo install -o root -g root -m 0644 deploy/opensandbox/opensandbox.toml /etc/jarvis/opensandbox.toml
   ```

4. Build and start from a reviewed checkout:

   ```bash
   set -a; . /etc/jarvis/opensandbox.env; set +a
   export JARVIS_OPENSANDBOX_ENV_FILE=/etc/jarvis/opensandbox.env
   docker compose -f deploy/opensandbox/docker-compose.yml build --pull opensandbox
   docker compose -f deploy/opensandbox/docker-compose.yml up -d opensandbox
   curl --fail http://127.0.0.1:8090/health
   ```

   The `jarvis-opensandbox.service` runs `validate-opensandbox-env` before
   Docker Compose. It refuses an incorrectly owned/mode-restricted file, an
   empty/short control-plane key, or either mutable base-image reference; do not
   bypass that check. The key is intentionally unreadable by `jarvis-core`.

5. Verify before giving any orchestrator access:

   ```bash
   sudo ss -ltnp | rg '(:8090|:410[0-9]{2})'
   # 8090 and dynamically created 41000–41150 entries must show 127.0.0.1 only.
   docker inspect $(docker compose -f deploy/opensandbox/docker-compose.yml ps -q opensandbox) \
     --format '{{json .HostConfig.Binds}}'
   ```

The control-plane container alone receives `/var/run/docker.sock`; never mount
it into a workload. If any dynamic port listens on `0.0.0.0`, stop the service
and do not enable sandbox execution.

After creating one intentionally disposable test workload, run the root-only
runtime verifier from this checked-out repository:

```bash
sudo JARVIS_OPENSANDBOX_VERIFY_RUNTIME=1 \
  bash deploy/opensandbox/tests/verify-ubuntu-runtime.sh
```

It verifies the local listener/auth boundary, Core's separation from Docker,
and the Docker configuration of each running workload/egress sidecar. It fails
closed if it cannot prove an invariant and deliberately leaves the actual
in-workload network, cancellation, artifact and storage-quota probes as
recorded activation evidence.

## Remaining activation gate

The Rust provider can create a digest-pinned, profile-bound sandbox through this
authenticated control plane; its protocol only permits task inputs below
`/workspace/input`, bounded commands in `/workspace`, and explicitly requested
artifacts below `/workspace/artifacts`. Scoped-secret injection remains
fail-closed. Do not wire it into Core until a real Ubuntu test proves the patched
image and Kata runtime: private/LAN/localhost/metadata denial, no host mounts or
Docker socket in workload, bounded timeout/cancellation, artifact filtering and
cleanup. Chat must stay available if this service is stopped.
