#!/usr/bin/env bash
# Disposable release-format fixture for the read-only IBKR broker; no
# credentials, network or host services.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
service="$repo/deploy/systemd/jarvis-ibkr-broker.service"

# Own identity, Core's socket group, credentials only through LoadCredential=
# under the fixed names the broker reads, and never the encryption key.
grep -Fxq 'User=jarvis-ibkr' "$service"
grep -Fxq 'Group=jarvis-ibkr' "$service"
grep -Fxq 'ExecStartPre=+/usr/bin/install -d -o jarvis-ibkr -g jarvis -m 2750 /run/jarvis-ibkr-broker' "$service"
[[ $(grep '^ReadWritePaths=' "$service") == 'ReadWritePaths=-/run/jarvis-ibkr-broker' ]]
grep -Fxq 'ExecStart=/opt/jarvis/current/jarvis-ibkr-broker' "$service"
grep -Fxq 'Environment=JARVIS_IBKR_BROKER_ALLOWED_PEERS=jarvis' "$service"
for credential in consumer_key:consumer_key access_token:access_token \
    access_token_secret:access_token_secret signature_key:private_signature.pem dh_param:dhparam.pem; do
    grep -Fxq "LoadCredential=${credential%%:*}:/etc/jarvis/secrets/ibkr-oauth/${credential#*:}" "$service"
done
[[ $(grep -c '^LoadCredential=' "$service") == 5 ]]
! grep -Fq private_encryption "$service"
for directive in NoNewPrivileges=true CapabilityBoundingSet= LockPersonality=true \
    RestrictSUIDSGID=true PrivateTmp=true PrivateDevices=true ProtectHome=true ProtectSystem=strict \
    ProtectControlGroups=true ProtectKernelModules=true ProtectKernelTunables=true \
    ProtectKernelLogs=true ProtectClock=true ProtectHostname=true ProtectProc=invisible ProcSubset=pid \
    RestrictRealtime=true RestrictNamespaces=true SystemCallArchitectures=native \
    SystemCallFilter=@system-service MemoryDenyWriteExecute=true \
    'RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6' 'InaccessiblePaths=/etc/jarvis /var/lib/jarvis' \
    UMask=0117 LimitCORE=0 TasksMax=64 Restart=on-failure StartLimitIntervalSec=300 StartLimitBurst=5 \
    PrivatePIDs=true RemoveIPC=true 'SystemCallFilter=~@privileged @resources' NoExecPaths=/ \
    'ExecPaths=/opt/jarvis/releases /usr/lib/x86_64-linux-gnu -/usr/lib64' \
    'IPAddressDeny=localhost link-local multicast 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10 fc00::/7' \
    IPAddressAllow=127.0.0.53; do
    grep -Fxq "$directive" "$service"
done
! grep -Eq '^(SupplementaryGroups|EnvironmentFile|StateDirectory|RuntimeDirectory|AmbientCapabilities|DynamicUser)=' "$service"

fixture=$(mktemp -d /tmp/jarvis-ibkr-release.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
# systemd-analyze also checks that ExecStart exists: verify a copy whose only
# change is a disposable executable in place of the release binary.
stub="$fixture/jarvis-ibkr-broker"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stub"
chmod 0755 "$stub"
sed "s#^ExecStart=/opt/jarvis/current/jarvis-ibkr-broker\$#ExecStart=$stub#" \
    "$service" > "$fixture/jarvis-ibkr-broker.service"
if ! verification=$(systemd-analyze verify "$fixture/jarvis-ibkr-broker.service" 2>&1); then
    residual=$(printf '%s\n' "$verification" | sed \
        -e '/^Failed to turn off SO_PASSRIGHTS on user lookup socket, ignoring: Operation not permitted$/d' \
        -e '/^Failed to enable SO_PASSCRED on handoff timestamp socket: Operation not permitted$/d')
    [[ -z $residual ]] || { printf '%s\n' "$verification" >&2; exit 1; }
    echo 'systemd-analyze verify restricted by local sandbox; static unit checks completed'
fi

release=$fixture/release
mkdir -p "$release"
install -m 0755 "$repo/deploy/systemd/manage-systemd-units.sh" "$release/manage-systemd-units"
for helper in verify-home-node install-home-node-core jarvis-ibkr-broker; do
    install -m 0755 "$stub" "$release/$helper"
done
printf '# fixture\n' > "$release/ui.sh"
for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
    jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
    jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service \
    jarvis-private-agent-updater.timer jarvis-ibkr-broker.service; do
    install -m 0644 "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
done
checksums() {
    (cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
        jarvis-ibkr-broker ui.sh systemd-* > artifact-binaries.sha256)
}
rejected() {
    if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
        echo "$1 was accepted" >&2; exit 1
    fi
}
declared='{"tooling":{"systemd_units":1,"ibkr_broker":1}}'
checksums
printf '%s\n' "$declared" > "$release/release.json"
"$release/manage-systemd-units" validate-artifacts "$release"

printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
rejected 'an undeclared IBKR broker unit'
printf '{"tooling":{"systemd_units":1,"ibkr_broker":2}}\n' > "$release/release.json"
rejected 'an unsupported IBKR broker capability'
printf '{"tooling":{"systemd_units":1,"ibkr_broker":"1"}}\n' > "$release/release.json"
rejected 'a non-numeric IBKR broker capability'
printf '{"tooling":{"ibkr_broker":1}}\n' > "$release/release.json"
rejected 'an IBKR broker without managed systemd policy'

printf '%s\n' "$declared" > "$release/release.json"
printf '# tamper\n' >> "$release/jarvis-ibkr-broker"
rejected 'a tampered IBKR broker'
install -m 0755 "$stub" "$release/jarvis-ibkr-broker"
chmod 0777 "$release/jarvis-ibkr-broker"
checksums
rejected 'a group-writable IBKR broker'
install -m 0755 "$stub" "$release/jarvis-ibkr-broker"
mv "$release/systemd-jarvis-ibkr-broker.service" "$fixture/saved-service"
checksums
rejected 'a missing declared IBKR broker unit'
mv "$fixture/saved-service" "$release/systemd-jarvis-ibkr-broker.service"
rm "$release/jarvis-ibkr-broker"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* > artifact-binaries.sha256)
rejected 'a missing declared IBKR broker binary'

# A release from before the broker stays valid without it.
rm "$release/systemd-jarvis-ibkr-broker.service"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* > artifact-binaries.sha256)
printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
"$release/manage-systemd-units" validate-artifacts "$release"

# The updater only try-restarts a running broker, and refuses to roll back to a
# release without it while the owner has it enabled or active.
updater="$repo/deploy/systemd/update-core-release.sh"
[[ $(grep -c '^    systemctl try-restart jarvis-ibkr-broker.service >/dev/null 2>&1 || true$' "$updater") == 2 ]]
! grep -Eq 'systemctl (enable|start|restart) [^|]*jarvis-ibkr-broker' "$updater"
grep -Fq "jq -e '.tooling.ibkr_broker == 1' \"\$previous/release.json\"" "$updater"
grep -Fq 'disable --now jarvis-ibkr-broker.service` before rolling back' "$updater"
echo 'IBKR broker release tests passed'
