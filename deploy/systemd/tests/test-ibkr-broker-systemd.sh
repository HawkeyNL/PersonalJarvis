#!/usr/bin/env bash
# Root-only CI regression: start the real broker binary under the real
# jarvis-ibkr-broker.service (only ExecStart and the credential source
# directory replaced) with disposable test-only credentials. Proves that
# LoadCredential= files pass the broker's permission check, that the socket is
# Core-group connectable while the broker is not in Core's group, and that the
# peer uid check refuses other callers. Never contacts IBKR.
set -Eeuo pipefail
trap 'echo "test-ibkr-broker-systemd: failed at line $LINENO" >&2
      ls -ld /run/jarvis-ibkr-broker /run/jarvis-ibkr-broker/broker.sock >&2 || true
      systemctl status --no-pager jarvis-ibkr-broker.service >&2 || true
      journalctl --no-pager -n 30 -u jarvis-ibkr-broker.service >&2 || true' ERR

[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || {
    echo "refusing to run outside GitHub Actions as root" >&2
    exit 1
}
binary=${JARVIS_IBKR_BROKER_BINARY:?set JARVIS_IBKR_BROKER_BINARY to the built broker}
[[ -x $binary ]]

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
unit=jarvis-ibkr-broker.service
socket=/run/jarvis-ibkr-broker/broker.sock
[[ -z $(systemctl list-unit-files --no-legend "$unit") && ! -e /run/jarvis-ibkr-broker ]] || {
    echo "refusing to run on a host with a broker install" >&2
    exit 1
}
# /tmp and /home are hidden from the unit, so the fixture lives below /run.
fixture=$(mktemp -d /run/jarvis-ibkr-ci.XXXXXX)
chmod 0755 "$fixture"
created=()
created_dirs=()
cleanup() {
    systemctl stop "$unit" 2>/dev/null || true
    rm -rf -- "/run/systemd/system/$unit" "$fixture" /run/jarvis-ibkr-broker
    systemctl daemon-reload
    for user in "${created[@]}"; do userdel "$user" 2>/dev/null || true; done
    for ((i = ${#created_dirs[@]} - 1; i >= 0; i--)); do rmdir -- "${created_dirs[i]}" 2>/dev/null || true; done
}
trap cleanup EXIT

for user in jarvis jarvis-ibkr; do
    getent passwd "$user" >/dev/null && continue
    useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin "$user"
    created+=("$user")
done
# InaccessiblePaths= entries must exist for the unit to start.
for dir in /etc/jarvis /var/lib/jarvis; do
    [[ -e $dir ]] && continue
    install -d -m 0750 "$dir"
    created_dirs+=("$dir")
done

# Disposable credentials in the format the owner's files use. The values are
# syntactically valid but unknown to IBKR, and no request is ever sent.
creds=$fixture/creds
install -d -m 0700 "$creds"
(
    umask 077
    printf 'TESTCONS\n' > "$creds/consumer_key"
    printf '6f531f8fd316915af53f\n' > "$creds/access_token"
    printf '74657374\n' > "$creds/access_token_secret"
    openssl genrsa -out "$creds/private_signature.pem" 2048 2>/dev/null
    openssl genpkey -genparam -algorithm DH -pkeyopt group:modp_2048 -out "$creds/dhparam.pem"
    openssl genrsa -out "$creds/private_encryption.pem" 2048 2>/dev/null
)
install -m 0755 "$binary" "$fixture/jarvis-ibkr-broker"
sed -e "s#^ExecStart=/opt/jarvis/current/jarvis-ibkr-broker\$#ExecStart=$fixture/jarvis-ibkr-broker#" \
    -e "s#/etc/jarvis/secrets/ibkr-oauth/#$creds/#g" \
    -e "s#^ExecPaths=/opt/jarvis/releases #ExecPaths=$fixture #" \
    "$repo_dir/deploy/systemd/$unit" > "/run/systemd/system/$unit"
grep -Fxq "ExecStart=$fixture/jarvis-ibkr-broker" "/run/systemd/system/$unit"
grep -Fxq "ExecPaths=$fixture /usr/lib/x86_64-linux-gnu -/usr/lib64" "/run/systemd/system/$unit"
[[ $(grep -c "^LoadCredential=[a-z_]*:$creds/" "/run/systemd/system/$unit") == 5 ]]
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

# The credentials passed the broker's checks: it stays up without restarts.
sleep 2
systemctl is-active --quiet "$unit"
[[ $(systemctl show -p NRestarts --value "$unit") == 0 ]]
journal_has 'IBKR read broker ready'
[[ $(stat -c '%U:%G:%a' /run/jarvis-ibkr-broker) == jarvis-ibkr:jarvis:2750 ]]
[[ $(stat -c '%U:%G:%a' "$socket") == jarvis-ibkr:jarvis:660 ]]

# Exactly the five fixed credentials; never the encryption key.
pid=$(systemctl show -p MainPID --value "$unit")
[[ $(ls "/run/credentials/$unit" | LC_ALL=C sort | tr '\n' ' ') == 'access_token access_token_secret consumer_key dh_param signature_key ' ]]
# The broker is not a member of Core's group.
jarvis_gid=$(getent group jarvis | cut -d: -f3)
read -r -a broker_groups < <(awk '/^Groups:/ { $1 = ""; print }' "/proc/$pid/status")
for gid in "${broker_groups[@]}" "$(awk '/^Gid:/ { print $2 }' "/proc/$pid/status")"; do
    [[ $gid != "$jarvis_gid" ]]
done

ask() {
    setpriv "$@" python3 -c '
import socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(sys.argv[1])
data = b""
try:
    s.sendall(b"{\"kind\":\"place_order\"}\n")
    s.shutdown(socket.SHUT_WR)
    while chunk := s.recv(4096):
        data += chunk
except OSError:
    pass
sys.stdout.write(data.decode())
' "$socket"
}
# Core's uid gets a typed refusal of the non-allowlisted request.
[[ $(ask --reuid=jarvis --regid=jarvis --clear-groups) == '{"error":{"code":"invalid_request"}}' ]]
# Another uid that can open the socket through the group gets nothing.
[[ -z $(ask --reuid=nobody --regid=jarvis --clear-groups) ]]
journal_has '"outcome":"peer_refused"'

systemctl stop "$unit"
[[ ! -e $socket ]]
echo 'IBKR broker systemd regression passed'
