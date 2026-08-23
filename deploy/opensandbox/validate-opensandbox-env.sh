#!/usr/bin/env bash
# Validate the root-managed OpenSandbox build/runtime environment before the
# trusted Docker control plane starts. This intentionally prints no values.
set -euo pipefail

fail() {
  echo "invalid OpenSandbox environment: $*" >&2
  exit 1
}

env_file=${JARVIS_OPENSANDBOX_ENV_FILE:-/etc/jarvis/opensandbox.env}
[[ $env_file == /etc/jarvis/opensandbox.env ]] || fail "unexpected environment file path"
[[ -r $env_file ]] || fail "environment file is not readable"
[[ $(stat -c '%U:%G' -- "$env_file") == root:root ]] || \
  fail "environment file must be owned root:root"
[[ $(stat -c '%a' -- "$env_file") == 600 ]] || \
  fail "environment file must have mode 0600"
# The file is created by the root-only deployment procedure. Do not echo or
# otherwise log it: it contains the control-plane API key.
set -a
. "$env_file"
set +a

[[ -n ${OPENSANDBOX_SERVER_API_KEY:-} ]] || fail "missing API key"
[[ ${#OPENSANDBOX_SERVER_API_KEY} -ge 32 ]] || fail "API key is too short"

is_digest_reference() {
  [[ $1 =~ ^[^[:space:]@]+@sha256:[a-f0-9]{64}$ ]]
}

is_digest_reference "${OPENSANDBOX_PYTHON_BASE_IMAGE:-}" || \
  fail "Python base image must be an immutable sha256 reference"
is_digest_reference "${OPENSANDBOX_UV_IMAGE:-}" || \
  fail "uv image must be an immutable sha256 reference"
