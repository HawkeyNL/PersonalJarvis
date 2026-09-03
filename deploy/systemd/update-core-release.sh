#!/usr/bin/env bash
# Download and activate one verified Jarvis Core GitHub release. This is a
# root-operated systemd helper, not an API or agent capability.
set -euo pipefail

readonly releases_dir=/opt/jarvis/releases
readonly current_link=/opt/jarvis/current
readonly lock_file=/run/jarvis-updater.lock
readonly updater_config=/etc/jarvis/updater.env
readonly canonical_repository=HawkeyNL/PersonalJarvis
readonly api_url=https://api.github.com
readonly core_admin_binary=/usr/bin/jarvis-core-admin
readonly core_admin_desktop=/usr/share/applications/com.hawkeynl.jarvis.core.admin.desktop
readonly legacy_core_admin_desktop=/usr/share/applications/jarvis-core-admin.desktop
readonly core_admin_icon=/usr/share/icons/hicolor/128x128/apps/jarvis-core-admin.png
readonly core_admin_version_file=/usr/share/jarvis-core-admin/version
repository=
github_curl_netrc=

usage() {
    cat >&2 <<'EOF'
Usage: update-core-release [--latest|--version vMAJOR.MINOR.PATCH|--check|--status|--rollback|--rollback-candidates|--rollback-version vMAJOR.MINOR.PATCH]

No argument is equivalent to --latest and is used by the systemd timer.
EOF
    exit 64
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "jarvis updater: required command missing: $1" >&2
        exit 1
    }
}

fail() {
    echo "jarvis updater: $*" >&2
    exit 1
}

valid_repository() {
    [[ $1 =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]
}

write_default_updater_config() {
    local temporary
    install -d -o root -g root -m 0750 /etc/jarvis
    temporary=$(mktemp /etc/jarvis/.updater.env.XXXXXX)
    trap 'rm -f -- "$temporary"' RETURN
    printf 'JARVIS_UPDATE_REPOSITORY=%s\nJARVIS_UPDATE_CHANNEL=stable\n' "$canonical_repository" > "$temporary"
    chown root:root "$temporary"
    chmod 0600 "$temporary"
    mv -f -- "$temporary" "$updater_config"
    trap - RETURN
    echo "jarvis updater: migrated trusted updater configuration for $canonical_repository" >&2
}

load_updater_config() {
    local allow_migration=$1
    local line key value seen_repository=false
    if [[ ! -e $updater_config ]]; then
        # v0.0.10 did not persist an update source. This one-time migration is
        # intentionally the public canonical repository, never caller input.
        if [[ $allow_migration == true ]]; then
            write_default_updater_config
        else
            repository=$canonical_repository
            return
        fi
    fi
    [[ -f $updater_config && ! -L $updater_config ]] || fail "updater configuration is unsafe"
    [[ $(stat -c '%U:%G:%a' "$updater_config") == root:root:600 ]] || \
        fail "updater configuration must be root:root mode 0600"
    while IFS= read -r line || [[ -n $line ]]; do
        [[ -z $line || $line == \#* ]] && continue
        [[ $line == *=* ]] || fail "updater configuration is malformed"
        key=${line%%=*}
        value=${line#*=}
        case $key in
            JARVIS_UPDATE_REPOSITORY)
                [[ $seen_repository == false ]] || fail "updater repository is duplicated"
                valid_repository "$value" || fail "updater repository is invalid"
                repository=$value
                seen_repository=true
                ;;
            JARVIS_UPDATE_CHANNEL) [[ $value == stable ]] || fail "updater channel is invalid" ;;
            JARVIS_GITHUB_CURL_NETRC) [[ $value == /* ]] || fail "updater netrc path is invalid"; github_curl_netrc=$value ;;
            *) fail "updater configuration contains an unsupported key" ;;
        esac
    done < "$updater_config"
    [[ $seen_repository == true ]] || fail "updater repository is missing"
}

version_is_newer() {
    local candidate=${1#v}
    local installed=${2#v}
    local candidate_major candidate_minor candidate_patch
    local installed_major installed_minor installed_patch
    IFS=. read -r candidate_major candidate_minor candidate_patch <<< "$candidate"
    IFS=. read -r installed_major installed_minor installed_patch <<< "$installed"
    ((10#$candidate_major > 10#$installed_major)) ||
        ((10#$candidate_major == 10#$installed_major && 10#$candidate_minor > 10#$installed_minor)) ||
        ((10#$candidate_major == 10#$installed_major && 10#$candidate_minor == 10#$installed_minor && 10#$candidate_patch > 10#$installed_patch))
}

valid_component_version() {
    [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

schema_fingerprint() {
    jq -er '.schema_sha256 | strings | select(test("^[0-9a-f]{64}$"))' "$1"
}

# Releases before the admin_helpers capability legitimately used the global
# compatibility scripts. Once capability version 1 is declared, both helpers
# are immutable release contents and their inner artifact checksums are
# mandatory. Callers decide whether an invalid release is fatal or merely an
# unavailable rollback candidate by inspecting admin_helper_tooling_reason.
admin_helper_tooling_valid() {
    local release=$1 helper metadata mode matches
    admin_helper_tooling_reason=
    if ! jq -e '((.tooling? | type) == "object") and (.tooling | has("admin_helpers"))' \
        "$release/release.json" >/dev/null 2>&1; then
        return 0
    fi
    if ! jq -e '(.tooling.admin_helpers | type) == "number" and .tooling.admin_helpers == 1' \
        "$release/release.json" >/dev/null 2>&1; then
        admin_helper_tooling_reason="unsupported admin-helper tooling capability"
        return 1
    fi
    if [[ ! -f $release/artifact-binaries.sha256 || -L $release/artifact-binaries.sha256 ]]; then
        admin_helper_tooling_reason="admin-helper artifact checksum manifest is missing or unsafe"
        return 1
    fi
    if ! LC_ALL=C awk '
        NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ { exit 1 }
        END { if (NR == 0) exit 1 }
    ' "$release/artifact-binaries.sha256"; then
        admin_helper_tooling_reason="admin-helper artifact checksum manifest is malformed"
        return 1
    fi
    for helper in jarvis-models jarvis-credentials; do
        if [[ ! -f $release/$helper || -L $release/$helper ]]; then
            admin_helper_tooling_reason="versioned admin helper is missing or unsafe: $helper"
            return 1
        fi
        metadata=$(stat -c '%u:%g:%a' "$release/$helper" 2>/dev/null || true)
        if [[ $metadata != 0:0:* ]]; then
            admin_helper_tooling_reason="versioned admin helper is not root-owned: $helper"
            return 1
        fi
        mode=${metadata##*:}
        if (( (8#$mode & 0022) != 0 || (8#$mode & 0111) == 0 )); then
            admin_helper_tooling_reason="versioned admin helper permissions are unsafe: $helper"
            return 1
        fi
        matches=$(awk -v helper="$helper" '$2 == helper { count++ } END { print count + 0 }' \
            "$release/artifact-binaries.sha256")
        if [[ $matches != 1 ]]; then
            admin_helper_tooling_reason="versioned admin helper is not uniquely checksum-bound: $helper"
            return 1
        fi
    done
    if ! (cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null); then
        admin_helper_tooling_reason="release artifact checksum verification failed"
        return 1
    fi
}

systemd_unit_tooling_valid() {
    local release=$1 detail
    systemd_unit_tooling_reason=
    release_has_managed_units=false
    if ! jq -e '((.tooling? | type) == "object") and (.tooling | has("systemd_units"))' \
        "$release/release.json" >/dev/null 2>&1; then
        return 0
    fi
    if ! jq -e '(.tooling.systemd_units | type) == "number" and .tooling.systemd_units == 1' \
        "$release/release.json" >/dev/null 2>&1; then
        systemd_unit_tooling_reason="unsupported managed-systemd capability"
        return 1
    fi
    if [[ ! -x $release/manage-systemd-units || -L $release/manage-systemd-units ]]; then
        systemd_unit_tooling_reason="managed-systemd helper is missing or unsafe"
        return 1
    fi
    if [[ ! -f $release/artifact-binaries.sha256 || -L $release/artifact-binaries.sha256 ]] || \
        ! LC_ALL=C awk '
            NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ { exit 1 }
            seen[$2]++ { if (seen[$2] > 1) exit 1 }
            END { if (NR == 0) exit 1 }
        ' "$release/artifact-binaries.sha256" || \
        ! (cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null); then
        systemd_unit_tooling_reason="managed-systemd checksum manifest is invalid"
        return 1
    fi
    if ! detail=$("$release/manage-systemd-units" validate-artifacts "$release" 2>&1); then
        detail=${detail#jarvis systemd units: }
        systemd_unit_tooling_reason="managed systemd unit artifacts are invalid: ${detail:-validation failed}"
        return 1
    fi
    release_has_managed_units=true
}

cleanup() {
    [[ -n ${staging_dir:-} && -d ${staging_dir:-} ]] || return 0
    case "$staging_dir" in
        "$releases_dir"/.staging.*) rm -rf -- "$staging_dir" ;;
        *) fail "refusing to remove unexpected staging directory" ;;
    esac
}

for command in awk cmp curl find flock jq sha256sum stat tar systemctl readlink mv ln mktemp install wc; do
    require_command "$command"
done

mode=latest
requested_tag=
case ${1:-} in
    '') ;;
    --latest) [[ $# == 1 ]] || usage ;;
    --version) [[ $# == 2 && $2 =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage; mode=version; requested_tag=$2 ;;
    --check) [[ $# == 1 ]] || usage; mode=check ;;
    --status) [[ $# == 1 ]] || usage; mode=status ;;
    --rollback) [[ $# == 1 ]] || usage; mode=rollback ;;
    --rollback-candidates) [[ $# == 1 ]] || usage; mode=rollback_candidates ;;
    --rollback-version) [[ $# == 2 && $2 =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage; mode=rollback_version; requested_tag=$2 ;;
    *) usage ;;
esac

[[ ${EUID} -eq 0 ]] || fail "must run as root"
case $mode in
    check|status|rollback_candidates) load_updater_config false ;;
    *) load_updater_config true ;;
esac
[[ -d $releases_dir && -L $current_link ]] || fail "expected Jarvis release layout is absent"

# Optional private-repository access is supplied through a root-only curl netrc
# file, never through the unit, process environment, or an argument.
curl_args=(--fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 10 --max-time 60)
if [[ -n $github_curl_netrc ]]; then
    [[ -f $github_curl_netrc ]] || fail "configured curl netrc does not exist"
    [[ $(stat -c '%U:%a' "$github_curl_netrc") == root:600 ]] || \
        fail "curl netrc must be root-owned with mode 0600"
    curl_args+=(--netrc-file "$github_curl_netrc")
fi

exec 9>"$lock_file"
flock -n 9 || { echo "jarvis updater: another update is running"; exit 75; }

current_target=$(readlink -f -- "$current_link")
[[ $current_target == "$releases_dir"/* ]] || fail "current release is outside the release directory"
current_tag=
current_schema_sha256=
current_core_version=unavailable
current_cli_version=unavailable
current_core_admin_version=not-installed
current_has_managed_units=false
if [[ -f $current_target/release.json && ! -L $current_target/release.json ]]; then
    current_tag=$(jq -er '.tag | strings' "$current_target/release.json" 2>/dev/null || true)
    [[ $current_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
        fail "installed release manifest has an unsafe tag"
    current_schema_sha256=$(schema_fingerprint "$current_target/release.json" 2>/dev/null || true)
    current_core_version=$(jq -er '.components.core // empty | strings' "$current_target/release.json" 2>/dev/null || true)
    current_cli_version=$(jq -er '.components.cli // empty | strings' "$current_target/release.json" 2>/dev/null || true)
    valid_component_version "$current_core_version" || current_core_version=${current_tag#v}
    valid_component_version "$current_cli_version" || current_cli_version=${current_tag#v}
    if ! systemd_unit_tooling_valid "$current_target"; then
        fail "active release systemd tooling is invalid: $systemd_unit_tooling_reason"
    fi
    current_has_managed_units=$release_has_managed_units
fi
if [[ -f $core_admin_version_file && ! -L $core_admin_version_file && \
    $(stat -c '%U:%G:%a' "$core_admin_version_file") == root:root:644 ]]; then
    read -r current_core_admin_version extra < "$core_admin_version_file" || true
    [[ -z ${extra:-} ]] || current_core_admin_version=unavailable
    valid_component_version "$current_core_admin_version" || current_core_admin_version=unavailable
    if [[ $current_core_admin_version != unavailable && -x $core_admin_binary && \
        ! -L $core_admin_binary && $(stat -c '%U:%G' "$core_admin_binary") == root:root ]]; then
        installed_app_binary_version=$("$core_admin_binary" --component-version 2>/dev/null || true)
        [[ $installed_app_binary_version == "$current_core_admin_version" ]] || \
            current_core_admin_version=unavailable
    else
        current_core_admin_version=unavailable
    fi
fi
previous_tag=

restart_brokers() {
    systemctl try-restart jarvis-config-broker.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-codex-broker.service >/dev/null 2>&1 || true
}

verification_marker_valid() {
    local release=$1 expected_tag=$2 marker marker_kind marker_value extra manifest_sha256
    marker=$release/release.verification
    [[ -f $marker && ! -L $marker ]] || return 1
    [[ $(stat -c '%U:%G:%a' "$marker") == root:root:644 ]] || return 1
    read -r marker_kind marker_value extra < "$marker" || return 1
    [[ -z $extra ]] || return 1
    if [[ ${#marker_kind} -eq 64 && $marker_kind != *[!0-9a-f]* && \
        $marker_value == "jarvis-core-$expected_tag-linux-x86_64.tar.gz" ]]; then
        return 0
    fi
    if [[ $marker_kind == legacy-active-release-manifest && ${#marker_value} -eq 64 && \
        $marker_value != *[!0-9a-f]* ]]; then
        manifest_sha256=$(sha256sum "$release/release.json" | awk '{print $1}')
        [[ $marker_value == "$manifest_sha256" ]]
        return
    fi
    return 1
}

migrate_legacy_release_verification() {
    local release=$1 expected_tag=$2 temporary manifest_sha256 unsafe_entry
    if [[ -e $release/release.verification ]]; then
        verification_marker_valid "$release" "$expected_tag" || \
            fail "installed release has an invalid verification marker: $expected_tag"
        return
    fi

    # Releases activated by the pre-v0.0.14 updater passed the published
    # archive checksum, but that updater did not persist the installation
    # marker. Qualify only the current/immediately previous root-controlled
    # release, after revalidating its immutable layout and exact manifest.
    [[ -d $release && ! -L $release ]] || fail "legacy release directory is unsafe: $expected_tag"
    [[ -f $release/release.json && ! -L $release/release.json ]] || \
        fail "legacy release manifest is unsafe: $expected_tag"
    jq -e --arg tag "$expected_tag" \
        '.tag == $tag and (.schema_sha256 | strings | test("^[0-9a-f]{64}$"))' \
        "$release/release.json" >/dev/null || fail "legacy release manifest is invalid: $expected_tag"
    for executable in jarvis-api jarvis-agent-bundle jarvis-config-broker jarvis-codex-broker jarvis update-core-release; do
        [[ -f $release/$executable && ! -L $release/$executable && -x $release/$executable ]] || \
            fail "legacy release tooling is invalid: $expected_tag"
    done
    if jq -e '.tooling.private_agents? == 1' "$release/release.json" >/dev/null 2>&1; then
        for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
            [[ -f $release/$helper && ! -L $release/$helper && -x $release/$helper ]] || \
                fail "legacy release private-agent tooling is invalid: $expected_tag"
        done
    fi
    admin_helper_tooling_valid "$release" || \
        fail "legacy release admin-helper tooling is invalid: $expected_tag: $admin_helper_tooling_reason"
    systemd_unit_tooling_valid "$release" || \
        fail "legacy release managed-systemd tooling is invalid: $expected_tag: $systemd_unit_tooling_reason"
    unsafe_entry=$(find "$release" -xdev \( -type l -o ! -user root -o ! -group root -o -perm /022 \) \
        -printf '%P (%y %u:%g %m)\n' -quit)
    if [[ -n $unsafe_entry ]]; then
        fail "legacy release permissions are unsafe: $expected_tag: $unsafe_entry"
    fi

    manifest_sha256=$(sha256sum "$release/release.json" | awk '{print $1}')
    temporary=$(mktemp "$release/.release.verification.XXXXXX")
    trap 'rm -f -- "$temporary"' RETURN
    printf 'legacy-active-release-manifest %s\n' "$manifest_sha256" > "$temporary"
    chown root:root "$temporary"
    chmod 0644 "$temporary"
    mv -Tf "$temporary" "$release/release.verification"
    trap - RETURN
    verification_marker_valid "$release" "$expected_tag" || \
        fail "legacy release verification migration failed: $expected_tag"
    echo "jarvis updater: migrated verification state for legacy release $expected_tag"
}

# Populate non-secret inspection fields for one managed release directory.
# Callers must use these fields rather than trusting a directory name alone.
inspect_release() {
    local tag=$1 release="$releases_dir/$1" canonical unsafe_entry manifest_tag schema
    inspected_current=false
    inspected_verified=false
    inspected_rollback_capable=false
    inspected_structurally_valid=false
    inspected_reason="unavailable"
    inspected_schema=

    [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
        inspected_reason="invalid release directory name"
        return 0
    }
    [[ -d $release && ! -L $release ]] || {
        inspected_reason="release directory is missing or unsafe"
        return 0
    }
    canonical=$(readlink -f -- "$release" 2>/dev/null || true)
    [[ $canonical == "$release" && $canonical == "$releases_dir"/* ]] || {
        inspected_reason="release directory resolves outside managed root"
        return 0
    }
    unsafe_entry=$(find "$release" -xdev \( -type l -o ! -user root -o ! -group root -o -perm /022 \) \
        -printf '%P (%y %u:%g %m)\n' -quit)
    [[ -z $unsafe_entry ]] || {
        inspected_reason="unsafe ownership, permissions, or link: $unsafe_entry"
        return 0
    }
    [[ -f $release/release.json && ! -L $release/release.json ]] || {
        inspected_reason="release manifest is missing or unsafe"
        return 0
    }
    manifest_tag=$(jq -er '.tag | strings' "$release/release.json" 2>/dev/null || true)
    [[ $manifest_tag == "$tag" ]] || {
        inspected_reason="release manifest tag does not match directory"
        return 0
    }
    schema=$(schema_fingerprint "$release/release.json" 2>/dev/null || true)
    [[ -n $schema ]] || {
        inspected_reason="release manifest schema fingerprint is invalid"
        return 0
    }
    for executable in jarvis-api jarvis-agent-bundle jarvis-config-broker jarvis-codex-broker jarvis update-core-release; do
        [[ -f $release/$executable && ! -L $release/$executable && -x $release/$executable ]] || {
            inspected_reason="expected release binary or tooling is unavailable"
            return 0
        }
    done
    if jq -e '.tooling.private_agents? == 1' "$release/release.json" >/dev/null 2>&1; then
        for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
            [[ -f $release/$helper && ! -L $release/$helper && -x $release/$helper ]] || {
                inspected_reason="versioned private-agent tooling is unavailable"
                return 0
            }
        done
    fi
    if ! admin_helper_tooling_valid "$release"; then
        inspected_reason=$admin_helper_tooling_reason
        return 0
    fi
    if ! systemd_unit_tooling_valid "$release"; then
        inspected_reason=$systemd_unit_tooling_reason
        return 0
    fi
    local inspected_has_managed_units=$release_has_managed_units
    if jq -e '.components.core_admin? | strings' "$release/release.json" >/dev/null 2>&1; then
        local inspected_app_version inspected_extra
        for app_file in jarvis-core-admin jarvis-core-admin.desktop jarvis-core-admin.png jarvis-core-admin.version; do
            [[ -f $release/$app_file && ! -L $release/$app_file ]] || {
                inspected_reason="graphical administrator artifact is unavailable"
                return 0
            }
        done
        [[ -x $release/jarvis-core-admin ]] || {
            inspected_reason="graphical administrator is not executable"
            return 0
        }
        read -r inspected_app_version inspected_extra < "$release/jarvis-core-admin.version" || {
            inspected_reason="graphical administrator version is invalid"
            return 0
        }
        [[ -z ${inspected_extra:-} && $inspected_app_version == \
            "$(jq -r '.components.core_admin' "$release/release.json")" && \
            "$("$release/jarvis-core-admin" --component-version 2>/dev/null || true)" == "$inspected_app_version" ]] || {
            inspected_reason="graphical administrator version does not match manifest"
            return 0
        }
    fi

    inspected_structurally_valid=true
    inspected_schema=$schema
    [[ $release -ef $current_target ]] && inspected_current=true
    if verification_marker_valid "$release" "$tag"; then
        inspected_verified=true
    else
        inspected_reason="verification marker is missing or invalid"
        return 0
    fi
    if [[ $inspected_current == true ]]; then
        inspected_reason="active release"
    elif [[ $current_has_managed_units == true && $inspected_has_managed_units != true ]]; then
        inspected_reason="legacy release has no integrity-bound systemd units"
    elif [[ -z $current_schema_sha256 ]]; then
        inspected_reason="active release schema fingerprint is unavailable"
    elif [[ $schema != "$current_schema_sha256" ]]; then
        inspected_reason="schema fingerprint differs from active release"
    else
        inspected_rollback_capable=true
        inspected_reason="eligible"
    fi
}

managed_release_tags() {
    find "$releases_dir" -mindepth 1 -maxdepth 1 -type d \
        -regextype posix-extended -regex "$releases_dir/v[0-9]+\.[0-9]+\.[0-9]+" \
        -printf '%f\n' | LC_ALL=C sort -Vr
}

find_verified_previous() {
    local candidate
    while IFS= read -r candidate; do
        inspect_release "$candidate"
        if [[ $inspected_rollback_capable == true ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done < <(managed_release_tags)
    return 1
}

list_rollback_candidates() {
    local candidate first=true
    printf '['
    while IFS= read -r candidate; do
        inspect_release "$candidate"
        [[ $first == true ]] || printf ','
        first=false
        jq -nc \
            --arg version "$candidate" \
            --argjson current "$inspected_current" \
            --argjson verified "$inspected_verified" \
            --argjson rollback_capable "$inspected_rollback_capable" \
            --arg reason "$inspected_reason" \
            '{version: $version, current: $current, verified: $verified, rollback_capable: $rollback_capable, reason: $reason}'
    done < <(managed_release_tags)
    printf ']\n'
}

# Preserve the one-time legacy compatibility boundary, but skip malformed
# historical directories instead of letting them block unrelated updates.
find_or_migrate_rollback_target() {
    local candidate
    while IFS= read -r candidate; do
        inspect_release "$candidate"
        [[ $inspected_current == false ]] || continue
        if [[ $inspected_rollback_capable == true ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
        if [[ $inspected_structurally_valid == true && ! -e $releases_dir/$candidate/release.verification && \
            -n $current_schema_sha256 && $inspected_schema == "$current_schema_sha256" ]]; then
            migrate_legacy_release_verification "$releases_dir/$candidate" "$candidate" >&2
            inspect_release "$candidate"
            if [[ $inspected_rollback_capable == true ]]; then
                printf '%s\n' "$candidate"
                return 0
            fi
        fi
    done < <(managed_release_tags)
    return 1
}

install_versioned_tooling() {
    local release=$1 app_present=false agent_tooling_present=false activation_failed=false
    local app_tmp desktop_tmp icon_tmp version_tmp app_previous desktop_previous icon_previous version_previous
    local index source target temporary previous
    local -a tooling_sources=("$release/jarvis" "$release/update-core-release")
    local -a tooling_targets=(/usr/local/sbin/jarvis /usr/local/libexec/jarvis/update-core-release)
    local -a tooling_temporaries=() tooling_previous=() tooling_had_previous=()
    if jq -e '.tooling.private_agents? == 1' "$release/release.json" >/dev/null 2>&1; then
        agent_tooling_present=true
        tooling_sources+=(
            "$release/jarvis-agent-bundle"
            "$release/install-agent-bundle"
            "$release/private-agent-poll"
            "$release/jarvis-private-update"
        )
        tooling_targets+=(
            /usr/local/libexec/jarvis/jarvis-agent-bundle
            /usr/local/libexec/jarvis/install-agent-bundle
            /usr/local/libexec/jarvis/private-agent-poll
            /usr/local/sbin/jarvis-private-update
        )
    fi
    if jq -e '.tooling.systemd_units? == 1' "$release/release.json" >/dev/null 2>&1; then
        tooling_sources+=("$release/manage-systemd-units" "$release/verify-home-node")
        tooling_targets+=(
            /usr/local/libexec/jarvis/manage-systemd-units
            /usr/local/libexec/jarvis/verify-home-node
        )
    fi
    app_tmp=/usr/bin/.jarvis-core-admin.new
    desktop_tmp=/usr/share/applications/.jarvis-core-admin.desktop.new
    icon_tmp=/usr/share/icons/hicolor/128x128/apps/.jarvis-core-admin.png.new
    version_tmp=/usr/share/jarvis-core-admin/.version.new
    app_previous=/usr/bin/.jarvis-core-admin.previous
    desktop_previous=/usr/share/applications/.jarvis-core-admin.desktop.previous
    icon_previous=/usr/share/icons/hicolor/128x128/apps/.jarvis-core-admin.png.previous
    version_previous=/usr/share/jarvis-core-admin/.version.previous
    install -d -o root -g root -m 0755 /usr/local/sbin /usr/local/libexec/jarvis || return 1
    for index in "${!tooling_sources[@]}"; do
        source=${tooling_sources[$index]}
        target=${tooling_targets[$index]}
        [[ -f $source && ! -L $source && -x $source ]] || return 1
        if [[ -e $target || -L $target ]]; then
            [[ -f $target && ! -L $target ]] || return 1
        fi
        temporary="${target%/*}/.${target##*/}.new"
        previous="${target%/*}/.${target##*/}.previous"
        tooling_temporaries+=("$temporary")
        tooling_previous+=("$previous")
        tooling_had_previous+=(false)
    done
    if [[ -f $release/jarvis-core-admin && ! -L $release/jarvis-core-admin ]]; then
        app_present=true
        [[ ! -e $core_admin_binary && ! -L $core_admin_binary || -f $core_admin_binary && ! -L $core_admin_binary ]] || return 1
        [[ ! -e $core_admin_desktop && ! -L $core_admin_desktop || -f $core_admin_desktop && ! -L $core_admin_desktop ]] || return 1
        [[ ! -e $core_admin_icon && ! -L $core_admin_icon || -f $core_admin_icon && ! -L $core_admin_icon ]] || return 1
        [[ ! -e $core_admin_version_file && ! -L $core_admin_version_file || -f $core_admin_version_file && ! -L $core_admin_version_file ]] || return 1
    fi
    rm -f -- "${tooling_temporaries[@]}" "${tooling_previous[@]}" \
        "$app_tmp" "$desktop_tmp" "$icon_tmp" "$version_tmp" \
        "$app_previous" "$desktop_previous" "$icon_previous" "$version_previous"
    for index in "${!tooling_sources[@]}"; do
        if ! install -o root -g root -m 0755 \
            "${tooling_sources[$index]}" "${tooling_temporaries[$index]}"; then
            rm -f -- "${tooling_temporaries[@]}" "${tooling_previous[@]}"
            return 1
        fi
        target=${tooling_targets[$index]}
        if [[ -f $target && ! -L $target ]]; then
            if ! install -o root -g root -m 0755 "$target" "${tooling_previous[$index]}"; then
                rm -f -- "${tooling_temporaries[@]}" "${tooling_previous[@]}"
                return 1
            fi
            tooling_had_previous[$index]=true
        fi
    done
    if [[ $app_present == true ]]; then
        if ! install -d -o root -g root -m 0755 /usr/share/jarvis-core-admin \
            /usr/share/applications /usr/share/icons/hicolor/128x128/apps || \
            ! install -o root -g root -m 0755 "$release/jarvis-core-admin" "$app_tmp" || \
            ! install -o root -g root -m 0644 "$release/jarvis-core-admin.desktop" "$desktop_tmp" || \
            ! install -o root -g root -m 0644 "$release/jarvis-core-admin.png" "$icon_tmp" || \
            ! install -o root -g root -m 0644 "$release/jarvis-core-admin.version" "$version_tmp"; then
            rm -f -- "${tooling_temporaries[@]}" "${tooling_previous[@]}" \
                "$app_tmp" "$desktop_tmp" "$icon_tmp" "$version_tmp"
            return 1
        fi
    fi
    if [[ $app_present == true ]]; then
        if { [[ -f $core_admin_binary ]] && \
                ! install -o root -g root -m 0755 "$core_admin_binary" "$app_previous"; } || \
            { [[ -f $core_admin_desktop ]] && \
                ! install -o root -g root -m 0644 "$core_admin_desktop" "$desktop_previous"; } || \
            { [[ -f $core_admin_icon ]] && \
                ! install -o root -g root -m 0644 "$core_admin_icon" "$icon_previous"; } || \
            { [[ -f $core_admin_version_file ]] && \
                ! install -o root -g root -m 0644 "$core_admin_version_file" "$version_previous"; }; then
            rm -f -- "${tooling_temporaries[@]}" "${tooling_previous[@]}" \
                "$app_tmp" "$desktop_tmp" "$icon_tmp" "$version_tmp" \
                "$app_previous" "$desktop_previous" "$icon_previous" "$version_previous"
            return 1
        fi
    fi

    for index in "${!tooling_targets[@]}"; do
        mv -Tf "${tooling_temporaries[$index]}" "${tooling_targets[$index]}" || {
            activation_failed=true
            break
        }
    done
    if [[ $activation_failed == false && $app_present == true ]]; then
        mv -Tf "$app_tmp" "$core_admin_binary" && \
            mv -Tf "$desktop_tmp" "$core_admin_desktop" && \
            mv -Tf "$icon_tmp" "$core_admin_icon" && \
            mv -Tf "$version_tmp" "$core_admin_version_file" || activation_failed=true
    fi
    if [[ $activation_failed == true ]]; then
        for index in "${!tooling_targets[@]}"; do
            if [[ ${tooling_had_previous[$index]} == true ]]; then
                mv -Tf "${tooling_previous[$index]}" "${tooling_targets[$index]}"
            else
                rm -f -- "${tooling_targets[$index]}"
            fi
        done
        if [[ $app_present == true ]]; then
            if [[ -f $app_previous ]]; then mv -Tf "$app_previous" "$core_admin_binary"; else rm -f -- "$core_admin_binary"; fi
            if [[ -f $desktop_previous ]]; then mv -Tf "$desktop_previous" "$core_admin_desktop"; else rm -f -- "$core_admin_desktop"; fi
            if [[ -f $icon_previous ]]; then mv -Tf "$icon_previous" "$core_admin_icon"; else rm -f -- "$core_admin_icon"; fi
            if [[ -f $version_previous ]]; then mv -Tf "$version_previous" "$core_admin_version_file"; else rm -f -- "$core_admin_version_file"; fi
        fi
        rm -f -- "${tooling_temporaries[@]}" "$app_tmp" "$desktop_tmp" "$icon_tmp" "$version_tmp"
        return 1
    fi
    rm -f -- "${tooling_previous[@]}" "$app_previous" \
        "$desktop_previous" "$icon_previous" "$version_previous"
    if [[ $app_present == true ]]; then
        if [[ -f $legacy_core_admin_desktop && ! -L $legacy_core_admin_desktop ]]; then
            rm -f -- "$legacy_core_admin_desktop"
        fi
        /usr/bin/update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
        /usr/bin/gtk-update-icon-cache --force --ignore-theme-index \
            /usr/share/icons/hicolor >/dev/null 2>&1 || true
    fi
    [[ $agent_tooling_present == false || -x /usr/local/libexec/jarvis/private-agent-poll ]]
}

restart_managed_services() {
    systemctl daemon-reload || return 1
    systemctl restart jarvis-surrealdb.service || return 1
    systemctl restart jarvis-config-broker.service || return 1
    systemctl try-restart jarvis-codex-broker.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-codex.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-opensandbox.service >/dev/null 2>&1 || true
    systemctl restart jarvis-core.service || return 1
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --retry 11 --retry-delay 5 --retry-connrefused \
        http://127.0.0.1:8080/readyz >/dev/null || return 1
    systemctl try-restart jarvis-updater.timer >/dev/null 2>&1 || true
    systemctl try-restart jarvis-private-agent-updater.timer >/dev/null 2>&1 || true
}

restore_release_transaction() {
    local previous=$1 unit_manager=$2 backup=$3 temporary_link=/opt/jarvis/.current.new
    rm -f -- "$temporary_link"
    ln -s "$previous" "$temporary_link"
    mv -Tf "$temporary_link" "$current_link"
    "$unit_manager" restore "$previous" "$backup" || return 1
    systemctl daemon-reload || return 1
    systemctl restart jarvis-surrealdb.service >/dev/null 2>&1 || return 1
    systemctl restart jarvis-config-broker.service >/dev/null 2>&1 || return 1
    systemctl try-restart jarvis-codex-broker.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-codex.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-opensandbox.service >/dev/null 2>&1 || true
    systemctl restart jarvis-core.service >/dev/null 2>&1 || return 1
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --retry 11 --retry-delay 5 --retry-connrefused \
        http://127.0.0.1:8080/readyz >/dev/null || return 1
}

activate_managed_release() {
    local release=$1 previous=$2 backup temporary_link=/opt/jarvis/.current.new
    local unit_manager="$release/manage-systemd-units"
    backup=$(mktemp -d /run/jarvis-systemd-rollback.XXXXXXXX)
    chmod 0700 "$backup"
    if ! "$unit_manager" validate-release "$release" || \
        ! "$unit_manager" install "$release" "$backup"; then
        rm -rf -- "$backup"
        return 1
    fi
    rm -f -- "$temporary_link"
    ln -s "$release" "$temporary_link"
    mv -Tf "$temporary_link" "$current_link"
    if restart_managed_services && install_versioned_tooling "$release"; then
        rm -rf -- "$backup"
        echo "jarvis updater: Core readiness and managed-unit integrity passed"
        return 0
    fi
    echo "jarvis updater: activation failed; restoring previous release and unit policy" >&2
    if ! restore_release_transaction "$previous" "$unit_manager" "$backup"; then
        echo "jarvis updater: CRITICAL: automatic release/unit restoration failed" >&2
        rm -rf -- "$backup"
        return 1
    fi
    rm -rf -- "$backup"
    return 1
}

repair_active_managed_units() {
    local release=$1 tag=$2 backup unit_manager="$release/manage-systemd-units"
    if "$unit_manager" check-installed "$release" >/dev/null 2>&1; then
        echo "jarvis updater: $tag is already active and managed systemd units match"
        return 0
    fi
    echo "jarvis updater: repairing managed systemd units for active release $tag"
    backup=$(mktemp -d /run/jarvis-systemd-rollback.XXXXXXXX)
    chmod 0700 "$backup"
    if "$unit_manager" install "$release" "$backup" && restart_managed_services && \
        install_versioned_tooling "$release"; then
        rm -rf -- "$backup"
        echo "jarvis updater: repaired managed systemd units for $tag"
        return 0
    fi
    "$unit_manager" restore "$release" "$backup" >/dev/null 2>&1 || true
    systemctl daemon-reload >/dev/null 2>&1 || true
    restart_managed_services >/dev/null 2>&1 || true
    rm -rf -- "$backup"
    fail "same-version unit repair failed; previous unit policy restored"
}

rollback() {
    local previous temporary_link
    if [[ $mode == rollback_version ]]; then
        previous=$requested_tag
        inspect_release "$previous"
        [[ $inspected_rollback_capable == true ]] || \
            fail "requested rollback release is unavailable: $previous: $inspected_reason"
    else
        previous=$(find_or_migrate_rollback_target || true)
        [[ -n $previous ]] || fail "no known verified historical release is available"
    fi
    if systemd_unit_tooling_valid "$releases_dir/$previous" && \
        [[ $release_has_managed_units == true ]]; then
        if activate_managed_release "$releases_dir/$previous" "$current_target"; then
            echo "jarvis updater: rolled back binaries and managed units to $previous"
            exit 0
        fi
        fail "rollback target failed activation; restored $current_tag"
    fi
    [[ $current_has_managed_units == false ]] || \
        fail "rollback target has no integrity-bound systemd units"
    temporary_link=/opt/jarvis/.current.new
    rm -f -- "$temporary_link"
    ln -s "$releases_dir/$previous" "$temporary_link"
    mv -Tf "$temporary_link" "$current_link"
    if systemctl restart jarvis-core.service && curl --fail --silent --show-error --connect-timeout 2 --max-time 5 --retry 11 --retry-delay 5 --retry-connrefused http://127.0.0.1:8080/readyz >/dev/null && \
        install_versioned_tooling "$releases_dir/$previous"; then
        restart_brokers
        echo "jarvis updater: rolled back to $previous"
        exit 0
    fi
    rm -f -- "$temporary_link"
    ln -s "$current_target" "$temporary_link"
    mv -Tf "$temporary_link" "$current_link"
    systemctl restart jarvis-core.service >/dev/null 2>&1 || true
    restart_brokers
    fail "rollback target or its tooling failed activation; restored $current_tag"
}

if [[ $mode == latest || $mode == version || $mode == rollback || $mode == rollback_version ]]; then
    [[ -z $current_tag ]] || migrate_legacy_release_verification "$current_target" "$current_tag"
fi

if [[ $mode == rollback_candidates ]]; then
    list_rollback_candidates
    exit 0
fi

if [[ $mode == rollback || $mode == rollback_version ]]; then
    rollback
fi

previous_tag=$(find_verified_previous || true)

metadata=$(mktemp)
remote_components=$(mktemp)
remote_components_checksum=$(mktemp)
staging_dir=
trap 'rm -f -- "$metadata" "$remote_components" "$remote_components_checksum"; cleanup' EXIT

metadata_path="/repos/$repository/releases/latest"
[[ $mode != version ]] || metadata_path="/repos/$repository/releases/tags/$requested_tag"
curl "${curl_args[@]}" \
    -H 'Accept: application/vnd.github+json' \
    "$api_url$metadata_path" > "$metadata"

tag=$(jq -er '.tag_name | strings' "$metadata") || fail "release has no tag"
draft=$(jq -r '.draft' "$metadata") || fail "release draft state is invalid"
prerelease=$(jq -r '.prerelease' "$metadata") || fail "release prerelease state is invalid"
[[ $draft == true || $draft == false ]] || fail "release draft state is invalid"
[[ $prerelease == true || $prerelease == false ]] || fail "release prerelease state is invalid"
[[ $draft == false && $prerelease == false ]] || fail "latest release is not a stable release"
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "release tag must use stable vMAJOR.MINOR.PATCH form"
echo "jarvis updater: resolved stable release $tag"
asset_url() {
    jq -er --arg name "$1" '.assets[] | select(.name == $name) | .browser_download_url' "$metadata"
}
latest_core_version=unavailable
latest_cli_version=unavailable
latest_core_admin_version=unavailable
components_asset="jarvis-core-$tag-components.json"
components_checksum="$components_asset.sha256"
components_url=$(asset_url "$components_asset" 2>/dev/null || true)
components_checksum_url=$(asset_url "$components_checksum" 2>/dev/null || true)
[[ -z $components_url && -z $components_checksum_url || -n $components_url && -n $components_checksum_url ]] || \
    fail "component manifest assets are incomplete"
if [[ -n $components_url && -n $components_checksum_url ]]; then
    [[ $components_url == https://github.com/* && $components_checksum_url == https://github.com/* ]] || \
        fail "component manifest asset URL is not a GitHub HTTPS URL"
    curl "${curl_args[@]}" "$components_url" -o "$remote_components"
    curl "${curl_args[@]}" "$components_checksum_url" -o "$remote_components_checksum"
    [[ $(wc -l < "$remote_components_checksum") -eq 1 ]] || \
        fail "component manifest checksum must contain one entry"
    read -r expected_components_sha expected_components_name extra < "$remote_components_checksum" || \
        fail "component manifest checksum is invalid"
    [[ ${#expected_components_sha} -eq 64 && $expected_components_sha != *[!0-9a-f]* && \
        $expected_components_name == "$components_asset" && -z ${extra:-} ]] || \
        fail "component manifest checksum is invalid"
    [[ $(sha256sum "$remote_components" | awk '{print $1}') == "$expected_components_sha" ]] || \
        fail "component manifest SHA-256 verification failed"
    jq -e --arg tag "$tag" \
        '.tag == $tag and (.revision | strings | test("^[0-9a-f]{40}$")) and (.components | [.core, .cli, .core_admin] | all(test("^[0-9]+\\.[0-9]+\\.[0-9]+$")))' \
        "$remote_components" >/dev/null || fail "component manifest is invalid"
    latest_core_version=$(jq -r '.components.core' "$remote_components")
    latest_cli_version=$(jq -r '.components.cli' "$remote_components")
    latest_core_admin_version=$(jq -r '.components.core_admin' "$remote_components")
fi
component_update_available=false
if valid_component_version "$latest_core_version" && \
    { ! valid_component_version "$current_core_version" || version_is_newer "v$latest_core_version" "v$current_core_version"; }; then
    component_update_available=true
fi
if valid_component_version "$latest_cli_version" && \
    { ! valid_component_version "$current_cli_version" || version_is_newer "v$latest_cli_version" "v$current_cli_version"; }; then
    component_update_available=true
fi
if valid_component_version "$latest_core_admin_version" && \
    { ! valid_component_version "$current_core_admin_version" || version_is_newer "v$latest_core_admin_version" "v$current_core_admin_version"; }; then
    component_update_available=true
fi
# Published release tags and their assets are immutable. Component metadata
# may explain why a newer bundle matters, but it can never authorize a same-tag
# replacement or bypass the existing downgrade refusal.
if [[ -n $current_tag && $current_tag == "$tag" ]] || \
    { [[ -n $current_tag ]] && ! version_is_newer "$tag" "$current_tag"; }; then
    component_update_available=false
fi
installed_unit_state=legacy-release
if [[ $current_has_managed_units == true ]]; then
    if "$current_target/manage-systemd-units" check-installed "$current_target" >/dev/null 2>&1; then
        installed_unit_state=release-matched
    else
        installed_unit_state=repair-required
    fi
fi
if [[ $mode == status ]]; then
    printf 'Current:  %s\nPrevious: %s\nLatest:   %s\nCore current: %s\nCore latest: %s\nCLI current: %s\nCLI latest: %s\nCore app current: %s\nCore app latest: %s\nSystemd units: %s\nUpdater:  %s\n' \
        "${current_tag:-unavailable}" "${previous_tag:-unavailable}" "$tag" \
        "$current_core_version" "$latest_core_version" "$current_cli_version" \
        "$latest_cli_version" "$current_core_admin_version" "$latest_core_admin_version" "$installed_unit_state" \
        "$(systemctl is-enabled jarvis-updater.timer 2>/dev/null || printf unavailable)"
    exit 0
fi
if [[ $mode == check ]]; then
    printf 'Current:  %s\nLatest:   %s\nCore current: %s\nCore latest: %s\nCLI current: %s\nCLI latest: %s\nCore app current: %s\nCore app latest: %s\nUpdate:   ' \
        "${current_tag:-unavailable}" "$tag" "$current_core_version" "$latest_core_version" \
        "$current_cli_version" "$latest_cli_version" "$current_core_admin_version" \
        "$latest_core_admin_version"
    if [[ $installed_unit_state == repair-required ]]; then printf 'repair required\n'; exit 2; fi
    if [[ $component_update_available == false && -n $current_tag && $current_tag == "$tag" ]]; then printf 'not available\n'; exit 0; fi
    if [[ $component_update_available == false && -n $current_tag ]] && ! version_is_newer "$tag" "$current_tag"; then printf 'not available\n'; exit 0; fi
    printf 'available\n'
    exit 2
fi
if [[ -n $current_tag && $current_tag == "$tag" ]]; then
    [[ -n $current_schema_sha256 ]] || fail "active release lacks a schema fingerprint; stage a tagged baseline manually before enabling automatic updates"
    if [[ $current_has_managed_units == true ]]; then
        repair_active_managed_units "$current_target" "$tag"
        exit 0
    fi
    echo "jarvis updater: $tag is already active"
    exit 0
fi
if [[ -n $current_tag ]] && ! version_is_newer "$tag" "$current_tag"; then
    if [[ $mode == latest ]]; then
        echo "jarvis updater: refusing automatic downgrade from $current_tag to $tag"
        exit 0
    fi
    fail "refusing downgrade from $current_tag to $tag; use the explicit rollback operation"
fi

artifact="jarvis-core-$tag-linux-x86_64.tar.gz"
checksum="$artifact.sha256"
artifact_url=$(asset_url "$artifact") || fail "release is missing $artifact"
checksum_url=$(asset_url "$checksum") || fail "release is missing $checksum"
[[ $artifact_url == https://github.com/* && $checksum_url == https://github.com/* ]] || \
    fail "release asset URL is not a GitHub HTTPS URL"

[[ ! -e $releases_dir/$tag ]] || fail "release directory already exists: $tag"
staging_dir=$(mktemp -d "$releases_dir/.staging.XXXXXXXX")
archive="$staging_dir/$artifact"

curl "${curl_args[@]}" "$artifact_url" -o "$archive"
curl "${curl_args[@]}" "$checksum_url" -o "$staging_dir/$checksum"
(
    cd "$staging_dir"
    sha256sum --strict --check "$checksum"
)
echo "jarvis updater: published SHA-256 verified"

# Fail closed on archive paths outside its single expected top-level directory.
expected_top="jarvis-core-$tag"
while IFS= read -r archive_path; do
    [[ $archive_path == "$expected_top" || $archive_path == "$expected_top"/* ]] || \
        fail "archive contains an unexpected path"
    [[ $archive_path != *'..'* && $archive_path != /* ]] || fail "archive path is unsafe"
done < <(tar -tzf "$archive")
while IFS= read -r archive_entry; do
    case "$archive_entry" in
        -rw*|drw*) ;;
        *) fail "archive contains a non-regular entry" ;;
    esac
done < <(tar -tvzf "$archive")
tar -xzf "$archive" --no-same-owner --no-same-permissions -C "$staging_dir"

release_dir="$staging_dir/$expected_top"
[[ -f $release_dir/jarvis-api && ! -L $release_dir/jarvis-api ]] || fail "release binary is invalid"
[[ -x $release_dir/jarvis-api ]] || fail "release binary is not executable"
[[ -x $release_dir/jarvis-agent-bundle && ! -L $release_dir/jarvis-agent-bundle ]] || \
    fail "agent-bundle validator is invalid"
[[ -x $release_dir/jarvis-config-broker && ! -L $release_dir/jarvis-config-broker ]] || \
    fail "config broker is invalid"
[[ -x $release_dir/jarvis-codex-broker && ! -L $release_dir/jarvis-codex-broker ]] || \
    fail "Codex broker is invalid"
[[ -x $release_dir/jarvis && ! -L $release_dir/jarvis ]] || \
    fail "Jarvis admin binary is invalid"
[[ -x $release_dir/update-core-release && ! -L $release_dir/update-core-release ]] || \
    fail "versioned updater helper is invalid"
find "$release_dir" -type f \( -name 'Jarvis.md' -o -path '*/agents/*' \) -print -quit | grep -q . && \
    fail "release contains protected private configuration"
[[ -f /etc/jarvis/Jarvis.md && ! -L /etc/jarvis/Jarvis.md ]] || \
    fail "protected persona is absent; use the owner-controlled private installer"
[[ -f $release_dir/release.json && ! -L $release_dir/release.json ]] || \
    fail "release manifest is invalid"
[[ $(jq -er '.tag | strings' "$release_dir/release.json") == "$tag" ]] || \
    fail "release manifest tag does not match"
jq -er '.revision | strings | test("^[0-9a-f]{40}$")' "$release_dir/release.json" >/dev/null || \
    fail "release manifest revision is invalid"
if jq -e '.tooling.private_agents? == 1' "$release_dir/release.json" >/dev/null 2>&1; then
    for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
        [[ -x $release_dir/$helper && ! -L $release_dir/$helper ]] || \
            fail "versioned private-agent tooling is incomplete"
    done
fi
admin_helper_tooling_valid "$release_dir" || fail "$admin_helper_tooling_reason"
systemd_unit_tooling_valid "$release_dir" || fail "$systemd_unit_tooling_reason"
candidate_schema_sha256=$(schema_fingerprint "$release_dir/release.json") || \
    fail "release manifest schema fingerprint is invalid"
if jq -e '.components? != null' "$release_dir/release.json" >/dev/null; then
    jq -e '.components | [.core, .cli, .core_admin] | all(test("^[0-9]+\\.[0-9]+\\.[0-9]+$"))' \
        "$release_dir/release.json" >/dev/null || fail "release component versions are invalid"
    [[ -x $release_dir/jarvis-core-admin && ! -L $release_dir/jarvis-core-admin ]] || \
        fail "graphical administrator binary is invalid"
    for app_file in jarvis-core-admin.desktop jarvis-core-admin.png jarvis-core-admin.version; do
        [[ -f $release_dir/$app_file && ! -L $release_dir/$app_file ]] || \
            fail "graphical administrator packaging is incomplete"
    done
    read -r packaged_app_version extra < "$release_dir/jarvis-core-admin.version" || \
        fail "graphical administrator version file is invalid"
    [[ -z ${extra:-} && $packaged_app_version == \
        "$(jq -r '.components.core_admin' "$release_dir/release.json")" ]] || \
        fail "graphical administrator version does not match release manifest"
    [[ $("$release_dir/jarvis-core-admin" --component-version) == "$packaged_app_version" ]] || \
        fail "graphical administrator executable version does not match release manifest"
    if [[ -s $remote_components ]]; then
        jq -e --slurpfile remote "$remote_components" \
            '.tag == $remote[0].tag and .revision == $remote[0].revision and .components == $remote[0].components' \
            "$release_dir/release.json" >/dev/null || \
            fail "archive component versions do not match published component manifest"
    fi
fi
echo "jarvis updater: archive and release manifest validated"

# A failed readiness check can roll the binary back, but it cannot safely
# reverse a schema change. Require an
# explicitly staged, tagged baseline and refuse schema changes from the timer.
[[ -n $current_schema_sha256 ]] || \
    fail "active release lacks a schema fingerprint; stage a tagged baseline manually before enabling automatic updates"
[[ $current_schema_sha256 == "$candidate_schema_sha256" ]] || \
    fail "release changes the database schema; automatic update refused, deploy manually with backup and recovery verification"

# Bind the immutable installed directory to the archive that passed the
# published SHA-256 check. Rollback never accepts an auto-installed release
# without this root-owned verification marker.
verified_sha256=$(sha256sum "$archive" | awk '{print $1}')
printf '%s  %s\n' "$verified_sha256" "$artifact" > "$release_dir/release.verification"
chown root:root "$release_dir/release.verification"
chmod 0644 "$release_dir/release.verification"

chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"
if [[ $release_has_managed_units == true ]]; then
    "$release_dir/manage-systemd-units" validate-release "$release_dir"
fi
mv --no-target-directory "$release_dir" "$releases_dir/$tag"
cleanup
staging_dir=
echo "jarvis updater: immutable release staged"

previous_target=$current_target
if [[ $release_has_managed_units == true ]]; then
    if activate_managed_release "$releases_dir/$tag" "$previous_target"; then
        echo "jarvis updater: administrative tooling and managed units activated"
        echo "jarvis updater: activated $tag"
        exit 0
    fi
    fail "rollback completed after failed managed activation; inspect sudo jarvis logs core"
fi

# Backwards-compatible path for historical releases that predate managed unit
# artifacts. New releases built by this source always use the transaction above.
temporary_link=/opt/jarvis/.current.new
rm -f -- "$temporary_link"
ln -s "$releases_dir/$tag" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
if systemctl restart jarvis-core.service && \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --retry 11 --retry-delay 5 --retry-connrefused http://127.0.0.1:8080/readyz >/dev/null && \
    install_versioned_tooling "$releases_dir/$tag"; then
    restart_brokers
    echo "jarvis updater: activated legacy-format release $tag"
    exit 0
fi
ln -s "$previous_target" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
systemctl restart jarvis-core.service >/dev/null 2>&1 || true
restart_brokers
fail "rollback completed after failed legacy-format activation; inspect sudo jarvis logs core"
