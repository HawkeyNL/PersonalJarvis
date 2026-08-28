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
repository=
github_curl_netrc=

usage() {
    cat >&2 <<'EOF'
Usage: update-core-release [--latest|--version vMAJOR.MINOR.PATCH|--check|--status|--rollback]

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
    local line key value seen_repository=false
    if [[ ! -e $updater_config ]]; then
        # v0.0.10 did not persist an update source. This one-time migration is
        # intentionally the public canonical repository, never caller input.
        write_default_updater_config
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

schema_fingerprint() {
    jq -er '.schema_sha256 | strings | select(test("^[0-9a-f]{64}$"))' "$1"
}

cleanup() {
    [[ -n ${staging_dir:-} && -d ${staging_dir:-} ]] || return 0
    case "$staging_dir" in
        "$releases_dir"/.staging.*) rm -rf -- "$staging_dir" ;;
        *) fail "refusing to remove unexpected staging directory" ;;
    esac
}

for command in awk curl flock jq sha256sum tar systemctl readlink mv ln mktemp install; do
    require_command "$command"
done

[[ ${EUID} -eq 0 ]] || fail "must run as root"
load_updater_config
[[ -d $releases_dir && -L $current_link ]] || fail "expected Jarvis release layout is absent"

mode=latest
requested_tag=
case ${1:-} in
    '') ;;
    --latest) [[ $# == 1 ]] || usage ;;
    --version) [[ $# == 2 && $2 =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage; mode=version; requested_tag=$2 ;;
    --check) [[ $# == 1 ]] || usage; mode=check ;;
    --status) [[ $# == 1 ]] || usage; mode=status ;;
    --rollback) [[ $# == 1 ]] || usage; mode=rollback ;;
    *) usage ;;
esac

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
if [[ -f $current_target/release.json && ! -L $current_target/release.json ]]; then
    current_tag=$(jq -er '.tag | strings' "$current_target/release.json" 2>/dev/null || true)
    [[ $current_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
        fail "installed release manifest has an unsafe tag"
    current_schema_sha256=$(schema_fingerprint "$current_target/release.json" 2>/dev/null || true)
fi
previous_tag=$(find "$releases_dir" -mindepth 1 -maxdepth 1 -type d -name 'v*' ! -samefile "$current_target" -printf '%f\n' | LC_ALL=C sort -V | tail -n 1)

restart_brokers() {
    systemctl try-restart jarvis-config-broker.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-codex-broker.service >/dev/null 2>&1 || true
}

install_versioned_tooling() {
    local release=$1 admin_tmp updater_tmp admin_previous updater_previous
    admin_tmp=/usr/local/sbin/.jarvis.new
    updater_tmp=/usr/local/libexec/jarvis/.update-core-release.new
    admin_previous=/usr/local/sbin/.jarvis.previous
    updater_previous=/usr/local/libexec/jarvis/.update-core-release.previous
    rm -f -- "$admin_tmp" "$updater_tmp" "$admin_previous" "$updater_previous"
    install -o root -g root -m 0755 "$release/jarvis" "$admin_tmp" || return 1
    install -o root -g root -m 0755 "$release/update-core-release" "$updater_tmp" || return 1
    [[ -f /usr/local/sbin/jarvis && ! -L /usr/local/sbin/jarvis ]] && \
        install -o root -g root -m 0755 /usr/local/sbin/jarvis "$admin_previous"
    [[ -f /usr/local/libexec/jarvis/update-core-release && ! -L /usr/local/libexec/jarvis/update-core-release ]] && \
        install -o root -g root -m 0755 /usr/local/libexec/jarvis/update-core-release "$updater_previous"
    if ! mv -Tf "$updater_tmp" /usr/local/libexec/jarvis/update-core-release; then
        rm -f -- "$admin_tmp" "$updater_tmp" "$admin_previous" "$updater_previous"
        return 1
    fi
    if ! mv -Tf "$admin_tmp" /usr/local/sbin/jarvis; then
        if [[ -f $updater_previous ]]; then
            mv -Tf "$updater_previous" /usr/local/libexec/jarvis/update-core-release || return 1
        else
            rm -f -- /usr/local/libexec/jarvis/update-core-release
        fi
        rm -f -- "$admin_tmp" "$updater_tmp" "$admin_previous" "$updater_previous"
        return 1
    fi
    rm -f -- "$admin_previous" "$updater_previous"
}

rollback() {
    local previous temporary_link
    previous=$(find "$releases_dir" -mindepth 1 -maxdepth 1 -type d -name 'v*' ! -samefile "$current_target" -printf '%f\n' | LC_ALL=C sort -V | tail -n 1)
    [[ -n $previous && -d $releases_dir/$previous && ! -L $releases_dir/$previous ]] || fail "no known verified historical release is available"
    [[ -f $releases_dir/$previous/release.json && -f $releases_dir/$previous/release.verification ]] || fail "historical release is not verified"
    jq -e --arg tag "$previous" '.tag == $tag and (.schema_sha256 | strings | test("^[0-9a-f]{64}$"))' "$releases_dir/$previous/release.json" >/dev/null || fail "historical release manifest is invalid"
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

[[ $mode != rollback ]] || rollback

metadata=$(mktemp)
staging_dir=
trap 'rm -f -- "$metadata"; cleanup' EXIT

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
if [[ $mode == status ]]; then
    printf 'Current:  %s\nPrevious: %s\nLatest:   %s\nUpdater:  %s\n' "${current_tag:-unavailable}" "${previous_tag:-unavailable}" "$tag" "$(systemctl is-enabled jarvis-updater.timer 2>/dev/null || printf unavailable)"
    exit 0
fi
if [[ $mode == check ]]; then
    printf 'Current:  %s\nLatest:   %s\nUpdate:   ' "${current_tag:-unavailable}" "$tag"
    if [[ -n $current_tag && $current_tag == "$tag" ]]; then printf 'not available\n'; exit 0; fi
    if [[ -n $current_tag ]] && ! version_is_newer "$tag" "$current_tag"; then printf 'not available\n'; exit 0; fi
    printf 'available\n'
    exit 2
fi
if [[ -n $current_tag && $current_tag == "$tag" ]]; then
    [[ -n $current_schema_sha256 ]] || fail "active release lacks a schema fingerprint; stage a tagged baseline manually before enabling automatic updates"
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
asset_url() {
    jq -er --arg name "$1" '.assets[] | select(.name == $name) | .browser_download_url' "$metadata"
}
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
candidate_schema_sha256=$(schema_fingerprint "$release_dir/release.json") || \
    fail "release manifest schema fingerprint is invalid"
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
mv --no-target-directory "$release_dir" "$releases_dir/$tag"
cleanup
staging_dir=
echo "jarvis updater: immutable release staged"

previous_target=$current_target
temporary_link=/opt/jarvis/.current.new
rm -f -- "$temporary_link"
ln -s "$releases_dir/$tag" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"

if systemctl restart jarvis-core.service && \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --retry 11 --retry-delay 5 --retry-connrefused \
        http://127.0.0.1:8080/readyz >/dev/null; then
    echo "jarvis updater: Core readiness passed"
    if install_versioned_tooling "$releases_dir/$tag"; then
        echo "jarvis updater: administrative tooling activated"
        restart_brokers
        echo "jarvis updater: activated $tag"
        exit 0
    fi
fi

echo "jarvis updater: $tag failed readiness or tooling activation; restoring previous release" >&2
ln -s "$previous_target" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
systemctl restart jarvis-core.service || true
restart_brokers
fail "rollback completed after failed activation; inspect journalctl -u jarvis-core"
