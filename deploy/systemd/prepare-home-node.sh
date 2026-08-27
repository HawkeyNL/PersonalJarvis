#!/usr/bin/env bash
# Idempotently prepare only Jarvis-owned identities and filesystem locations.
# This script intentionally does not install packages, alter firewall/router/SSH
# configuration, add Docker privileges, or enable optional execution services.
set -euo pipefail

fail() { echo "Home Node preparation: $*" >&2; exit 1; }
[[ ${EUID} -eq 0 ]] || fail "must run as root"
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
fi
id -nG jarvis | tr ' ' '\n' | grep -qx docker && fail "jarvis must not be a Docker-group member"

install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis
install -d -o root -g root -m 0700 /var/lib/jarvis/surrealdb
install -d -o root -g root -m 0755 /opt/jarvis /opt/jarvis/releases
# The service needs directory traversal to read only its explicitly group-readable
# inputs.  Individual secrets below this directory remain root:root 0600.
install -d -o root -g jarvis -m 0750 /etc/jarvis
install -d -o root -g jarvis -m 0750 /etc/jarvis/secrets
install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
install -o root -g root -m 0644 "$repo_dir/deploy/lib/ui.sh" /usr/local/libexec/jarvis/ui.sh

install -d -o root -g root -m 0755 /opt/jarvis/surrealdb
install -o root -g root -m 0644 \
    "$repo_dir/deploy/surrealdb/docker-compose.yml" \
    /opt/jarvis/surrealdb/docker-compose.yml
for helper in initialize-production-surrealdb.sh start-production-surrealdb.sh provision-core-user.sh; do
    install -o root -g root -m 0755 \
        "$repo_dir/deploy/surrealdb/$helper" \
        "/usr/local/libexec/jarvis/${helper%.sh}"
done
for helper in generate-core-env.sh stage-core-release.sh verify-home-node.sh jarvis-models.sh; do
    [[ -f "$repo_dir/deploy/systemd/$helper" ]] || continue
    install -o root -g root -m 0755 \
        "$repo_dir/deploy/systemd/$helper" \
        "/usr/local/libexec/jarvis/${helper%.sh}"
done
install -o root -g root -m 0755 \
    "$repo_dir/deploy/systemd/jarvis-models.sh" \
    /usr/local/sbin/jarvis-models
install -o root -g root -m 0755 \
    "$repo_dir/deploy/systemd/jarvis-credentials.sh" \
    /usr/local/sbin/jarvis-credentials
for helper in install-private-config.sh install-agent-bundle.sh; do
    install -o root -g root -m 0755 \
        "$repo_dir/deploy/private/$helper" \
        "/usr/local/libexec/jarvis/${helper%.sh}"
done
install -o root -g root -m 0755 \
    "$repo_dir/deploy/private/jarvis-private-update.sh" \
    /usr/local/sbin/jarvis-private-update

echo "Home Node preparation: Jarvis-owned directories and unprivileged service identity are ready."
