#!/usr/bin/env bash
# Root-only CI regression: start the real page reader binary under the real
# jarvis-page-reader.service (only ExecStart and ExecPaths replaced). Proves
# that the hardened unit starts, that the socket is Core-group connectable
# while the reader is not in Core's group, that Core's uid gets a typed
# refusal for a non-public URL and that the peer uid check refuses other
# callers. Never fetches a page.
set -Eeuo pipefail
trap 'echo "test-page-reader-systemd: failed at line $LINENO" >&2
      ls -ld /run/jarvis-page-reader /run/jarvis-page-reader/reader.sock >&2 || true
      systemctl status --no-pager jarvis-page-reader.service >&2 || true
      journalctl --no-pager -n 30 -u jarvis-page-reader.service >&2 || true' ERR

[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || {
    echo "refusing to run outside GitHub Actions as root" >&2
    exit 1
}
binary=${JARVIS_PAGE_READER_BINARY:?set JARVIS_PAGE_READER_BINARY to the built reader}
[[ -x $binary ]]

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
unit=jarvis-page-reader.service
socket=/run/jarvis-page-reader/reader.sock
[[ -z $(systemctl list-unit-files --no-legend "$unit") && ! -e /run/jarvis-page-reader ]] || {
    echo "refusing to run on a host with a page reader install" >&2
    exit 1
}
# /tmp and /home are hidden from the unit, so the fixture lives below /run.
fixture=$(mktemp -d /run/jarvis-page-reader-ci.XXXXXX)
chmod 0755 "$fixture"
created=()
created_dirs=()
cleanup() {
    systemctl stop "$unit" 2>/dev/null || true
    rm -rf -- "/run/systemd/system/$unit" "$fixture" /run/jarvis-page-reader
    systemctl daemon-reload
    for user in "${created[@]}"; do userdel "$user" 2>/dev/null || true; done
    for ((i = ${#created_dirs[@]} - 1; i >= 0; i--)); do rmdir -- "${created_dirs[i]}" 2>/dev/null || true; done
}
trap cleanup EXIT

for user in jarvis jarvis-reader; do
    getent passwd "$user" >/dev/null && continue
    useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin "$user"
    created+=("$user")
done
# The required InaccessiblePaths= entries must exist for the unit to start.
for dir in /etc/jarvis /var/lib/jarvis; do
    [[ -e $dir ]] && continue
    install -d -m 0750 "$dir"
    created_dirs+=("$dir")
done

install -m 0755 "$binary" "$fixture/jarvis-page-reader"
sed -e "s#^ExecStart=/opt/jarvis/current/jarvis-page-reader\$#ExecStart=$fixture/jarvis-page-reader#" \
    -e "s#^ExecPaths=/opt/jarvis/releases #ExecPaths=$fixture #" \
    "$repo_dir/deploy/systemd/$unit" > "/run/systemd/system/$unit"
grep -Fxq "ExecStart=$fixture/jarvis-page-reader" "/run/systemd/system/$unit"
grep -Fxq "ExecPaths=$fixture /usr/lib/x86_64-linux-gnu -/usr/lib64" "/run/systemd/system/$unit"
# Nothing else in the unit differs from the packaged one.
[[ $(diff "$repo_dir/deploy/systemd/$unit" "/run/systemd/system/$unit" | grep -c '^>') == 2 ]]
systemctl daemon-reload
systemctl start "$unit"
for _ in $(seq 1 50); do
    [[ -S $socket ]] && break
    sleep 0.2
done
[[ -S $socket ]]

# The journal may lag; read it into a file (grep -q on a pipe can make the
# writer fail under pipefail) and retry briefly.
journal_has() {
    for _ in $(seq 1 25); do
        journalctl --no-pager -u "$unit" > "$fixture/journal" 2>&1 || true
        grep -Fq -- "$1" "$fixture/journal" && return 0
        sleep 0.2
    done
    return 1
}

# The hardening lets it start and stay up without restarts.
sleep 2
systemctl is-active --quiet "$unit"
[[ $(systemctl show -p NRestarts --value "$unit") == 0 ]]
journal_has 'page reader ready'
[[ $(stat -c '%U:%G:%a' /run/jarvis-page-reader) == jarvis-reader:jarvis:2750 ]]
[[ $(stat -c '%U:%G:%a' "$socket") == jarvis-reader:jarvis:660 ]]
# The reader is not a member of Core's group.
pid=$(systemctl show -p MainPID --value "$unit")
jarvis_gid=$(getent group jarvis | cut -d: -f3)
read -r -a reader_groups < <(awk '/^Groups:/ { $1 = ""; print }' "/proc/$pid/status")
for gid in "${reader_groups[@]}" "$(awk '/^Gid:/ { print $2 }' "/proc/$pid/status")"; do
    [[ $gid != "$jarvis_gid" ]]
done
# The address fence is in force.
[[ -n $(systemctl show -p IPAddressDeny --value "$unit") ]]

ask() {
    local url=$1
    shift
    setpriv "$@" python3 -c '
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(25)
s.connect(sys.argv[1])
data = b""
try:
    s.sendall(json.dumps({"v": 1, "url": sys.argv[2]}).encode() + b"\n")
    s.shutdown(socket.SHUT_WR)
    while chunk := s.recv(4096):
        data += chunk
except OSError:
    pass
sys.stdout.write(data.decode())
' "$socket" "$url"
}
core=(--reuid=jarvis --regid=jarvis --clear-groups)
# Core's uid gets typed refusals; nothing is fetched.
[[ $(ask https://127.0.0.1/ "${core[@]}") == '{"refused":{"code":"url_refused"}}' ]]
[[ $(ask http://example.com/ "${core[@]}") == '{"refused":{"code":"url_refused"}}' ]]
[[ $(ask https://example.com/logout "${core[@]}") == '{"refused":{"code":"action_path"}}' ]]
journal_has '"outcome":"url_refused"'
# Another uid that can open the socket through the group gets nothing.
[[ -z $(ask https://example.com/ --reuid=nobody --regid=jarvis --clear-groups) ]]
journal_has '"outcome":"peer_refused"'

systemctl stop "$unit"
[[ ! -e $socket ]]
echo 'Page reader systemd regression passed'
