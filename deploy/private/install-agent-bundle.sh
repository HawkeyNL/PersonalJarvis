#!/usr/bin/env bash
# Build and atomically activate a read-only runtime bundle from private Markdown
# profiles. Profile text may request no authority: effective capabilities remain
# the intersection enforced by public Core policy at runtime.
set -euo pipefail

readonly bundle_root=/var/lib/jarvis/agents
readonly releases_dir="$bundle_root/releases"
readonly current_link="$bundle_root/current"
fail() { echo "private agents: $*" >&2; exit 1; }
usage() { echo "Usage: sudo $0 --source /path/to/PersonalJarvisAgents" >&2; exit 64; }

source_root=
while (($#)); do
    case "$1" in
        --source) source_root=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ -n $source_root ]] || usage
for command in jq sha256sum find mktemp realpath; do command -v "$command" >/dev/null 2>&1 || fail "$command is required"; done

source_root=$(realpath -e -- "$source_root") || fail "private checkout does not exist"
source_agents="$source_root/agents"
[[ -d $source_agents && ! -L $source_agents ]] || fail "agents directory is missing or unsafe"
find "$source_agents" -maxdepth 1 -type l -print -quit | grep -q . && \
    fail "agent source contains a symlink"

install -d -o root -g root -m 0755 "$bundle_root" "$releases_dir"
stage=$(mktemp -d "$releases_dir/.staging.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/agents"

count=0
while IFS= read -r -d '' source_file; do
    [[ -f $source_file && ! -L $source_file ]] || fail "agent source is unsafe"
    base=${source_file##*/}
    [[ $base =~ ^[0-9]{2}_[A-Za-z0-9_]+\.md$ ]] || continue
    id=$(printf '%s' "${base%.md}" | tr '[:upper:]_' '[:lower:]-')
    target="$stage/agents/$id.json"
    jq -n --rawfile instructions "$source_file" \
        --arg id "$id" \
        --arg name "${base%.md}" \
        '{id:$id,name:$name,description:"Private owner-provided agent profile.",model_policy:"default",instructions:$instructions,requested_capabilities:[],allowed_tools:[],denied_actions:[],limits:{max_runtime_seconds:300,max_context_chars:20000,max_output_chars:12000,max_parallel_runs:1}}' \
        > "$target"
    ((count += 1))
done < <(find "$source_agents" -maxdepth 1 -type f -name '*.md' -print0 | LC_ALL=C sort -z)
((count > 0)) || fail "no numbered private agent profiles found"

manifest_entries=$(for file in "$stage"/agents/*.json; do
    hash=$(sha256sum "$file" | awk '{print $1}')
    jq -n --arg id "${file##*/}" --arg path "agents/${file##*/}" --arg sha256 "$hash" \
        '{id:($id | rtrimstr(".json")),path:$path,sha256:$sha256}'
done | jq -s '.')
bundle_hash=$(printf '%s' "$manifest_entries" | sha256sum | awk '{print $1}')
bundle_id="bundle-${bundle_hash:0:16}"
jq -n --arg bundle_id "$bundle_id" --argjson agents "$manifest_entries" \
    '{version:1,bundle_id:$bundle_id,agents:$agents}' > "$stage/manifest.json"

final_dir="$releases_dir/$bundle_id"
if [[ -e $final_dir ]]; then
    rm -rf -- "$stage"
    trap - EXIT
    echo "UNCHANGED private agent bundle $bundle_id ($count agents)"
else
    chown -R root:root "$stage"
    chmod -R go-w "$stage"
    mv --no-target-directory "$stage" "$final_dir"
    trap - EXIT
    echo "CREATE private agent bundle $bundle_id ($count agents)"
fi
temporary_link="$bundle_root/.current.new"
rm -f -- "$temporary_link"
ln -s "releases/$bundle_id" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
echo "UPDATE private agent bundle activated"
