#!/usr/bin/env bash
# Rootless behavioral coverage for the canonical release packager. The costly
# canonical-builder stage is covered by release CI; this fixture proves that
# package mode publishes only a complete, checksum-bound admin-helper bundle.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
builder="$repo_dir/scripts/release/build-linux.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT

revision=0123456789abcdef0123456789abcdef01234567

write_candidate() {
    local tag=$1 release="$fixture/candidate/jarvis-core-$1" helper
    mkdir -p "$release"
    for helper in jarvis-models jarvis-credentials; do
        printf '#!/usr/bin/env bash\nprintf "%s fixture\\n"\n' "$helper" > "$release/$helper"
        chmod 0755 "$release/$helper"
    done
    jq -n --arg tag "$tag" --arg revision "$revision" \
        '{tag:$tag,revision:$revision,components:{core:"0.1.0",cli:"0.1.1",core_admin:"0.1.1"},tooling:{private_agents:1,admin_helpers:1}}' \
        > "$release/release.json"
    (
        cd "$release"
        sha256sum jarvis-models jarvis-credentials > artifact-binaries.sha256
    )
}

tag=v9.8.7
write_candidate "$tag"
bash "$builder" package "$tag" "$revision" "$fixture"
archive="$fixture/jarvis-core-$tag-linux-x86_64.tar.gz"
[[ -f $archive ]]
tar -tzf "$archive" | grep -qx "jarvis-core-$tag/jarvis-models"
tar -tzf "$archive" | grep -qx "jarvis-core-$tag/jarvis-credentials"
extracted="$fixture/extracted"
mkdir -p "$extracted"
tar -xzf "$archive" -C "$extracted"
cmp "$fixture/candidate/jarvis-core-$tag/jarvis-models" \
    "$extracted/jarvis-core-$tag/jarvis-models"
cmp "$fixture/candidate/jarvis-core-$tag/jarvis-credentials" \
    "$extracted/jarvis-core-$tag/jarvis-credentials"
jq -e '.tooling.admin_helpers == 1' \
    "$extracted/jarvis-core-$tag/release.json" >/dev/null

bad_tag=v9.8.8
write_candidate "$bad_tag"
rm -f -- "$fixture/candidate/jarvis-core-$bad_tag/jarvis-credentials"
if bash "$builder" package "$bad_tag" "$revision" "$fixture" \
    >"$fixture/bad.stdout" 2>"$fixture/bad.stderr"; then
    echo "release packager accepted a missing declared admin helper" >&2
    exit 1
fi
grep -Fq 'release candidate is missing executable jarvis-credentials' "$fixture/bad.stderr" || {
    echo "release packager did not explain the missing admin helper" >&2
    cat "$fixture/bad.stderr" >&2
    exit 1
}

echo "Canonical release package fixture tests passed"
