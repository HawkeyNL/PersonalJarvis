#!/usr/bin/env bash
# Validate, install, compare and restore the fixed set of release-owned Jarvis
# systemd units. This root-only helper never accepts a unit name from a caller.
set -euo pipefail

readonly systemd_root=${JARVIS_SYSTEMD_ROOT:-/etc/systemd/system}
readonly releases_root=${JARVIS_RELEASES_ROOT:-/opt/jarvis/releases}
readonly polkit_root=${JARVIS_POLKIT_ROOT:-/usr/share/polkit-1/actions}
readonly device_policy=com.hawkeynl.jarvis.devices.policy
readonly -a managed_units=(
    jarvis-core.service
    jarvis-config-broker.service
    jarvis-codex-broker.service
    jarvis-codex.service
    jarvis-opensandbox.service
    jarvis-surrealdb.service
    jarvis-updater.service
    jarvis-updater.timer
    jarvis-private-agent-updater.service
    jarvis-private-agent-updater.timer
)

fail() { echo "jarvis systemd units: $*" >&2; exit 1; }
usage() {
    echo "usage: $0 validate-artifacts|validate-release|check-installed|install|restore RELEASE [BACKUP_DIR]" >&2
    exit 64
}

test_override_allowed() {
    [[ ${JARVIS_SYSTEMD_TEST_MODE:-false} == true && ${GITHUB_ACTIONS:-false} == true ]] ||
        fail "systemd path overrides are test-only"
}

if [[ $systemd_root != /etc/systemd/system || $releases_root != /opt/jarvis/releases || $polkit_root != /usr/share/polkit-1/actions ]]; then
    test_override_allowed
fi

device_capability() {
    local release=$1
    if ! jq -e '.tooling | has("local_devices")' "$release/release.json" >/dev/null; then
        echo legacy
        return
    fi
    jq -e '.tooling.local_devices == 1 and (.tooling.local_devices | type) == "number"' \
        "$release/release.json" >/dev/null || fail "unsupported local-device capability"
    echo 1
}

validate_device_policy() {
    local release=$1 metadata matches device_version
    device_version=$(device_capability "$release") || return 1
    [[ $device_version == 1 ]] || return 0
    [[ -f $release/$device_policy && ! -L $release/$device_policy ]] || fail "device policy is missing or unsafe"
    metadata=$(stat -c '%a' "$release/$device_policy")
    (( (8#$metadata & 0022) == 0 )) || fail "device policy is writable by non-owner"
    matches=$(awk -v name="$device_policy" '$2 == name { count++ } END { print count + 0 }' "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || fail "device policy is not uniquely checksum-bound"
}

validate_policy_target() {
    local metadata target="$polkit_root/$device_policy"
    [[ ! -L $polkit_root ]] || fail "unsafe PolicyKit directory"
    if [[ -e $polkit_root ]]; then
        [[ -d $polkit_root ]] || fail "unsafe PolicyKit directory"
        metadata=$(stat -c '%u:%g:%a' "$polkit_root")
        [[ $metadata == 0:0:* ]] && (( (8#${metadata##*:} & 0022) == 0 )) || fail "unsafe PolicyKit directory ownership/mode"
    fi
    if [[ -e $target || -L $target ]]; then
        [[ -f $target && ! -L $target ]] || fail "unsafe installed device policy"
        metadata=$(stat -c '%u:%g:%a' "$target")
        [[ $metadata == 0:0:* ]] && (( (8#${metadata##*:} & 0022) == 0 )) || fail "unsafe installed device policy ownership/mode"
    fi
}

capability() {
    local release=$1
    [[ -f $release/release.json && ! -L $release/release.json ]] || fail "release manifest is missing or unsafe"
    if ! jq -e '((.tooling? | type) == "object") and (.tooling | has("systemd_units"))' \
        "$release/release.json" >/dev/null 2>&1; then
        printf 'legacy\n'
        return
    fi
    jq -e '(.tooling.systemd_units | type) == "number" and .tooling.systemd_units == 1' \
        "$release/release.json" >/dev/null 2>&1 || fail "unsupported managed-systemd capability"
    printf '1\n'
}

validate_checksum_manifest() {
    local release=$1
    [[ -f $release/artifact-binaries.sha256 && ! -L $release/artifact-binaries.sha256 ]] ||
        fail "artifact checksum manifest is missing or unsafe"
    LC_ALL=C awk '
        NF != 2 || $1 !~ /^[0-9a-f]{64}$/ ||
          $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ { exit 1 }
        seen[$2]++ { if (seen[$2] > 1) exit 1 }
        END { if (NR == 0) exit 1 }
    ' "$release/artifact-binaries.sha256" || fail "artifact checksum manifest is malformed or duplicated"
}

validate_artifacts() {
    local release=$1 unit path mode matches packaged expected managed_version device_version
    managed_version=$(capability "$release") || return 1
    device_version=$(device_capability "$release") || return 1
    if [[ $managed_version != 1 ]]; then
        [[ $device_version == legacy ]] || fail "local-device capability requires managed systemd policy"
        return 0
    fi
    validate_checksum_manifest "$release"
    validate_device_policy "$release"
    [[ -f $release/manage-systemd-units && ! -L $release/manage-systemd-units && -x $release/manage-systemd-units ]] ||
        fail "managed-systemd helper is missing or unsafe"
    for helper in verify-home-node install-home-node-core; do
        [[ -f $release/$helper && ! -L $release/$helper && -x $release/$helper ]] ||
            fail "versioned Home Node helper is missing or unsafe: $helper"
    done
    [[ -f $release/ui.sh && ! -L $release/ui.sh ]] || fail "versioned terminal UI helper is missing or unsafe"
    for packaged in "$release"/systemd-*.service "$release"/systemd-*.timer; do
        [[ -e $packaged || -L $packaged ]] || continue
        expected=false
        for unit in "${managed_units[@]}"; do
            [[ ${packaged##*/} == "systemd-$unit" ]] && expected=true
        done
        [[ $expected == true ]] || fail "unexpected managed unit artifact: ${packaged##*/}"
    done
    for unit in "${managed_units[@]}"; do
        path="$release/systemd-$unit"
        [[ -f $path && ! -L $path ]] || fail "managed unit is missing or unsafe: $unit"
        mode=$(stat -c '%a' "$path")
        (( (8#$mode & 0022) == 0 )) || fail "managed unit permissions are unsafe: $unit"
        matches=$(awk -v name="systemd-$unit" '$2 == name { count++ } END { print count + 0 }' \
            "$release/artifact-binaries.sha256")
        [[ $matches == 1 ]] || fail "managed unit is not uniquely checksum-bound: $unit"
    done
    matches=$(awk '$2 == "manage-systemd-units" { count++ } END { print count + 0 }' \
        "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || fail "managed-systemd helper is not uniquely checksum-bound"
    matches=$(awk '$2 == "verify-home-node" { count++ } END { print count + 0 }' \
        "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || fail "Home Node verifier is not uniquely checksum-bound"
    matches=$(awk '$2 == "install-home-node-core" { count++ } END { print count + 0 }' \
        "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || fail "Home Node installer is not uniquely checksum-bound"
    matches=$(awk '$2 == "ui.sh" { count++ } END { print count + 0 }' \
        "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || fail "terminal UI helper is not uniquely checksum-bound"
    (cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null) ||
        fail "release artifact checksum verification failed"
}

validate_release() {
    local release=$1 entry helper metadata
    validate_artifacts "$release"
    if [[ $(device_capability "$release") == 1 ]]; then
        [[ $(stat -c '%u:%g' "$release/$device_policy") == 0:0 ]] || fail "device policy is not root-owned"
    fi
    [[ $(capability "$release") == 1 ]] || return 0
    entry=$(find "$release" -maxdepth 1 -type f -name 'systemd-*' \
        \( ! -user root -o ! -group root -o -perm /022 \) -printf '%f (%y %u:%g %m)\n' -quit)
    [[ -z $entry ]] || fail "managed unit tree has unsafe ownership, permissions, or links: $entry"
    for helper in manage-systemd-units verify-home-node install-home-node-core; do
        metadata=$(stat -c '%u:%g:%a' "$release/$helper")
        [[ $metadata == 0:0:* ]] || fail "versioned systemd tooling is not root-owned: $helper"
        (( (8#${metadata##*:} & 0022) == 0 && (8#${metadata##*:} & 0111) != 0 )) ||
            fail "versioned systemd tooling permissions are unsafe: $helper"
    done
    metadata=$(stat -c '%u:%g:%a' "$release/ui.sh")
    [[ $metadata == 0:0:* ]] || fail "versioned terminal UI helper is not root-owned"
    (( (8#${metadata##*:} & 0022) == 0 )) || fail "versioned terminal UI helper permissions are unsafe"
}

validate_dropins() {
    local unit directory file metadata line key
    local -a files=()
    for unit in "${managed_units[@]}"; do
        directory="$systemd_root/$unit.d"
        [[ ! -e $directory && ! -L $directory ]] && continue
        [[ -d $directory && ! -L $directory ]] || fail "unsafe drop-in directory for $unit"
        metadata=$(stat -c '%u:%g:%a' "$directory")
        [[ $metadata == 0:0:* ]] || fail "drop-in directory is not root-owned: $directory"
        (( (8#${metadata##*:} & 0022) == 0 )) || fail "drop-in directory is group/world writable: $directory"
        files=()
        mapfile -d '' files < <(find "$directory" -mindepth 1 -maxdepth 1 -name '*.conf' -print0 | LC_ALL=C sort -z)
        for file in "${files[@]}"; do
            [[ -f $file && ! -L $file ]] || fail "unsafe drop-in for $unit: $file"
            metadata=$(stat -c '%u:%g:%a' "$file")
            [[ $metadata == 0:0:* ]] || fail "drop-in is not root-owned: $file"
            (( (8#${metadata##*:} & 0022) == 0 )) || fail "drop-in is group/world writable: $file"
            while IFS= read -r line || [[ -n $line ]]; do
                line=${line#${line%%[![:space:]]*}}
                [[ -z $line || $line == \#* || $line == \;* || $line == \[* ]] && continue
                key=${line%%=*}
                key=${key%${key##*[![:space:]]}}
                case $key in
                    Type|RemainAfterExit|ExecStart|ExecStartPre|ExecStartPost|ExecReload|ExecStop|User|Group|SupplementaryGroups|DynamicUser|Environment|EnvironmentFile|WorkingDirectory|RootDirectory|RootImage|NoNewPrivileges|CapabilityBoundingSet|AmbientCapabilities|ProtectSystem|ProtectHome|ProtectControlGroups|ProtectKernelModules|ProtectKernelTunables|PrivateTmp|PrivateDevices|RestrictAddressFamilies|ReadWritePaths|ReadOnlyPaths|InaccessiblePaths|BindPaths|BindReadOnlyPaths|RuntimeDirectory|RuntimeDirectoryMode|StateDirectory|StateDirectoryMode|CacheDirectory|LogsDirectory|UMask|Requires|Wants|After|Before|ConditionPathExists|Unit|OnBootSec|OnUnitActiveSec|OnCalendar|Persistent|RandomizedDelaySec)
                        fail "conflicting release-owned directive $key in administrator drop-in $file"
                        ;;
                esac
            done < "$file"
        done
    done
}

check_installed() {
    local release=$1 unit source target metadata
    validate_release "$release"
    [[ $(capability "$release") == 1 ]] || fail "active release does not manage systemd units"
    validate_dropins
    validate_policy_target
    if [[ $(device_capability "$release") == 1 ]]; then
        [[ $(stat -c '%a' "$polkit_root/$device_policy" 2>/dev/null) == 644 ]] && \
            cmp -s "$release/$device_policy" "$polkit_root/$device_policy" || fail "installed device policy differs from active release"
    else
        [[ ! -e $polkit_root/$device_policy ]] || fail "legacy release has incompatible device policy installed"
    fi
    for unit in "${managed_units[@]}"; do
        source="$release/systemd-$unit"
        target="$systemd_root/$unit"
        [[ -f $target && ! -L $target ]] || fail "installed managed unit is missing or unsafe: $unit"
        metadata=$(stat -c '%u:%g:%a' "$target")
        [[ $metadata == 0:0:644 ]] || fail "installed managed unit permissions differ from release policy: $unit"
        cmp -s -- "$source" "$target" || fail "installed managed unit differs from active release: $unit"
    done
}

install_units() {
    local release=$1 backup=$2 unit source target staged
    local -a staged_units=()
    validate_release "$release"
    [[ $(capability "$release") == 1 ]] || fail "target release does not manage systemd units"
    validate_dropins
    validate_policy_target
    [[ -d $backup && ! -L $backup && $(stat -c '%u:%g:%a' "$backup") == 0:0:700 ]] ||
        fail "unit rollback directory is unsafe"
    if [[ -f $polkit_root/$device_policy ]]; then
        install -o root -g root -m 0644 "$polkit_root/$device_policy" "$backup/$device_policy"
        printf 'present\n' > "$backup/device-policy-state"
    else
        printf 'absent\n' > "$backup/device-policy-state"
    fi
    install -d -o root -g root -m 0755 "$systemd_root"
    : > "$backup/state"
    chmod 0600 "$backup/state"
    for unit in "${managed_units[@]}"; do
        target="$systemd_root/$unit"
        if [[ -e $target || -L $target ]]; then
            [[ -f $target && ! -L $target ]] || fail "installed managed unit is not a regular file: $unit"
            metadata=$(stat -c '%u:%g:%a' "$target")
            [[ $metadata == 0:0:* ]] || fail "installed managed unit is not root-owned: $unit"
            (( (8#${metadata##*:} & 0022) == 0 )) || fail "installed managed unit permissions are unsafe: $unit"
            install -o root -g root -m 0644 "$target" "$backup/$unit"
            printf '%s present\n' "$unit" >> "$backup/state"
        else
            printf '%s absent\n' "$unit" >> "$backup/state"
        fi
    done
    for unit in "${managed_units[@]}"; do
        source="$release/systemd-$unit"
        staged="$systemd_root/.$unit.jarvis-new"
        rm -f -- "$staged"
        if ! install -o root -g root -m 0644 "$source" "$staged"; then
            ((${#staged_units[@]} == 0)) || rm -f -- "${staged_units[@]}"
            fail "could not stage managed unit: $unit"
        fi
        staged_units+=("$staged")
    done
    for unit in "${managed_units[@]}"; do
        if ! mv -Tf "$systemd_root/.$unit.jarvis-new" "$systemd_root/$unit"; then
            restore_units "$backup"
            rm -f -- "${staged_units[@]}"
            fail "managed unit replacement failed; prior units restored"
        fi
    done
    if ! (install_device_policy "$release") || ! (check_installed "$release"); then
        restore_units "$backup"
        fail "installed unit verification failed; prior units restored"
    fi
}

install_device_policy() {
    local release=$1 staged
    if [[ $(device_capability "$release") == 1 ]]; then
        install -d -o root -g root -m 0755 "$polkit_root"
        staged=$(mktemp "$polkit_root/.jarvis-device-policy.XXXXXXXX")
        if ! install -o root -g root -m 0644 "$release/$device_policy" "$staged" || \
            ! mv -Tf "$staged" "$polkit_root/$device_policy"; then
            rm -f -- "$staged"
            return 1
        fi
    else
        rm -f -- "$polkit_root/$device_policy"
    fi
}

restore_units() {
    local backup=$1 unit state staged
    [[ -d $backup && ! -L $backup && -f $backup/state && ! -L $backup/state ]] ||
        fail "unit rollback state is unavailable"
    validate_policy_target
    if [[ -f $backup/device-policy-state && ! -L $backup/device-policy-state ]]; then
        case $(<"$backup/device-policy-state") in
            present)
                [[ -f $backup/$device_policy && ! -L $backup/$device_policy ]] || fail "device policy rollback file missing"
                install -d -o root -g root -m 0755 "$polkit_root"
                staged=$(mktemp "$polkit_root/.jarvis-device-policy.XXXXXXXX")
                install -o root -g root -m 0644 "$backup/$device_policy" "$staged"
                mv -Tf "$staged" "$polkit_root/$device_policy"
                ;;
            absent) rm -f -- "$polkit_root/$device_policy" ;;
            *) fail "invalid device policy rollback state" ;;
        esac
    fi
    for unit in "${managed_units[@]}"; do
        read -r _ state < <(awk -v unit="$unit" '$1 == unit { print; exit }' "$backup/state")
        case $state in
            present)
                [[ -f $backup/$unit && ! -L $backup/$unit ]] || fail "unit rollback file is missing: $unit"
                staged="$systemd_root/.$unit.jarvis-restore"
                install -o root -g root -m 0644 "$backup/$unit" "$staged"
                mv -Tf "$staged" "$systemd_root/$unit"
                ;;
            absent) rm -f -- "$systemd_root/$unit" ;;
            *) fail "unit rollback state is malformed: $unit" ;;
        esac
    done
}

[[ $# -ge 2 && $# -le 3 ]] || usage
command=$1
release=$2
case $command in
    validate-artifacts) [[ $# == 2 ]] || usage; validate_artifacts "$release" ;;
    validate-release) [[ $# == 2 ]] || usage; validate_release "$release" ;;
    check-installed) [[ $# == 2 ]] || usage; check_installed "$release" ;;
    install) [[ $# == 3 ]] || usage; install_units "$release" "$3" ;;
    restore) [[ $# == 3 ]] || usage; restore_units "$3" ;;
    *) usage ;;
esac
