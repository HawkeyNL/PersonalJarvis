#!/usr/bin/env bash
# Install one already-built, reviewed Jarvis Core release on the Ubuntu Home Node.
# Run as root from a trusted administrator session; this script never installs
# packages, creates secrets, opens firewall ports, or grants Docker/root access to
# the Jarvis service account.
set -euo pipefail

usage() {
    echo "Usage: sudo $0 /opt/jarvis/releases/<reviewed-commit>" >&2
    exit 64
}

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
[[ $# -eq 1 ]] || usage

release_dir=$(realpath -e -- "$1")
case "$release_dir" in
    /opt/jarvis/releases/*) ;;
    *) echo "release must be beneath /opt/jarvis/releases" >&2; exit 1 ;;
esac

[[ -x "$release_dir/jarvis-api" ]] || {
    echo "missing executable: $release_dir/jarvis-api" >&2
    exit 1
}
[[ -f "$release_dir/core/Jarvis.md" ]] || {
    echo "missing Core persona: $release_dir/core/Jarvis.md" >&2
    exit 1
}
[[ -f /etc/jarvis/core.env ]] || {
    echo "missing /etc/jarvis/core.env; create it from deploy/systemd/README.md first" >&2
    exit 1
}

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
fi
install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis

# The service must be able to read the environment file but nothing else may.
chown root:jarvis /etc/jarvis/core.env
chmod 0640 /etc/jarvis/core.env

grep -qx 'JARVIS_ENVIRONMENT=production' /etc/jarvis/core.env || {
    echo "JARVIS_ENVIRONMENT must be production" >&2
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

hops=$(sed -n 's/^JARVIS_TRUSTED_PROXY_HOPS=//p' /etc/jarvis/core.env | tail -n 1)
ips=$(sed -n 's/^JARVIS_TRUSTED_PROXY_IPS=//p' /etc/jarvis/core.env | tail -n 1)
if [[ ${hops:-0} != 0 && -z ${ips:-} ]]; then
    echo "trusted proxy hops require JARVIS_TRUSTED_PROXY_IPS" >&2
    exit 1
fi

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
install -o root -g root -m 0644 \
    "$repo_dir/deploy/systemd/jarvis-core.service" \
    /etc/systemd/system/jarvis-core.service

# A release is immutable: the unprivileged service cannot modify its binary or
# Core persona even if an application-level control were bypassed.
chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"
ln -sfn "$release_dir" /opt/jarvis/current

systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/jarvis-core.service
systemctl enable --now jarvis-core

curl --fail --silent --show-error http://127.0.0.1:8080/livez >/dev/null
curl --fail --silent --show-error http://127.0.0.1:8080/readyz >/dev/null
systemctl --no-pager --full status jarvis-core

echo "Jarvis Core is running from $release_dir"
