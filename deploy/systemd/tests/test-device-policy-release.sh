#!/usr/bin/env bash
# Only temporary fixture paths are mutated. No production services are called:
# systemctl is a fake that only records its arguments.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'root-owned file fixture requires root' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'umount "$fixture/polkit" 2>/dev/null || true; rm -rf -- "$fixture"' EXIT
export GITHUB_ACTIONS=true JARVIS_SYSTEMD_TEST_MODE=true
mkdir -p "$fixture/bin"
cat > "$fixture/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$JARVIS_SYSTEMCTL_LOG"
EOF
chmod 0755 "$fixture/bin/systemctl"
export PATH="$fixture/bin:$PATH" JARVIS_SYSTEMCTL_LOG="$fixture/systemctl.log"
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

# A release that moves the model-policy layout stops Core and the config broker
# during install. A PolicyKit directory the updater sandbox cannot write must
# not strand them: units are restored, systemd reloaded, both services started
# again, and the install still fails.
polkit_readonly() { mount --bind "$JARVIS_POLKIT_ROOT" "$JARVIS_POLKIT_ROOT"; mount -o remount,bind,ro "$JARVIS_POLKIT_ROOT"; }
layout="$fixture/releases/layout"
cp -a "$release" "$layout"
printf '#!/usr/bin/env bash\ncase $1 in layout) echo legacy ;; initialize|select-*|check-directory) ;; *) exit 1 ;; esac\n' \
    > "$layout/jarvis-model-policy-storage"
chmod 0755 "$layout/jarvis-model-policy-storage"
printf '{"tooling":{"systemd_units":1,"local_devices":1,"model_policy_directory":1}}\n' > "$layout/release.json"
(cd "$layout" && sha256sum systemd-* manage-systemd-units verify-home-node install-home-node-core ui.sh "$policy" jarvis-model-policy-storage > artifact-binaries.sha256)
layout_manager="$layout/manage-systemd-units"
printf 'previous policy fixture\n' > "$JARVIS_POLKIT_ROOT/$policy"
printf '# previous broker unit\n' > "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service"
chmod 0644 "$JARVIS_POLKIT_ROOT/$policy" "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service"
cp "$JARVIS_POLKIT_ROOT/$policy" "$fixture/previous"
cp "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service" "$fixture/previous-broker"
polkit_readonly
mkdir -m 0700 "$fixture/readonly-backup"
: > "$JARVIS_SYSTEMCTL_LOG"
if bash "$layout_manager" install "$layout" "$fixture/readonly-backup" 2>"$fixture/readonly.err"; then
    echo 'install reported success without writing the changed device policy' >&2; exit 1
fi
grep -Fq 'prior units restored' "$fixture/readonly.err"
cmp "$fixture/previous-broker" "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service"
cmp "$fixture/previous" "$JARVIS_POLKIT_ROOT/$policy"
[[ $(tail -n 2 "$JARVIS_SYSTEMCTL_LOG") == $'daemon-reload\nstart jarvis-config-broker.service jarvis-core.service' ]]

# A device policy that cannot be restored is reported only after every unit.
umount "$JARVIS_POLKIT_ROOT"
install -m 0644 "$layout/$policy" "$JARVIS_POLKIT_ROOT/$policy"
printf '# candidate broker unit\n' > "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service"
polkit_readonly
if bash "$layout_manager" restore "$layout" "$fixture/readonly-backup" 2>/dev/null; then
    echo 'restore reported success without restoring the device policy' >&2; exit 1
fi
cmp "$fixture/previous-broker" "$JARVIS_SYSTEMD_ROOT/jarvis-config-broker.service"

# Unchanged policy bytes need no write, so the same install succeeds read-only.
bash "$layout_manager" install "$layout" "$fixture/readonly-backup"
bash "$layout_manager" check-installed "$layout"
umount "$JARVIS_POLKIT_ROOT"
echo 'Device policy install, repair, legacy switch and rollback fixtures passed'
