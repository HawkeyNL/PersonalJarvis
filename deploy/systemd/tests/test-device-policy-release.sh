#!/usr/bin/env bash
# Only temporary fixture paths are mutated. No production services are called.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'root-owned file fixture requires root' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
export GITHUB_ACTIONS=true JARVIS_SYSTEMD_TEST_MODE=true
export JARVIS_SYSTEMD_ROOT="$fixture/systemd" JARVIS_RELEASES_ROOT="$fixture/releases" JARVIS_POLKIT_ROOT="$fixture/polkit"
manager="$repo/deploy/systemd/manage-systemd-units.sh"
policy=com.hawkeynl.jarvis.devices.policy
mkdir -p "$fixture/releases/new" "$fixture/backup"
chmod 0700 "$fixture/backup"
release="$fixture/releases/new"
for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service jarvis-private-agent-updater.timer; do
    install -m 0644 "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
done
for helper in manage-systemd-units verify-home-node install-home-node-core; do
    install -m 0755 "$repo/deploy/systemd/$helper.sh" "$release/$helper"
done
install -m 0644 "$repo/deploy/lib/ui.sh" "$release/ui.sh"
install -m 0644 "$repo/jarvis-core-admin/packaging/$policy" "$release/$policy"
printf '{"tooling":{"systemd_units":1,"local_devices":1}}\n' > "$release/release.json"
(cd "$release" && sha256sum systemd-* manage-systemd-units verify-home-node install-home-node-core ui.sh "$policy" > artifact-binaries.sha256)
chmod -R go-w "$release"

bash "$manager" install "$release" "$fixture/backup"
bash "$manager" check-installed "$release"
cmp "$release/$policy" "$JARVIS_POLKIT_ROOT/$policy"
[[ $(stat -c '%u:%g:%a' "$JARVIS_POLKIT_ROOT/$policy") == 0:0:644 ]]
bash "$manager" restore "$release" "$fixture/backup"
[[ ! -e $JARVIS_POLKIT_ROOT/$policy ]]

# Existing reviewed bytes must survive failed-activation recovery unchanged.
printf 'previous policy fixture\n' > "$JARVIS_POLKIT_ROOT/$policy"
chmod 0644 "$JARVIS_POLKIT_ROOT/$policy"
cp "$JARVIS_POLKIT_ROOT/$policy" "$fixture/previous"
bash "$manager" install "$release" "$fixture/backup"
bash "$manager" restore "$release" "$fixture/backup"
cmp "$fixture/previous" "$JARVIS_POLKIT_ROOT/$policy"

# Same-version drift is detectable and repairable.
if bash "$manager" check-installed "$release" 2>/dev/null; then exit 1; fi
bash "$manager" install "$release" "$fixture/backup"
bash "$manager" check-installed "$release"

# Unsafe canonical destinations are refused without following the link.
rm -- "$JARVIS_POLKIT_ROOT/$policy"
ln -s "$fixture/previous" "$JARVIS_POLKIT_ROOT/$policy"
if bash "$manager" install "$release" "$fixture/backup" 2>/dev/null; then exit 1; fi
cmp "$fixture/previous" <(printf 'previous policy fixture\n')
rm -- "$JARVIS_POLKIT_ROOT/$policy"

# A legitimate historical release has no policy. Switching back removes the
# new capability; aborting that switch restores the exact new policy bytes.
bash "$manager" install "$release" "$fixture/backup"
cp -a "$release" "$fixture/releases/legacy"
printf '{"tooling":{"systemd_units":1}}\n' > "$fixture/releases/legacy/release.json"
bash "$manager" install "$fixture/releases/legacy" "$fixture/backup"
[[ ! -e $JARVIS_POLKIT_ROOT/$policy ]]
bash "$manager" restore "$release" "$fixture/backup"
cmp "$release/$policy" "$JARVIS_POLKIT_ROOT/$policy"
echo 'Device policy install, repair, legacy switch and rollback fixtures passed'
