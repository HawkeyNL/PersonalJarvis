#!/usr/bin/env bash
# Install one already-built, reviewed Jarvis Core release on the Ubuntu Home Node.
# Run as root from a trusted administrator session; this script never installs
# packages, creates secrets, opens firewall ports, or grants Docker/root access to
# the Jarvis service account.
set -euo pipefail

usage() {
    echo "Usage: sudo $0 /opt/jarvis/releases/vMAJOR.MINOR.PATCH" >&2
    exit 64
}

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
[[ $# -eq 1 ]] || usage

release_dir=$(realpath -e -- "$1")
case "$release_dir" in
    /opt/jarvis/releases/*) ;;
    *) echo "release must be beneath /opt/jarvis/releases" >&2; exit 1 ;;
esac
release_tag=${release_dir##*/}
[[ $release_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "release must be a verified stable tag directory" >&2
    exit 1
}

find "$release_dir" -xdev -type l -print -quit | grep -q . && {
    echo "release must not contain symlinks" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-api" ]] || {
    echo "missing executable: $release_dir/jarvis-api" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-agent-bundle" ]] || {
    echo "missing agent-bundle validator: $release_dir/jarvis-agent-bundle" >&2
    exit 1
}
[[ -f /etc/jarvis/Jarvis.md && ! -L /etc/jarvis/Jarvis.md ]] || {
    echo "missing protected persona: /etc/jarvis/Jarvis.md" >&2
    exit 1
}
[[ -f "$release_dir/release.json" && ! -L "$release_dir/release.json" ]] || {
    echo "missing verified release manifest" >&2
    exit 1
}
[[ -f "$release_dir/release.verification" && ! -L "$release_dir/release.verification" ]] || {
    echo "release was not staged by the verified-release helper" >&2
    exit 1
}
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
jq -e --arg tag "$release_tag" '
    .tag == $tag and
    (.revision | strings | test("^[0-9a-f]{40}$")) and
    (.schema_sha256 | strings | test("^[0-9a-f]{64}$"))
' "$release_dir/release.json" >/dev/null || {
    echo "release manifest is invalid or mismatched" >&2
    exit 1
}
[[ -f /etc/jarvis/core.env && ! -L /etc/jarvis/core.env ]] || {
    echo "missing /etc/jarvis/core.env; create it from deploy/systemd/README.md first" >&2
    exit 1
}

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
fi
install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis

[[ -f /etc/jarvis/surrealdb.env && ! -L /etc/jarvis/surrealdb.env ]] || {
    echo "missing root-only /etc/jarvis/surrealdb.env" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis/surrealdb.env) == root:root:600 ]] || {
    echo "/etc/jarvis/surrealdb.env must be root:root mode 0600" >&2
    exit 1
}

# Do not "repair" a surprising secret file. The generator creates exactly this
# ownership/mode; accepting a symlink or broad mode here would weaken Core.
[[ $(stat -c '%U:%G:%a' /etc/jarvis/core.env) == root:jarvis:640 ]] || {
    echo "/etc/jarvis/core.env must be root:jarvis mode 0640" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]] || {
    echo "/etc/jarvis/Jarvis.md must be root:jarvis mode 0640" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis) == root:jarvis:750 ]] || {
    echo "/etc/jarvis must be root:jarvis mode 0750 so Core can traverse protected inputs" >&2
    exit 1
}

grep -qx 'JARVIS_ENVIRONMENT=production' /etc/jarvis/core.env || {
    echo "JARVIS_ENVIRONMENT must be production" >&2
    exit 1
}
grep -Eq '^JARVIS_BIND_ADDR=(127\.0\.0\.1|\[::1\]):[0-9]+$' /etc/jarvis/core.env || {
    echo "production JARVIS_BIND_ADDR must bind only 127.0.0.1 or [::1]" >&2
    exit 1
}
grep -qx 'JARVIS_AGENT_ENABLED=false' /etc/jarvis/core.env || {
    echo "agent execution must remain disabled for the initial deployment" >&2
    exit 1
}
grep -qx 'JARVIS_AGENT_CLAUDE_CODE_ENABLED=false' /etc/jarvis/core.env || {
    echo "Claude Code execution must remain disabled for the initial deployment" >&2
    exit 1
}

for required in JARVIS_SURREAL_ENDPOINT JARVIS_SURREAL_NAMESPACE JARVIS_SURREAL_DATABASE JARVIS_SURREAL_USERNAME JARVIS_SURREAL_PASSWORD; do
    value=$(sed -n "s/^${required}=//p" /etc/jarvis/core.env | tail -n 1)
    [[ -n ${value:-} ]] || { echo "$required must be set" >&2; exit 1; }
done

hops=$(sed -n 's/^JARVIS_TRUSTED_PROXY_HOPS=//p' /etc/jarvis/core.env | tail -n 1)
ips=$(sed -n 's/^JARVIS_TRUSTED_PROXY_IPS=//p' /etc/jarvis/core.env | tail -n 1)
if [[ ${hops:-0} != 0 && -z ${ips:-} ]]; then
    echo "trusted proxy hops require JARVIS_TRUSTED_PROXY_IPS" >&2
    exit 1
fi
if [[ ${hops:-0} != 0 && ${hops:-0} != 1 ]]; then
    echo "the single local Caddy ingress requires JARVIS_TRUSTED_PROXY_HOPS=1" >&2
    exit 1
fi

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
install -o root -g root -m 0644 \
    "$repo_dir/deploy/systemd/jarvis-core.service" \
    /etc/systemd/system/jarvis-core.service
install -o root -g root -m 0644 \
    "$repo_dir/deploy/systemd/jarvis-surrealdb.service" \
    /etc/systemd/system/jarvis-surrealdb.service
install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
install -o root -g root -m 0755 \
    "$release_dir/jarvis-agent-bundle" \
    /usr/local/libexec/jarvis/jarvis-agent-bundle
install -o root -g root -m 0755 \
    "$repo_dir/deploy/systemd/update-core-release.sh" \
    /usr/local/libexec/jarvis/update-core-release
install -o root -g root -m 0755 \
    "$repo_dir/deploy/systemd/verify-home-node.sh" \
    /usr/local/libexec/jarvis/verify-home-node
install -o root -g root -m 0644 \
    "$repo_dir/deploy/systemd/jarvis-updater.service" \
    /etc/systemd/system/jarvis-updater.service
install -o root -g root -m 0644 \
    "$repo_dir/deploy/systemd/jarvis-updater.timer" \
    /etc/systemd/system/jarvis-updater.timer

# A release is immutable: the unprivileged service cannot modify its binary or
# Core persona even if an application-level control were bypassed.
chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"
ln -sfn "$release_dir" /opt/jarvis/current

systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/jarvis-core.service
systemd-analyze verify /etc/systemd/system/jarvis-surrealdb.service
systemd-analyze verify /etc/systemd/system/jarvis-updater.service
systemd-analyze verify /etc/systemd/system/jarvis-updater.timer
systemctl enable --now jarvis-surrealdb.service
systemctl enable --now jarvis-core

core_ready=false
for _attempt in $(seq 1 15); do
    if curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/livez >/dev/null \
        && curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/readyz >/dev/null; then
        core_ready=true
        break
    fi
    if systemctl is-failed --quiet jarvis-core.service; then
        break
    fi
    sleep 1
done
if [[ $core_ready != true ]]; then
    echo "Jarvis Core did not become ready; recent service diagnostics follow:" >&2
    systemctl --no-pager --full status jarvis-core.service || true
    journalctl --no-pager -u jarvis-core.service -n 80 || true
    exit 1
fi
systemctl --no-pager --full status jarvis-core

echo "Jarvis Core is running from $release_dir"
