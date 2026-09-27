#!/usr/bin/env bash
# Disposable release-format and local-socket fixture; no provider login or network.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
service="$repo/deploy/systemd/jarvis-claude.service"
socket="$repo/deploy/systemd/jarvis-claude.socket"
grep -Fxq 'ListenStream=/run/jarvis-claude.sock' "$socket"
grep -Fxq 'SocketUser=root' "$socket"
grep -Fxq 'SocketGroup=jarvis' "$socket"
grep -Fxq 'SocketMode=0660' "$socket"
grep -Fxq 'User=jarvis-claude' "$service"
grep -Fxq 'Group=jarvis-claude' "$service"
grep -Fxq 'ProtectHome=true' "$service"
grep -Fxq 'ProtectSystem=strict' "$service"
grep -Fxq 'ExecStart=/opt/jarvis/current/jarvis-claude-worker' "$service"
grep -Fxq 'InaccessiblePaths=/etc/jarvis /var/lib/jarvis /var/lib/jarvis-codex' "$service"
! grep -Eq '^SupplementaryGroups=|^EnvironmentFile=.*(anthropic|openai|core[.]env)' "$service"
! grep -Eq '^Listen(Stream|Datagram)=[0-9]|^Listen(Stream|Datagram)=127[.]' "$socket"

fixture=$(mktemp -d /tmp/jarvis-subscription-release.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
# systemd-analyze also checks that ExecStart exists. A clean CI runner has no
# activated /opt/jarvis/current, so verify a copy with only that fixed binary
# path replaced by a disposable executable. The production unit stays intact.
stub="$fixture/jarvis-claude-worker"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stub"
chmod 0755 "$stub"
sed "s#^ExecStart=/opt/jarvis/current/jarvis-claude-worker\$#ExecStart=$stub#" \
    "$service" > "$fixture/jarvis-claude.service"
install -m 0644 "$socket" "$fixture/jarvis-claude.socket"
if ! verification=$(systemd-analyze verify \
    "$fixture/jarvis-claude.service" "$fixture/jarvis-claude.socket" 2>&1); then
    residual=$(printf '%s\n' "$verification" | sed \
        -e '/^Failed to turn off SO_PASSRIGHTS on user lookup socket, ignoring: Operation not permitted$/d' \
        -e '/^Failed to enable SO_PASSCRED on handoff timestamp socket: Operation not permitted$/d')
    [[ -z $residual ]] || { printf '%s\n' "$verification" >&2; exit 1; }
    echo 'systemd-analyze verify restricted by local sandbox; static unit checks completed'
fi

release=$fixture/release
mkdir -p "$release"
install -m 0755 "$repo/deploy/systemd/manage-systemd-units.sh" "$release/manage-systemd-units"
for helper in verify-home-node install-home-node-core jarvis-claude-worker; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$release/$helper"
    chmod 0755 "$release/$helper"
done
printf '# fixture\n' > "$release/ui.sh"
for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
    jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
    jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service \
    jarvis-private-agent-updater.timer jarvis-claude.service jarvis-claude.socket; do
    install -m 0644 "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
done
printf '{"tooling":{"systemd_units":1,"subscription_workers":1}}\n' > "$release/release.json"
checksums() {
    (cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
        jarvis-claude-worker ui.sh systemd-* > artifact-binaries.sha256)
}
checksums
"$release/manage-systemd-units" validate-artifacts "$release"

mv "$release/systemd-jarvis-claude.socket" "$fixture/saved-socket"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'missing declared Claude socket was accepted' >&2; exit 1
fi
ln -s "$fixture/saved-socket" "$release/systemd-jarvis-claude.socket"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'symlink Claude socket was accepted' >&2; exit 1
fi
rm "$release/systemd-jarvis-claude.socket"
mv "$fixture/saved-socket" "$release/systemd-jarvis-claude.socket"
printf '# tamper\n' >> "$release/jarvis-claude-worker"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'tampered Claude worker was accepted' >&2; exit 1
fi
printf '#!/usr/bin/env bash\nexit 0\n' > "$release/jarvis-claude-worker"
chmod 0755 "$release/jarvis-claude-worker"
checksums
printf '{"tooling":{"systemd_units":1,"subscription_workers":2}}\n' > "$release/release.json"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'unsupported subscription capability was accepted' >&2; exit 1
fi
rm "$release/systemd-jarvis-claude.service" "$release/systemd-jarvis-claude.socket" "$release/jarvis-claude-worker"
printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core ui.sh systemd-* > artifact-binaries.sha256)
"$release/manage-systemd-units" validate-artifacts "$release"
echo 'subscription-worker release artifacts and legacy compatibility passed'
