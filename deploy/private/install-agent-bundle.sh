#!/usr/bin/env bash
# Build and atomically activate a read-only runtime bundle from private Markdown
# profiles. Profile text may request no authority: effective capabilities remain
# the intersection enforced by public Core policy at runtime.
set -euo pipefail

readonly bundle_root=/var/lib/jarvis/agents
readonly releases_dir="$bundle_root/releases"
readonly current_link="$bundle_root/current"
readonly validator=${JARVIS_AGENT_BUNDLE_VALIDATOR:-/usr/local/libexec/jarvis/jarvis-agent-bundle}
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
[[ -x $validator && ! -L $validator ]] || fail "validated agent-bundle helper is unavailable"

source_root=$(realpath -e -- "$source_root") || fail "private checkout does not exist"
source_agents="$source_root/agents"
[[ -d $source_agents && ! -L $source_agents ]] || fail "agents directory is missing or unsafe"
source_symlink=$(find "$source_agents" -maxdepth 1 -type l -print -quit)
[[ -z $source_symlink ]] || fail "agent source contains a symlink"

# Core may traverse and read immutable definitions, but never owns or writes
# the activation tree.  The parent /var/lib/jarvis remains Core-owned for
# legitimate runtime state; this protected subtree is root-owned.
install -d -o root -g jarvis -m 0750 "$bundle_root" "$releases_dir"
stage=$(mktemp -d "$releases_dir/.staging.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
mkdir -p "$stage/agents"

count=0
declare -A profile_lines_by_id=()
declare -A source_updated_at_by_id=()
source_is_git=false
if git -C "$source_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    source_is_git=true
fi
while IFS= read -r -d '' source_file; do
    [[ -f $source_file && ! -L $source_file ]] || fail "agent source is unsafe"
    base=${source_file##*/}
    [[ $base =~ ^(0[1-9]|1[0-9])_[A-Za-z0-9_]+\.md$ ]] || continue
    target="$stage/agents/${base%.md}.json"
    "$validator" validate "$source_file" "$target" || fail "agent definition failed validation"
    id=$(jq -er '.id | strings | select(test("^[A-Za-z0-9_-]{1,64}$"))' "$target") || fail "validated agent has unsafe ID"
    profile_lines=$(awk 'END { print NR }' "$source_file")
    [[ $profile_lines =~ ^[0-9]+$ && $profile_lines -gt 0 && $profile_lines -le 100000 ]] \
        || fail "agent profile has an invalid line count"
    profile_lines_by_id["$id"]=$profile_lines
    if [[ $source_is_git == true ]]; then
        relative_source=${source_file#"$source_root/"}
        source_updated_at=$(git -C "$source_root" log -1 --format=%cI -- "$relative_source")
        if [[ -n $source_updated_at ]]; then
            source_updated_at_by_id["$id"]=$source_updated_at
        fi
    fi
    mv -- "$target" "$stage/agents/$id.json"
    ((count += 1))
done < <(find "$source_agents" -maxdepth 1 -type f -name '*.md' -print0 | LC_ALL=C sort -z)
((count > 0)) || fail "no numbered private agent profiles found"

manifest_entries=$(for file in "$stage"/agents/*.json; do
    id=${file##*/}
    id=${id%.json}
    hash=$(sha256sum "$file" | awk '{print $1}')
    safe_metadata=$(jq -ec '
        . as $agent
        | ($agent.name | strings | select(length > 0 and length <= 80)) as $name
        | ($agent.model_policy | strings | select(test("^(fast|utility|default|standard|strong|frontier|coding|trading|research)$"; "i"))) as $model_policy
        | (($agent.group // null) | if . == null then null else strings | select(length > 0 and length <= 80) end) as $group
        | {name:$name,group:$group,model_policy:$model_policy}
    ' "$file") || fail "validated agent has unsafe presentation metadata"
    jq -n --arg id "${file##*/}" --arg path "agents/${file##*/}" --arg sha256 "$hash" \
        --argjson profile_lines "${profile_lines_by_id[$id]}" \
        --arg source_updated_at "${source_updated_at_by_id[$id]:-}" \
        --argjson metadata "$safe_metadata" \
        '{id:($id | rtrimstr(".json")),path:$path,sha256:$sha256}
         + $metadata
         + {profile_lines:$profile_lines}
         + (if $source_updated_at == "" then {} else {source_updated_at:$source_updated_at} end)
         | with_entries(select(.value != null))'
done | jq -s '.')
bundle_hash=$(printf '%s' "$manifest_entries" | sha256sum | awk '{print $1}')
bundle_id="bundle-${bundle_hash:0:16}"
jq -n --arg bundle_id "$bundle_id" --argjson agents "$manifest_entries" \
    '{version:1,bundle_id:$bundle_id,agents:$agents}' > "$stage/manifest.json"

final_dir="$releases_dir/$bundle_id"
if [[ -e $final_dir ]]; then
    [[ -d $final_dir && ! -L $final_dir ]] || fail "existing bundle path is unsafe"
    # Repair only root-controlled metadata on a known immutable release.  This
    # is needed when upgrading a v0.0.7 Home Node whose otherwise valid bundle
    # was too restrictive for the service user to traverse.
    chown -R root:jarvis "$final_dir"
    find "$final_dir" -type d -exec chmod 0750 {} +
    find "$final_dir" -type f -exec chmod 0640 {} +
    rm -rf -- "$stage"
    trap - EXIT
    echo "UNCHANGED private agent bundle $bundle_id ($count agents)"
else
    # Complete all validation before making the staged tree available to the
    # service group.  Files are read-only and every directory is only
    # traversable/readable, so the atomic rename never exposes a writable
    # active bundle to jarvis.
    chown -R root:jarvis "$stage"
    find "$stage" -type d -exec chmod 0750 {} +
    find "$stage" -type f -exec chmod 0640 {} +
    mv --no-target-directory "$stage" "$final_dir"
    trap - EXIT
    echo "CREATE private agent bundle $bundle_id ($count agents)"
fi
temporary_link="$bundle_root/.current.new"
rm -f -- "$temporary_link"
ln -s "releases/$bundle_id" "$temporary_link"
mv -Tf "$temporary_link" "$current_link"
echo "UPDATE private agent bundle activated"
