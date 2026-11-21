#!/usr/bin/env bash
# Disposable release-format fixture for the page reader; no network or host
# services.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
service="$repo/deploy/systemd/jarvis-page-reader.service"

# Own identity and group, a setgid socket directory in Core's group, no
# credentials, no state, public internet only and no IP listener.
grep -Fxq 'User=jarvis-reader' "$service"
grep -Fxq 'Group=jarvis-reader' "$service"
grep -Fxq 'ExecStartPre=+/usr/bin/install -d -o jarvis-reader -g jarvis -m 2750 /run/jarvis-page-reader' "$service"
[[ $(grep '^ReadWritePaths=' "$service") == 'ReadWritePaths=-/run/jarvis-page-reader' ]]
grep -Fxq 'ExecStart=/opt/jarvis/current/jarvis-page-reader' "$service"
grep -Fxq 'Environment=JARVIS_PAGE_READER_ALLOWED_PEERS=jarvis' "$service"
[[ $(grep -c '^Environment=' "$service") == 1 ]]
for directive in NoNewPrivileges=true CapabilityBoundingSet= LockPersonality=true \
    RestrictSUIDSGID=true PrivateTmp=true PrivateDevices=true ProtectHome=true ProtectSystem=strict \
    ProtectControlGroups=true ProtectKernelModules=true ProtectKernelTunables=true \
    ProtectKernelLogs=true ProtectClock=true ProtectHostname=true ProtectProc=invisible ProcSubset=pid \
    RestrictRealtime=true RestrictNamespaces=true SystemCallArchitectures=native \
    SystemCallFilter=@system-service MemoryDenyWriteExecute=true \
    'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6' \
    UMask=0117 LimitCORE=0 TasksMax=32 MemoryMax=256M Restart=on-failure StartLimitIntervalSec=300 StartLimitBurst=5 \
    PrivatePIDs=true RemoveIPC=true 'SystemCallFilter=~@privileged @resources' NoExecPaths=/ \
    'ExecPaths=/opt/jarvis/releases /usr/lib/x86_64-linux-gnu -/usr/lib64' \
    IPAddressAllow=127.0.0.53 SocketBindDeny=any; do
    grep -Fxq "$directive" "$service"
done
deny=$(grep '^IPAddressDeny=' "$service")
for range in localhost link-local multicast 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 172.16.0.0/12 \
    192.0.0.0/24 192.0.2.0/24 192.88.99.0/24 192.168.0.0/16 198.18.0.0/15 198.51.100.0/24 \
    203.0.113.0/24 240.0.0.0/4 ::/128 ::ffff:0:0/96 64:ff9b::/96 64:ff9b:1::/48 100::/64 \
    2001::/23 2001:db8::/32 2002::/16 3fff::/20 fc00::/7 fec0::/10; do
    [[ " ${deny#IPAddressDeny=} " == *" $range "* ]] || { echo "IPAddressDeny lacks $range" >&2; exit 1; }
done
inaccessible=$(grep '^InaccessiblePaths=' "$service")
for path in /etc/jarvis /var/lib/jarvis -/var/lib/jarvis-claude -/var/lib/jarvis-codex -/run/jarvis-core \
    -/run/jarvis-ibkr-broker -/run/docker.sock; do
    [[ " ${inaccessible#InaccessiblePaths=} " == *" $path "* ]] || { echo "InaccessiblePaths lacks $path" >&2; exit 1; }
done
! grep -Eq '^(SupplementaryGroups|EnvironmentFile|StateDirectory|RuntimeDirectory|AmbientCapabilities|DynamicUser|LoadCredential|SetCredential|ConditionPathExists)=' "$service"

fixture=$(mktemp -d /tmp/jarvis-page-reader-release.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
# systemd-analyze also checks that ExecStart exists: verify a copy whose only
# change is a disposable executable in place of the release binary.
stub="$fixture/jarvis-page-reader"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stub"
chmod 0755 "$stub"
sed "s#^ExecStart=/opt/jarvis/current/jarvis-page-reader\$#ExecStart=$stub#" \
    "$service" > "$fixture/jarvis-page-reader.service"
if ! verification=$(systemd-analyze verify "$fixture/jarvis-page-reader.service" 2>&1); then
    residual=$(printf '%s\n' "$verification" | sed \
        -e '/^Failed to turn off SO_PASSRIGHTS on user lookup socket, ignoring: Operation not permitted$/d' \
        -e '/^Failed to enable SO_PASSCRED on handoff timestamp socket: Operation not permitted$/d')
    [[ -z $residual ]] || { printf '%s\n' "$verification" >&2; exit 1; }
    echo 'systemd-analyze verify restricted by local sandbox; static unit checks completed'
fi

release=$fixture/release
mkdir -p "$release"
install -m 0755 "$repo/deploy/systemd/manage-systemd-units.sh" "$release/manage-systemd-units"
for helper in verify-home-node install-home-node-core jarvis-page-reader; do
    install -m 0755 "$stub" "$release/$helper"
done
printf '# fixture\n' > "$release/ui.sh"
for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
    jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
    jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service \
    jarvis-private-agent-updater.timer jarvis-page-reader.service; do
    install -m 0644 "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
done
checksums() {
    (cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
        jarvis-page-reader ui.sh systemd-* > artifact-binaries.sha256)
}
rejected() {
    if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
        echo "$1 was accepted" >&2; exit 1
    fi
}
declared='{"tooling":{"systemd_units":1,"page_reader":1}}'
checksums
printf '%s\n' "$declared" > "$release/release.json"
"$release/manage-systemd-units" validate-artifacts "$release"

printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
rejected 'an undeclared page reader unit'
printf '{"tooling":{"systemd_units":1,"page_reader":2}}\n' > "$release/release.json"
rejected 'an unsupported page reader capability'
printf '{"tooling":{"systemd_units":1,"page_reader":"1"}}\n' > "$release/release.json"
rejected 'a non-numeric page reader capability'
printf '{"tooling":{"page_reader":1}}\n' > "$release/release.json"
rejected 'a page reader without managed systemd policy'

printf '%s\n' "$declared" > "$release/release.json"
printf '# tamper\n' >> "$release/jarvis-page-reader"
rejected 'a tampered page reader'
install -m 0755 "$stub" "$release/jarvis-page-reader"
chmod 0777 "$release/jarvis-page-reader"
checksums
rejected 'a group-writable page reader'
install -m 0755 "$stub" "$release/jarvis-page-reader"
mv "$release/systemd-jarvis-page-reader.service" "$fixture/saved-service"
checksums
rejected 'a missing declared page reader unit'
mv "$fixture/saved-service" "$release/systemd-jarvis-page-reader.service"
mv "$release/jarvis-page-reader" "$fixture/saved-binary"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* > artifact-binaries.sha256)
rejected 'a missing declared page reader binary'

# A release from before the reader stays valid without it.
mv "$release/systemd-jarvis-page-reader.service" "$fixture/saved-service"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* > artifact-binaries.sha256)
printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
"$release/manage-systemd-units" validate-artifacts "$release"

# Nothing enables or starts the reader: the installer only creates its
# identity, and the updater only try-restarts a running reader and refuses to
# roll back to a release without it while the owner has it enabled or active.
installer="$repo/deploy/systemd/install-home-node-core.sh"
grep -Fq 'useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin jarvis-reader' "$installer"
! grep -Eq 'systemctl [^|]*jarvis-page-reader' "$installer"
updater="$repo/deploy/systemd/update-core-release.sh"
[[ $(grep -c '^    systemctl try-restart jarvis-page-reader.service >/dev/null 2>&1 || true$' "$updater") == 2 ]]
! grep -Eq 'systemctl (enable|start|restart) [^|]*jarvis-page-reader' "$updater"
grep -Fq "jq -e '.tooling.page_reader == 1' \"\$previous/release.json\"" "$updater"
grep -Fq 'disable --now jarvis-page-reader.service` before rolling back' "$updater"
echo 'Page reader release tests passed'
