#!/usr/bin/env bash
# Create the root-only runtime configuration for the production SurrealDB
# container. This script is intentionally interactive: root credentials must
# never be supplied on a command line or copied into shell history.
set -euo pipefail

readonly config_dir=/etc/jarvis
readonly env_file="$config_dir/surrealdb.env"
readonly pinned_image='surrealdb/surrealdb@sha256:d653f6c8a89e81f865ee31cd2f587c50f50ace922175e04150b1e385d2f86011'

fail() { echo "SurrealDB bootstrap: $*" >&2; exit 1; }

[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ -t 0 && -t 1 ]] || fail "requires an interactive terminal so secrets are not redirected into logs"
command -v openssl >/dev/null 2>&1 || fail "openssl is required"

if [[ -e $env_file ]]; then
    [[ ! -L $env_file ]] || fail "$env_file must not be a symlink"
    [[ $(stat -c '%U:%G:%a' "$env_file") == root:root:600 ]] || \
        fail "$env_file must be root:root mode 0600"
    echo "SurrealDB bootstrap: existing $env_file retained; no credentials rotated."
    exit 0
fi

install -d -o root -g root -m 0750 "$config_dir"
root_password=$(openssl rand -base64 48)
tmp=$(mktemp "$config_dir/.surrealdb.env.XXXXXX")
trap 'rm -f -- "$tmp"' EXIT
umask 077
{
    printf '%s\n' '# Root credential: container startup and root-operated provisioning only.'
    printf '%s\n' "SURREALDB_IMAGE=$pinned_image"
    printf '%s\n' 'SURREAL_ROOT_USER=root'
    printf '%s\n' "SURREAL_ROOT_PASSWORD=$root_password"
} > "$tmp"
chown root:root "$tmp"
chmod 0600 "$tmp"
mv -f -- "$tmp" "$env_file"
trap - EXIT

echo "SurrealDB bootstrap: created $env_file (root-only). The root password was generated and was not displayed."
