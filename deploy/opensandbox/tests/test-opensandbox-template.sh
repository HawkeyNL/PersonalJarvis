#!/usr/bin/env bash
# Static invariants for the trusted OpenSandbox control plane. These checks do
# not claim to prove container isolation; the Ubuntu/Kata adversarial test is a
# separate activation gate in ADR-040.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
config="$repo_root/deploy/opensandbox/opensandbox.toml"
compose="$repo_root/deploy/opensandbox/docker-compose.yml"
patch="$repo_root/deploy/opensandbox/patches/0001-loopback-published-ports.patch"
egress_patch="$repo_root/deploy/opensandbox/patches/0002-egress-private-range-deny.patch"
sidecar_patch="$repo_root/deploy/opensandbox/patches/0003-egress-sidecar-hardening.patch"
sidecar_limits_patch="$repo_root/deploy/opensandbox/patches/0004-egress-sidecar-resource-limits.patch"
env_example="$repo_root/deploy/opensandbox/opensandbox.env.example"
unit="$repo_root/deploy/systemd/jarvis-opensandbox.service"
env_validator="$repo_root/deploy/opensandbox/validate-opensandbox-env.sh"
ci="$repo_root/.github/workflows/ci.yml"
release_ci="$repo_root/.github/workflows/release.yml"

require() {
  local pattern=$1 file=$2
  if ! grep -Fqx -- "$pattern" "$file"; then
    echo "missing required OpenSandbox invariant in $file: $pattern" >&2
    exit 1
  fi
}

forbid() {
  local pattern=$1 file=$2
  if grep -Fq -- "$pattern" "$file"; then
    echo "forbidden OpenSandbox setting in $file: $pattern" >&2
    exit 1
  fi
}

require 'host = "127.0.0.1"' "$config"
require 'port = 8090' "$config"
require 'network_mode = "bridge"' "$config"
require 'publish_host = "127.0.0.1"' "$config"
require 'sandbox_binds = []' "$config"
require 'mode = "dns+nft"' "$config"
require 'disable_ipv6 = true' "$config"
require 'no_new_privileges = true' "$config"
forbid 'host = "0.0.0.0"' "$config"
forbid '/var/run/docker.sock:/var/run/docker.sock' "$config"

# The trusted control plane may use host networking only to reach its own
# loopback-published workload proxies. The workload configuration itself stays
# bridge-only and receives no host binds/socket.
require '    network_mode: host' "$compose"
require '      - /var/run/docker.sock:/var/run/docker.sock' "$compose"
forbid '    ports:' "$compose"

# The local patch makes loopback the default and removes the upstream
# single-tenant proxy authentication bypass.
require '+        default="127.0.0.1",' "$patch"
require '+DEFAULT_DOCKER_PUBLISH_HOST = "127.0.0.1"' "$patch"
require '-        if self._is_proxy_path(request.url.path) and not self._is_multi_tenant:' "$patch"
require '+func isForbiddenEgressAddr(addr netip.Addr) bool {' "$egress_patch"
require '+func TestExtractResolvedIPs_RejectsPrivateAndSpecialUseDestinations(t *testing.T) {' "$egress_patch"
require '     patches/0004-egress-sidecar-resource-limits.patch /tmp/' "$repo_root/deploy/opensandbox/Dockerfile"
require '+            base_sidecar_host_config_kwargs["security_opt"] = ["no-new-privileges:true"]' "$sidecar_patch"
require '+            base_sidecar_host_config_kwargs["pids_limit"] = docker_cfg.pids_limit' "$sidecar_patch"
require '+            "mem_limit": 128 * 1024 * 1024,' "$sidecar_limits_patch"
require '+            "nano_cpus": 250_000_000,' "$sidecar_limits_patch"
require 'OPENSANDBOX_SERVER_API_KEY=' "$env_example"
require 'OPENSANDBOX_PYTHON_BASE_IMAGE=' "$env_example"
require 'OPENSANDBOX_UV_IMAGE=' "$env_example"
require 'ARG PYTHON_BASE_IMAGE' "$repo_root/deploy/opensandbox/Dockerfile"
require 'ARG UV_IMAGE' "$repo_root/deploy/opensandbox/Dockerfile"
require 'COPY --from=uv /uv /uvx /bin/' "$repo_root/deploy/opensandbox/Dockerfile"
require '        PYTHON_BASE_IMAGE: ${OPENSANDBOX_PYTHON_BASE_IMAGE:?set OPENSANDBOX_PYTHON_BASE_IMAGE to a verified python:3.10-slim digest}' "$compose"
require '        UV_IMAGE: ${OPENSANDBOX_UV_IMAGE:?set OPENSANDBOX_UV_IMAGE to a verified ghcr.io/astral-sh/uv digest}' "$compose"
forbid 'astral.sh/uv/install.sh' "$repo_root/deploy/opensandbox/Dockerfile"
if grep -Eq '^OPENSANDBOX_SERVER_API_KEY=.+$' "$env_example"; then
  echo "the checked-in OpenSandbox environment example must not contain a key" >&2
  exit 1
fi
if grep -Eq '^OPENSANDBOX_PYTHON_BASE_IMAGE=.+$' "$env_example"; then
  echo "the checked-in OpenSandbox environment example must not contain an image digest" >&2
  exit 1
fi
if grep -Eq '^OPENSANDBOX_UV_IMAGE=.+$' "$env_example"; then
  echo "the checked-in OpenSandbox environment example must not contain a uv image digest" >&2
  exit 1
fi

require 'ConditionPathExists=/etc/jarvis/opensandbox.env' "$unit"
require 'Environment=JARVIS_OPENSANDBOX_ENV_FILE=/etc/jarvis/opensandbox.env' "$unit"
require 'EnvironmentFile=/etc/jarvis/opensandbox.env' "$unit"
require 'NoNewPrivileges=true' "$unit"
require 'ExecStartPre=/usr/local/libexec/jarvis/validate-opensandbox-env' "$unit"
if ! grep -Fq 'OPENSANDBOX_PYTHON_BASE_IMAGE' "$env_validator" || \
   ! grep -Fq 'OPENSANDBOX_UV_IMAGE' "$env_validator"; then
  echo "OpenSandbox environment validator must require immutable base images" >&2
  exit 1
fi
require '[[ $(stat -c '\''%U:%G'\'' -- "$env_file") == root:root ]] || \' "$env_validator"
require '[[ $(stat -c '\''%a'\'' -- "$env_file") == 600 ]] || \' "$env_validator"
if ! bash -n "$env_validator"; then
  echo "OpenSandbox environment validator has invalid shell syntax" >&2
  exit 1
fi

# The source patch has a regression test against the exact upstream revision.
# Static template checks alone are insufficient because an upstream source move
# could otherwise make a patch apply differently or fail only during deploy.
for workflow in "$ci" "$release_ci"; do
  require '          go-version: "1.25.0"' "$workflow"
  require '          OPENSANDBOX_COMMIT: 6b2023e9b7eb80a940d88e6ae05fcbc0eb0cf23f' "$workflow"
  require '          git -C "$sandbox_source" apply deploy/opensandbox/patches/0003-egress-sidecar-hardening.patch' "$workflow"
  require '          git -C "$sandbox_source" apply deploy/opensandbox/patches/0004-egress-sidecar-resource-limits.patch' "$workflow"
  require '          (cd "$sandbox_source/components/egress" && go test ./pkg/dnsproxy)' "$workflow"
done

echo "OpenSandbox template safety checks passed"
