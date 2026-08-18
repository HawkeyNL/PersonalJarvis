#!/usr/bin/env bash
# Download and activate one verified Jarvis Core GitHub release. This is a
# root-operated systemd helper, not an API or agent capability.
set -euo pipefail

readonly releases_dir=/opt/jarvis/releases
readonly current_link=/opt/jarvis/current
readonly lock_file=/run/jarvis-updater.lock
readonly api_url="${JARVIS_GITHUB_API_URL:-https://api.github.com}"
readonly repository="${JARVIS_UPDATE_REPOSITORY:?JARVIS_UPDATE_REPOSITORY is required}"

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

for command in curl flock jq sha256sum tar systemctl readlink mv ln mktemp install; do
    require_command "$command"
done

[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid repository"
[[ $api_url == https://api.github.com ]] || fail "only the GitHub API endpoint is supported"
[[ -d $releases_dir && -L $current_link ]] || fail "expected Jarvis release layout is absent"

# Optional private-repository access is supplied through a root-only curl netrc
# file, never through the unit, process environment, or an argument.
curl_args=(--fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 10 --max-time 60)
if [[ -n ${JARVIS_GITHUB_CURL_NETRC:-} ]]; then
    [[ -f $JARVIS_GITHUB_CURL_NETRC ]] || fail "configured curl netrc does not exist"
    [[ $(stat -c '%U:%a' "$JARVIS_GITHUB_CURL_NETRC") == root:600 ]] || \
        fail "curl netrc must be root-owned with mode 0600"
    curl_args+=(--netrc-file "$JARVIS_GITHUB_CURL_NETRC")
fi

exec 9>"$lock_file"
flock -n 9 || { echo "jarvis updater: another update is running"; exit 0; }

metadata=$(mktemp)
staging_dir=
trap 'rm -f -- "$metadata"; cleanup' EXIT

curl "${curl_args[@]}" \
    -H 'Accept: application/vnd.github+json' \
    "$api_url/repos/$repository/releases/latest" > "$metadata"

tag=$(jq -er '.tag_name | strings' "$metadata") || fail "release has no tag"
draft=$(jq -r '.draft' "$metadata") || fail "release draft state is invalid"
prerelease=$(jq -r '.prerelease' "$metadata") || fail "release prerelease state is invalid"
[[ $draft == true || $draft == false ]] || fail "release draft state is invalid"
[[ $prerelease == true || $prerelease == false ]] || fail "release prerelease state is invalid"
[[ $draft == false && $prerelease == false ]] || fail "latest release is not a stable release"
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "release tag must use stable vMAJOR.MINOR.PATCH form"

current_target=$(readlink -f -- "$current_link")
[[ $current_target == "$releases_dir"/* ]] || fail "current release is outside the release directory"
current_tag=
current_schema_sha256=
if [[ -f $current_target/release.json && ! -L $current_target/release.json ]]; then
    current_tag=$(jq -er '.tag | strings' "$current_target/release.json" 2>/dev/null || true)
    [[ $current_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || \
        fail "installed release manifest has an unsafe tag"
    current_schema_sha256=$(schema_fingerprint "$current_target/release.json" 2>/dev/null || true)
    if [[ $current_tag == "$tag" ]]; then
        [[ -n $current_schema_sha256 ]] || \
            fail "active release lacks a schema fingerprint; stage a tagged baseline manually before enabling automatic updates"
        echo "jarvis updater: $tag is already active"
        exit 0
    fi
    version_is_newer "$tag" "$current_tag" || {
        echo "jarvis updater: refusing automatic downgrade from $current_tag to $tag"
        exit 0
    }
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
[[ -f $release_dir/core/Jarvis.md && ! -L $release_dir/core/Jarvis.md ]] || \
    fail "Core persona is invalid"
[[ -f $release_dir/release.json && ! -L $release_dir/release.json ]] || \
    fail "release manifest is invalid"
[[ $(jq -er '.tag | strings' "$release_dir/release.json") == "$tag" ]] || \
    fail "release manifest tag does not match"
jq -er '.revision | strings | test("^[0-9a-f]{40}$")' "$release_dir/release.json" >/dev/null || \
    fail "release manifest revision is invalid"
candidate_schema_sha256=$(schema_fingerprint "$release_dir/release.json") || \
    fail "release manifest schema fingerprint is invalid"

# A failed readiness check can roll the binary back, but it cannot safely
# reverse a schema change. Require an
# explicitly staged, tagged baseline and refuse schema changes from the timer.
[[ -n $current_schema_sha256 ]] || \
    fail "active release lacks a schema fingerprint; stage a tagged baseline manually before enabling automatic updates"
[[ $current_schema_sha256 == "$candidate_schema_sha256" ]] || \
    fail "release changes the database schema; automatic update refused, deploy manually with backup and recovery verification"

chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"
mv --no-target-directory "$release_dir" "$releases_dir/$tag"
cleanup
staging_dir=

previous_target=$current_target
temporary_link=/opt/jarvis/.current.new
rm -f -- "$temporary_link"
ln -s "$releases_dir/$tag" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"

if systemctl restart jarvis-core.service && \
    curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --retry 11 --retry-delay 5 --retry-connrefused \
        http://127.0.0.1:8080/readyz >/dev/null; then
    echo "jarvis updater: activated $tag"
    exit 0
fi

echo "jarvis updater: $tag failed readiness; restoring previous release" >&2
ln -s "$previous_target" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
systemctl restart jarvis-core.service || true
fail "rollback completed after failed readiness; inspect journalctl -u jarvis-core"
