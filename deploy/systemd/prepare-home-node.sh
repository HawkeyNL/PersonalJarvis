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

if ! getent passwd jarvis-codex >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis-codex --shell /usr/sbin/nologin jarvis-codex
fi
id -nG jarvis-codex | tr ' ' '\n' | grep -qx docker && fail "jarvis-codex must not be a Docker-group member"
if ! getent passwd jarvis-claude >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis-claude --shell /usr/sbin/nologin jarvis-claude
fi
id -nG jarvis-claude | tr ' ' '\n' | grep -qx docker && fail "jarvis-claude must not be a Docker-group member"
for worker in jarvis-claude jarvis-codex; do
    worker_state="/var/lib/$worker"
    if [[ -e $worker_state || -L $worker_state ]]; then
        [[ -d $worker_state && ! -L $worker_state &&
           $(stat -c '%U:%G:%a' "$worker_state") == "$worker:$worker:700" ]] ||
            fail "unsafe $worker subscription state directory"
    else
        install -d -o "$worker" -g "$worker" -m 0700 "$worker_state"
    fi
done

install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis
# The config broker's persistent and ephemeral state are intentionally created
# by its systemd StateDirectory=/RuntimeDirectory= lifecycle.  Pre-creating
# /run would be lost at reboot and fails ProtectSystem namespace setup.
install -d -o root -g root -m 0700 /var/lib/jarvis/surrealdb
install -d -o root -g root -m 0755 /opt/jarvis /opt/jarvis/releases
# The service needs directory traversal to read only its explicitly group-readable
# inputs.  Individual secrets below this directory remain root:root 0600.
install -d -o root -g jarvis -m 0750 /etc/jarvis
install -d -o root -g jarvis -m 0750 /etc/jarvis/secrets
if [[ ! -e /etc/jarvis/pricing-registry.json ]]; then
    install -o root -g jarvis -m 0640 \
        "$repo_dir/deploy/systemd/pricing-overrides.empty.json" \
        /etc/jarvis/pricing-registry.json
fi
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
for helper in generate-core-env.sh stage-core-release.sh verify-home-node.sh jarvis-models.sh jarvis-admin.sh; do
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
# jarvis-backup ships inside each verified Core release; the command always
# runs the active release's copy instead of a checkout snapshot. This path is
# deliberately a symlink: never add it to the updater's tooling_targets, which
# refuse symlinked targets.
ln -sfn /opt/jarvis/current/jarvis-backup /usr/local/sbin/jarvis-backup
# Owner backup config and recipient public keys; see docs/BACKUP_AND_RESTORE.md.
install -d -o root -g root -m 0700 /etc/jarvis-backup
# `jarvis-admin.sh` is retained only as an internal migration reference.  The
# canonical `sudo jarvis` binary is installed from a verified release by
# install-home-node-core.sh; bootstrap deliberately does not publish a
# checkout-owned root CLI at that path.
for helper in install-private-config.sh install-agent-bundle.sh; do
    install -o root -g root -m 0755 \
        "$repo_dir/deploy/private/$helper" \
        "/usr/local/libexec/jarvis/${helper%.sh}"
done
install -o root -g root -m 0755 \
    "$repo_dir/deploy/private/jarvis-private-agent-poll.sh" \
    /usr/local/libexec/jarvis/private-agent-poll
install -o root -g root -m 0755 \
    "$repo_dir/deploy/private/jarvis-private-update.sh" \
    /usr/local/sbin/jarvis-private-update

echo "Home Node preparation: Jarvis-owned directories and unprivileged service identity are ready."
