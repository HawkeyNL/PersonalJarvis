#!/usr/bin/env bash
# Download and verify one explicit, reviewed release. This does not activate the
# release, create a tag, or use the moving "latest" endpoint.
set -euo pipefail

readonly releases_dir=/opt/jarvis/releases
readonly repository=${JARVIS_RELEASE_REPOSITORY:-HawkeyNL/PersonalJarvis}
readonly base_url=${JARVIS_RELEASE_BASE_URL:-https://github.com}

usage() { echo "Usage: sudo $0 vMAJOR.MINOR.PATCH" >&2; exit 64; }
fail() { echo "Jarvis release staging: $*" >&2; exit 1; }

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
