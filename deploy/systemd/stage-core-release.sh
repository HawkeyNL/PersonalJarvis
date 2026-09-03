#!/usr/bin/env bash
# Download and verify one explicit, reviewed release. This does not activate the
# release, create a tag, or use the moving "latest" endpoint.
set -euo pipefail

readonly releases_dir=/opt/jarvis/releases
readonly repository=${JARVIS_RELEASE_REPOSITORY:-HawkeyNL/PersonalJarvis}
readonly base_url=${JARVIS_RELEASE_BASE_URL:-https://github.com}

usage() { echo "Usage: sudo $0 vMAJOR.MINOR.PATCH" >&2; exit 64; }
fail() { echo "Jarvis release staging: $*" >&2; exit 1; }

validate_admin_helper_tooling() {
    local release=$1 helper metadata mode matches
    if ! jq -e '((.tooling? | type) == "object") and (.tooling | has("admin_helpers"))' \
        "$release/release.json" >/dev/null 2>&1; then
        return 0
    fi
    jq -e '(.tooling.admin_helpers | type) == "number" and .tooling.admin_helpers == 1' \
        "$release/release.json" >/dev/null 2>&1 || \
        fail "unsupported admin-helper tooling capability"
    [[ -f $release/artifact-binaries.sha256 && ! -L $release/artifact-binaries.sha256 ]] || \
        fail "admin-helper artifact checksum manifest is missing or unsafe"
    LC_ALL=C awk '
        NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ { exit 1 }
        END { if (NR == 0) exit 1 }
    ' "$release/artifact-binaries.sha256" || fail "artifact checksum manifest is malformed"
    for helper in jarvis-models jarvis-credentials; do
        [[ -f $release/$helper && ! -L $release/$helper ]] || \
            fail "versioned admin helper is missing or unsafe: $helper"
        metadata=$(stat -c '%u:%g:%a' "$release/$helper")
        [[ $metadata == 0:0:* ]] || fail "versioned admin helper is not root-owned: $helper"
        mode=${metadata##*:}
        (( (8#$mode & 0022) == 0 && (8#$mode & 0111) != 0 )) || \
            fail "versioned admin helper permissions are unsafe: $helper"
        matches=$(awk -v helper="$helper" '$2 == helper { count++ } END { print count + 0 }' \
            "$release/artifact-binaries.sha256")
        [[ $matches == 1 ]] || fail "versioned admin helper is not uniquely checksum-bound: $helper"
    done
    (cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null) || \
        fail "release artifact checksum verification failed"
}

[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ $# -eq 1 ]] || usage
tag=$1
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "tag must use stable vMAJOR.MINOR.PATCH form"
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || fail "invalid repository"
[[ $base_url == https://github.com ]] || fail "only GitHub HTTPS release assets are supported"
for command in curl jq sha256sum tar install mv find readlink; do
    command -v "$command" >/dev/null 2>&1 || fail "required command missing: $command"
done

install -d -o root -g root -m 0755 "$releases_dir"
[[ ! -e $releases_dir/$tag ]] || fail "release already staged: $tag"

netrc=${JARVIS_RELEASE_CURL_NETRC:-}
curl_args=(--fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 --connect-timeout 10 --max-time 120)
if [[ -n $netrc ]]; then
    [[ -f $netrc && ! -L $netrc ]] || fail "release netrc is not a regular file"
    [[ $(stat -c '%U:%G:%a' "$netrc") == root:root:600 ]] || fail "release netrc must be root:root mode 0600"
    curl_args+=(--netrc-file "$netrc")
fi

artifact="jarvis-core-$tag-linux-x86_64.tar.gz"
release_url="$base_url/$repository/releases/download/$tag"
staging=$(mktemp -d "$releases_dir/.staging.XXXXXXXX")
cleanup() { [[ -d ${staging:-} ]] && rm -rf -- "$staging"; }
trap cleanup EXIT

curl "${curl_args[@]}" -o "$staging/$artifact" "$release_url/$artifact"
curl "${curl_args[@]}" -o "$staging/$artifact.sha256" "$release_url/$artifact.sha256"
(
    cd "$staging"
    sha256sum --strict --check "$artifact.sha256"
)

expected_top="jarvis-core-$tag"
while IFS= read -r path; do
    [[ $path == "$expected_top" || $path == "$expected_top"/* ]] || fail "archive has an unexpected path"
    [[ $path != /* && $path != *'..'* ]] || fail "archive path is unsafe"
done < <(tar -tzf "$staging/$artifact")
while IFS= read -r entry; do
    case "$entry" in
        -rw*|drw*) ;;
        *) fail "archive contains a non-regular entry" ;;
    esac
done < <(tar -tvzf "$staging/$artifact")
tar -xzf "$staging/$artifact" --no-same-owner --no-same-permissions -C "$staging"

release_dir="$staging/$expected_top"
[[ -x $release_dir/jarvis-api && ! -L $release_dir/jarvis-api ]] || fail "release binary is invalid"
[[ -x $release_dir/jarvis-config-broker && ! -L $release_dir/jarvis-config-broker ]] || fail "config broker is invalid"
[[ -x $release_dir/jarvis-codex-broker && ! -L $release_dir/jarvis-codex-broker ]] || fail "Codex broker is invalid"
[[ -x $release_dir/jarvis-agent-bundle && ! -L $release_dir/jarvis-agent-bundle ]] || fail "agent-bundle validator is invalid"
[[ -x $release_dir/jarvis && ! -L $release_dir/jarvis ]] || fail "Jarvis admin binary is invalid"
[[ -x $release_dir/update-core-release && ! -L $release_dir/update-core-release ]] || \
    fail "versioned updater helper is invalid"
# The protected persona is supplied separately from the private owner checkout.
# A public software release must never contain Jarvis.md or agent definitions.
find "$release_dir" -type f \( -name 'Jarvis.md' -o -path '*/agents/*' \) -print -quit | grep -q . && \
    fail "release contains protected private configuration"
[[ -f $release_dir/release.json && ! -L $release_dir/release.json ]] || fail "release manifest is invalid"
jq -e --arg tag "$tag" '
    .tag == $tag and
    (.revision | strings | test("^[0-9a-f]{40}$")) and
    (.schema_sha256 | strings | test("^[0-9a-f]{64}$"))
' "$release_dir/release.json" >/dev/null || fail "release manifest does not bind this tag, revision and schema"
if jq -e '.tooling.private_agents? == 1' "$release_dir/release.json" >/dev/null 2>&1; then
    for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
        [[ -x $release_dir/$helper && ! -L $release_dir/$helper ]] || \
            fail "versioned private-agent tooling is incomplete"
    done
fi
validate_admin_helper_tooling "$release_dir"
if jq -e '((.tooling? | type) == "object") and (.tooling | has("systemd_units"))' \
    "$release_dir/release.json" >/dev/null 2>&1; then
    jq -e '(.tooling.systemd_units | type) == "number" and .tooling.systemd_units == 1' \
        "$release_dir/release.json" >/dev/null || fail "unsupported managed-systemd capability"
    [[ -x $release_dir/manage-systemd-units && ! -L $release_dir/manage-systemd-units ]] || \
        fail "managed-systemd helper is invalid"
    "$release_dir/manage-systemd-units" validate-artifacts "$release_dir"
fi
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
fi
find "$release_dir" -xdev -type l -print -quit | grep -q . && fail "release contains a symlink"
sha256sum "$staging/$artifact" > "$release_dir/release.verification"
chown root:root "$release_dir/release.verification"
chmod 0644 "$release_dir/release.verification"

chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"
mv --no-target-directory "$release_dir" "$releases_dir/$tag"
staging=
trap - EXIT
echo "Jarvis release staging: verified and staged $tag. Activate it only with install-home-node-core.sh."
