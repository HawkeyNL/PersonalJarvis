#!/usr/bin/env bash
# Root-only CI regression: start the real jarvis-core.service under systemd
# (only ExecStart, WorkingDirectory and EnvironmentFile replaced) and require
# /run/jarvis-core to stay jarvis:caddy 2750 across start, reload and restart.
# systemd resets RuntimeDirectory= owner and mode before every Exec* command,
# which a grep of the unit text cannot see.
set -Eeuo pipefail
trap 'echo "test-core-socket-dir-systemd: failed at line $LINENO" >&2
      ls -ld /run/jarvis-core /run/jarvis-core/api.sock >&2 || true
      systemctl status --no-pager jarvis-core.service >&2 || true' ERR

[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || {
    echo "refusing to run outside GitHub Actions as root" >&2
    exit 1
}

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
unit=jarvis-core.service
[[ -z $(systemctl list-unit-files --no-legend "$unit") && ! -e /run/jarvis-core ]] || {
    echo "refusing to run on a host with a Core install" >&2
    exit 1
}
# /tmp and /home are hidden from the unit, so the probe lives below /run.
fixture=$(mktemp -d /run/jarvis-core-ci.XXXXXX)
chmod 0755 "$fixture"
created=()
created_dirs=()
cleanup() {
    systemctl stop "$unit" 2>/dev/null || true
    rm -rf -- "/run/systemd/system/$unit" "/run/systemd/system/$unit.d" "$fixture" /run/jarvis-core
    systemctl daemon-reload
    for user in "${created[@]}"; do userdel "$user" 2>/dev/null || true; done
    for ((i = ${#created_dirs[@]} - 1; i >= 0; i--)); do rmdir -- "${created_dirs[i]}" 2>/dev/null || true; done
}
trap cleanup EXIT

for user in jarvis caddy; do
    getent passwd "$user" >/dev/null && continue
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin "$user"
    created+=("$user")
done
# The unit's ReadWritePaths= and ReadOnlyPaths= entries must exist to start it.
for dir in /var/lib/jarvis /var/lib/jarvis/agents; do
    [[ -e $dir ]] && continue
    install -d -m 0755 "$dir"
    created_dirs+=("$dir")
done

# Stand-in for jarvis-api: replace a stale socket and listen, as bind_api_socket does.
cat > "$fixture/probe.py" <<'PY'
import os, signal, socket
signal.signal(signal.SIGHUP, signal.SIG_IGN)
path = "/run/jarvis-core/api.sock"
if os.path.lexists(path):
    os.unlink(path)
server = socket.socket(socket.AF_UNIX)
server.bind(path)
os.chmod(path, 0o660)
server.listen()
while True:
    server.accept()[0].close()
PY
install -m 0644 "$repo_dir/deploy/systemd/$unit" "/run/systemd/system/$unit"
install -d "/run/systemd/system/$unit.d"
printf '[Service]\nEnvironmentFile=\nWorkingDirectory=/\nExecStart=\nExecStart=/usr/bin/python3 -I %s/probe.py\n' \
    "$fixture" > "/run/systemd/system/$unit.d/ci.conf"

caddy_connects() {
    runuser -u caddy -- python3 -c 'import socket; socket.socket(socket.AF_UNIX).connect("/run/jarvis-core/api.sock")'
}
check() {
    for _ in $(seq 1 20); do
        caddy_connects 2>/dev/null && break
        sleep 0.5
    done
    systemctl is-active --quiet "$unit"
    [[ $(stat -c %U:%G:%a /run/jarvis-core) == jarvis:caddy:2750 ]]
    [[ $(stat -c %U:%G:%a /run/jarvis-core/api.sock) == jarvis:caddy:660 ]]
    [[ $(stat -c %U:%G:%a /run/jarvis-core-admin) == jarvis:jarvis:700 ]]
    caddy_connects
}

systemctl daemon-reload
systemctl start "$unit"
check
# ExecReload must not reset the directory either.
systemctl reload "$unit"
check
# As left by the old RuntimeDirectory= unit; install -d repairs it.
chmod 0700 /run/jarvis-core
systemctl restart "$unit"
check

echo "Core API socket directory survives start, reload and restart under systemd"
