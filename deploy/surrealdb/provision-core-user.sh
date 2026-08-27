#!/usr/bin/env bash
# Create the least-privileged database account consumed by Jarvis Core. A
# root-owned marker makes normal reruns idempotent and refuses silent rotation.
set -euo pipefail

readonly env_file=/etc/jarvis/surrealdb.env
readonly marker=/etc/jarvis/surrealdb-core-provisioned
readonly default_namespace=jarvis
readonly default_database=core
readonly default_user=core
namespace=$default_namespace
database=$default_database
username=$default_user
password_file=

usage() {
    cat >&2 <<'EOF'
Usage: sudo provision-core-user.sh [--namespace NAME] [--database NAME] [--username NAME] --password-file /run/jarvis-core-db-password
EOF
    exit 64
}
fail() { echo "SurrealDB provisioning: $*" >&2; exit 1; }

while (($#)); do
    case "$1" in
        --namespace) namespace=${2:-}; shift 2 ;;
        --database) database=${2:-}; shift 2 ;;
        --username) username=${2:-}; shift 2 ;;
        --password-file) password_file=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done

[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ $namespace =~ ^[A-Za-z][A-Za-z0-9_]{0,63}$ ]] || fail "invalid namespace"
[[ $database =~ ^[A-Za-z][A-Za-z0-9_]{0,63}$ ]] || fail "invalid database"
[[ $username =~ ^[A-Za-z][A-Za-z0-9_]{0,63}$ ]] || fail "invalid username"
[[ $password_file == /run/* && $password_file != */../* ]] || fail "password file must be an absolute path below /run"
[[ ! -e $password_file ]] || fail "password file already exists; refusing to overwrite a secret"
[[ -f $env_file && ! -L $env_file ]] || fail "missing root-only $env_file"
[[ $(stat -c '%U:%G:%a' "$env_file") == root:root:600 ]] || fail "$env_file must be root:root mode 0600"
command -v docker >/dev/null 2>&1 || fail "Docker is required"
command -v openssl >/dev/null 2>&1 || fail "openssl is required"
# shellcheck disable=SC1090
source "$env_file"
[[ ${SURREALDB_IMAGE:-} =~ ^surrealdb/surrealdb@sha256:[0-9a-f]{64}$ ]] || \
    fail "SURREALDB_IMAGE must be a digest-pinned official image"
[[ -n ${SURREAL_ROOT_PASSWORD:-} ]] || fail "root password is missing"

if [[ -e $marker ]]; then
    [[ ! -L $marker && $(stat -c '%U:%G:%a' "$marker") == root:root:600 ]] || \
        fail "unsafe provisioning marker"
    echo "SurrealDB provisioning: existing Core user retained; no credentials rotated."
    exit 0
fi

core_password=$(openssl rand -base64 48)
[[ $core_password =~ ^[A-Za-z0-9+/=]+$ ]] || fail "generated Core password has an unsafe format"
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
compose_file=${JARVIS_SURREALDB_COMPOSE_FILE:-/opt/jarvis/surrealdb/docker-compose.yml}
[[ -f $compose_file ]] || compose_file="$repo_dir/deploy/surrealdb/docker-compose.yml"

# The root credential is inherited from the root-managed container environment;
# it is not sent with docker exec and therefore never appears in host argv. The
# generated base64 password contains no quote characters, so it is safe in this
# fixed SQL literal. Feed the statement through stdin directly to the official,
# shell-less image's `/surreal sql` binary. This creates exactly the
# database-scoped EDITOR account required by current Core.
printf "DEFINE USER %s ON DATABASE PASSWORD '%s' ROLES EDITOR;\\n" "$username" "$core_password" \
    | docker compose --env-file "$env_file" -f "$compose_file" exec -T surrealdb \
        /surreal sql --hide-welcome --endpoint ws://127.0.0.1:8000 \
            --auth-level root --namespace "$namespace" --database "$database" >/dev/null

umask 077
printf '%s' "$core_password" > "$password_file"
chown root:root "$password_file"
chmod 0600 "$password_file"
printf 'namespace=%s\ndatabase=%s\nusername=%s\n' "$namespace" "$database" "$username" > "$marker"
chown root:root "$marker"
chmod 0600 "$marker"
echo "SurrealDB provisioning: Core account created; pass the one-time password file to generate-core-env.sh."
