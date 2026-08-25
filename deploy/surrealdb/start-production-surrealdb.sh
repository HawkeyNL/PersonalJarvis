#!/usr/bin/env bash
# Start the production-only SurrealDB compose project after validating that the
# root credential and immutable image reference have not been weakened.
set -euo pipefail

readonly env_file=/etc/jarvis/surrealdb.env
readonly image_pattern='^surrealdb/surrealdb@sha256:[0-9a-f]{64}$'
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
compose_file=${JARVIS_SURREALDB_COMPOSE_FILE:-/opt/jarvis/surrealdb/docker-compose.yml}
[[ -f $compose_file ]] || compose_file="$repo_dir/deploy/surrealdb/docker-compose.yml"

fail() { echo "SurrealDB start: $*" >&2; exit 1; }
[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ -f $env_file && ! -L $env_file ]] || fail "missing $env_file; run initialize-production-surrealdb.sh first"
[[ $(stat -c '%U:%G:%a' "$env_file") == root:root:600 ]] || fail "$env_file must be root:root mode 0600"
[[ -f $compose_file ]] || fail "production compose manifest is missing"
command -v docker >/dev/null 2>&1 || fail "Docker is required"

# shellcheck disable=SC1090
source "$env_file"
[[ ${SURREALDB_IMAGE:-} =~ $image_pattern ]] || fail "SURREALDB_IMAGE must be a digest-pinned official image"
[[ -n ${SURREAL_ROOT_USER:-} && -n ${SURREAL_ROOT_PASSWORD:-} ]] || fail "root credentials are incomplete"

install -d -o root -g root -m 0700 /var/lib/jarvis/surrealdb
docker compose --env-file "$env_file" -f "$compose_file" up -d --wait
