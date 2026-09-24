#!/usr/bin/env bash
# Disposable GitHub runner only: real native migration + real unit transaction,
# fake systemctl. Never start a service, read credentials, or contact a network.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-false} == true && $EUID == 0 ]] || { echo 'isolated root CI fixture only' >&2; exit 1; }
[[ ! -e /etc/jarvis && ! -L /etc/jarvis && ! -e /opt/jarvis && ! -L /opt/jarvis ]] || {
    echo 'refusing to use a runner with existing Jarvis state' >&2; exit 1;
}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
binary=${JARVIS_POLICY_STORAGE_BINARY:?compiled native migration helper required}
[[ -f $binary && -x $binary && ! -L $binary ]]
fixture=$(mktemp -d /tmp/jarvis-policy-install.XXXXXXXX)
cleanup() {
    rm -f -- /opt/jarvis/current
    rmdir /opt/jarvis 2>/dev/null || true
    rm -f -- /etc/jarvis/model-policy/policy.json /etc/jarvis/model-policy/layout /etc/jarvis/model-policy.json
    rmdir /etc/jarvis/model-policy /etc/jarvis 2>/dev/null || true
    rm -rf -- "$fixture"
}
trap cleanup EXIT
install -d -o root -g root -m 0750 /etc/jarvis
install -d -o root -g root -m 0755 /opt/jarvis
mkdir -p "$fixture/bin" "$fixture/releases" "$fixture/systemd" "$fixture/polkit"
cat > "$fixture/bin/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$JARVIS_POLICY_TEST_LOG"
exit 0
EOF
chmod 0755 "$fixture/bin/systemctl"
export PATH="$fixture/bin:$PATH" JARVIS_POLICY_TEST_LOG="$fixture/services.log"
export JARVIS_SYSTEMD_TEST_MODE=true JARVIS_SYSTEMD_ROOT="$fixture/systemd"
export JARVIS_RELEASES_ROOT="$fixture/releases" JARVIS_POLKIT_ROOT="$fixture/polkit"

candidate() {
    local version=$1 capability=$2 release="$fixture/releases/$1" unit helper
    mkdir -p "$release"
    cp "$repo/deploy/systemd/manage-systemd-units.sh" "$release/manage-systemd-units"
    chmod 0755 "$release/manage-systemd-units"
    for helper in verify-home-node install-home-node-core; do
        printf '#!/usr/bin/env bash\nexit 0\n' > "$release/$helper"
        chmod 0755 "$release/$helper"
    done
    printf '# fixture\n' > "$release/ui.sh"
    for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
        jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
        jarvis-updater.service jarvis-updater.timer jarvis-private-agent-updater.service jarvis-private-agent-updater.timer; do
        cp "$repo/deploy/systemd/$unit" "$release/systemd-$unit"
        if [[ $capability == legacy && $unit == jarvis-config-broker.service ]]; then
            sed -i 's#ReadWritePaths=/etc/jarvis/model-policy$#ReadWritePaths=/etc/jarvis/model-policy.json#' "$release/systemd-$unit"
        fi
    done
    if [[ $capability == directory ]]; then
        install -o root -g root -m 0755 "$binary" "$release/jarvis-model-policy-storage"
        printf '{"tooling":{"systemd_units":1,"model_policy_directory":1}}\n' > "$release/release.json"
    else
        printf '{"tooling":{"systemd_units":1}}\n' > "$release/release.json"
    fi
    (cd "$release"; sha256sum manage-systemd-units verify-home-node install-home-node-core ui.sh systemd-* > artifact-binaries.sha256
        if [[ $capability == directory ]]; then sha256sum jarvis-model-policy-storage >> artifact-binaries.sha256; fi)
}
candidate v1.0.0 legacy
candidate v1.1.0 directory
new="$fixture/releases/v1.1.0"
old="$fixture/releases/v1.0.0"
helper="$new/jarvis-model-policy-storage"
manager="$new/manage-systemd-units"

# Fresh host: initialize explicit deny-by-default and install packaged units.
mkdir -m 0700 "$fixture/fresh-backup"
"$manager" install "$new" "$fixture/fresh-backup"
ln -s "$new" /opt/jarvis/current
"$manager" check-installed "$new"
jq -e '.version == 1 and .models == []' /etc/jarvis/model-policy/policy.json >/dev/null
[[ $(stat -c %a /etc/jarvis/model-policy) == 750 ]]
[[ $(stat -c %a /etc/jarvis/model-policy/policy.json) == 640 ]]

# A current owner change must survive failed activation / restoration and a
# later successful upgrade. Legacy files must remain regular, never symlinks.
printf '{"version":1,"models":[{"provider":"openai-api","model":"fixture","enabled":false}]}\n' > /etc/jarvis/model-policy/policy.json
"$manager" restore "$old" "$fixture/fresh-backup"
ln -sfn "$old" /opt/jarvis/current
[[ $("$helper" layout) == legacy ]]
jq -e '.models[0].enabled == false' /etc/jarvis/model-policy.json >/dev/null
[[ -f /etc/jarvis/model-policy.json && ! -L /etc/jarvis/model-policy.json ]]

mkdir -m 0700 "$fixture/upgrade-backup"
"$manager" install "$new" "$fixture/upgrade-backup"
ln -sfn "$new" /opt/jarvis/current
"$manager" check-installed "$new"
# Same-version repair cannot reimport stale legacy authorization.
printf '{"version":1,"models":[{"provider":"openai-api","model":"fixture","enabled":true}]}\n' > /etc/jarvis/model-policy.json
printf '# stale unit\n' > "$fixture/systemd/jarvis-config-broker.service"
mkdir -m 0700 "$fixture/repair-backup"
"$manager" install "$new" "$fixture/repair-backup"
jq -e '.models[0].enabled == false' /etc/jarvis/model-policy/policy.json >/dev/null
"$manager" check-installed "$new"

# Explicit downgrade uses the current manager, then the legacy release can
# inspect its units without understanding the new capability.
mkdir -m 0700 "$fixture/downgrade-backup"
"$manager" install "$old" "$fixture/downgrade-backup"
ln -sfn "$old" /opt/jarvis/current
"$old/manage-systemd-units" check-installed "$old"
jq -e '.models[0].enabled == false' /etc/jarvis/model-policy.json >/dev/null
"$manager" restore "$new" "$fixture/downgrade-backup"
ln -sfn "$new" /opt/jarvis/current
"$manager" check-installed "$new"
grep -Fq 'stop jarvis-config-broker.service jarvis-core.service' "$fixture/services.log"
echo 'Native model-policy installation, repair and rollback fixtures passed'
