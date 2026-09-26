#!/usr/bin/env bash
# Disposable release fixture; never starts a service or downloads weights.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
socket_unit="$repo/deploy/systemd/jarvis-laya.socket"
grep -Fxq 'ListenStream=/run/jarvis-laya.sock' "$socket_unit"
grep -Fxq 'SocketUser=root' "$socket_unit"
grep -Fxq 'SocketGroup=jarvis' "$socket_unit"
grep -Fxq 'SocketMode=0660' "$socket_unit"
grep -Fxq 'User=jarvis-laya' "$repo/deploy/systemd/jarvis-laya.service"
grep -Fxq 'Group=jarvis-laya' "$repo/deploy/systemd/jarvis-laya.service"
! grep -Eq '^SupplementaryGroups=.*jarvis([[:space:]]|$)' "$repo/deploy/systemd/jarvis-laya.service"
! grep -Eq '^Listen(Stream|Datagram)=[0-9]|^Listen(Stream|Datagram)=127[.]' "$socket_unit"
grep -Fxq 'RestrictAddressFamilies=AF_UNIX' "$repo/deploy/systemd/jarvis-laya.service"
# Update/rollback state transitions are exercised behaviorally by
# test-update-core-release.sh, including socket restart stopping the service.
fixture=$(mktemp -d /tmp/jarvis-laya-release.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
release=$fixture/release
mkdir -p "$release"
install -m 0755 "$repo/deploy/systemd/manage-systemd-units.sh" "$release/manage-systemd-units"
for helper in verify-home-node install-home-node-core; do
    printf '#!/usr/bin/env bash\nexit 0\n' > "$release/$helper"
    chmod 0755 "$release/$helper"
done
printf '# fixture\n' > "$release/ui.sh"
for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
    jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
    jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service \
    jarvis-private-agent-updater.timer; do
    install -m 0644 "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
done
install -m 0644 "$repo/deploy/systemd/laya-offline.py" "$release/laya-offline.py"
install -m 0755 "$repo/deploy/systemd/provision-laya.sh" "$release/provision-laya"
install -m 0644 "$repo/deploy/systemd/jarvis-laya.service" "$release/systemd-jarvis-laya.service"
install -m 0644 "$repo/deploy/systemd/jarvis-laya.socket" "$release/systemd-jarvis-laya.socket"
printf '{"tooling":{"systemd_units":1,"laya_runtime":1}}\n' > "$release/release.json"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* laya-offline.py provision-laya > artifact-binaries.sha256)
"$release/manage-systemd-units" validate-artifacts "$release"

mv "$release/systemd-jarvis-laya.service" "$fixture/saved-unit"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'missing declared Laya unit was accepted' >&2; exit 1
fi
ln -s "$fixture/saved-unit" "$release/systemd-jarvis-laya.service"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'symlink Laya unit was accepted' >&2; exit 1
fi
rm "$release/systemd-jarvis-laya.service"
mv "$fixture/saved-unit" "$release/systemd-jarvis-laya.service"
printf '# tampered\n' >> "$release/systemd-jarvis-laya.service"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'tampered Laya unit was accepted' >&2; exit 1
fi
install -m 0644 "$repo/deploy/systemd/jarvis-laya.service" "$release/systemd-jarvis-laya.service"
mv "$release/systemd-jarvis-laya.socket" "$fixture/saved-socket"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'missing declared Laya socket was accepted' >&2; exit 1
fi
mv "$fixture/saved-socket" "$release/systemd-jarvis-laya.socket"
printf '{"tooling":{"systemd_units":1,"laya_runtime":2}}\n' > "$release/release.json"
if "$release/manage-systemd-units" validate-artifacts "$release" >/dev/null 2>&1; then
    echo 'unsupported Laya capability was accepted' >&2; exit 1
fi
rm "$release/systemd-jarvis-laya.service" "$release/systemd-jarvis-laya.socket" "$release/laya-offline.py" "$release/provision-laya"
printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
(cd "$release" && sha256sum manage-systemd-units verify-home-node install-home-node-core \
    ui.sh systemd-* > artifact-binaries.sha256)
"$release/manage-systemd-units" validate-artifacts "$release"
echo 'Laya release artifacts and legacy compatibility passed'
