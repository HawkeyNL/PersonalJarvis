#!/usr/bin/env bash
# Root-only synthetic fixture. It contains no owner content and is limited to
# GitHub Actions because it deliberately exercises canonical system paths.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || { echo "CI root fixture only" >&2; exit 1; }
[[ -x ${JARVIS_PERSONA_VALIDATOR:-} ]] || { echo "set JARVIS_PERSONA_VALIDATOR to the built jarvis binary" >&2; exit 1; }

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
cleanup() {
    rm -rf -- "$fixture" /etc/jarvis /var/lib/jarvis/agents
    if [[ ${created_user:-false} == true ]]; then userdel jarvis 2>/dev/null || true; fi
}
trap cleanup EXIT
# The persona installer checks content with the real `jarvis` validator, which
# it accepts only as a root-owned file nobody else can write.
install -d -o root -g root -m 0755 "$fixture/bin"
install -o root -g root -m 0755 "$JARVIS_PERSONA_VALIDATOR" "$fixture/bin/jarvis"
export JARVIS_PERSONA_VALIDATOR="$fixture/bin/jarvis"

created_user=false
if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin jarvis
    created_user=true
fi
mkdir -p "$fixture/private/personaljarvis/jarvis-core" "$fixture/private/agents"
printf '# Synthetic owner persona\n' > "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
cat > "$fixture/private/agents/01_TEST_AGENT.md" <<'EOF'
---
schema_version: 1
id: test-agent
name: Test Agent
description: Synthetic fixture
model_policy: default
max_runtime_seconds: 30
max_context_chars: 1000
max_output_chars: 500
max_parallel_runs: 1
---

Synthetic instructions.
EOF
cat > "$fixture/validator" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == validate && $# == 3 ]] || exit 1
jq -n --rawfile instructions "$2" '{id:"test-agent",name:"Test Agent",description:"Synthetic fixture",model_policy:"default",instructions:$instructions,requested_capabilities:[],allowed_tools:[],denied_actions:[],limits:{max_runtime_seconds:30,max_context_chars:1000,max_output_chars:500,max_parallel_runs:1}}' > "$3"
EOF
chmod 0755 "$fixture/validator"
git -C "$fixture/private" init --quiet
git -C "$fixture/private" config user.name "Jarvis CI"
git -C "$fixture/private" config user.email "jarvis-ci@example.invalid"
git -C "$fixture/private" add agents/01_TEST_AGENT.md
GIT_AUTHOR_DATE=2026-08-29T12:32:00Z GIT_COMMITTER_DATE=2026-08-29T12:32:00Z \
    git -C "$fixture/private" commit --quiet -m "Add synthetic agent"

install -d -o root -g root -m 0750 /etc/jarvis
bash "$repo_dir/deploy/private/install-private-config.sh" --source "$fixture/private"
[[ $(stat -c '%U:%G:%a' /etc/jarvis) == root:jarvis:750 ]]
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]]
runuser -u jarvis -- test -r /etc/jarvis/Jarvis.md
runuser -u jarvis -- test ! -w /etc/jarvis/Jarvis.md
before=$(sha256sum /etc/jarvis/Jarvis.md | awk '{print $1}')
rm "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
ln -s /etc/passwd "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
if bash "$repo_dir/deploy/private/install-private-config.sh" --source "$fixture/private"; then
    echo "persona symlink was accepted" >&2
    exit 1
fi
[[ $(sha256sum /etc/jarvis/Jarvis.md | awk '{print $1}') == "$before" ]]

installer="$repo_dir/deploy/private/install-private-config.sh"
history=/etc/jarvis/persona-history
persona="$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
installed_hash() { sha256sum /etc/jarvis/Jarvis.md | awk '{print $1}'; }
write_persona() {
    printf '# Synthetic persona %s\n' "$1" > "$persona"
    sha256sum "$persona" | awk '{print $1}'
}
rm "$persona"
v1=$(write_persona 1)
# --expect-sha256 binds the installed bytes to the version validated earlier.
if bash "$installer" --source "$fixture/private" --expect-sha256 "$(printf '0%.0s' {1..64})"; then
    echo "persona with an unexpected hash was installed" >&2
    exit 1
fi
[[ $(installed_hash) == "$before" ]]
bash "$installer" --source "$fixture/private" --expect-sha256 "$v1"
[[ $(installed_hash) == "$v1" ]]
# The replaced version is archived for rollback.
[[ $(stat -c '%U:%G:%a' "$history") == root:root:700 ]]
[[ $(stat -c '%U:%G:%a' "$history/$before") == root:jarvis:640 ]]
[[ $(bash "$installer" --source "$fixture/private") == "UNCHANGED protected persona sha256=${v1:0:12}" ]]
# Only the ten most recently used versions are kept, the installed one included.
for version in $(seq 2 13); do
    latest=$(write_persona "$version")
    [[ $version != 11 ]] || v11=$latest
    [[ $version != 12 ]] || v12=$latest
    bash "$installer" --source "$fixture/private" >/dev/null
done
[[ $(find "$history" -mindepth 1 -type f | wc -l) == 10 ]]
[[ -f $history/$latest && ! -e $history/$v1 && ! -e $history/$before ]]
# Restore selects an archived version by hash only.
[[ $(bash "$installer" --restore "$v12") == "RESTORE protected persona sha256=${v12:0:12}" ]]
[[ $(installed_hash) == "$v12" ]]
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]]
[[ -f $history/$latest && $(find "$history" -mindepth 1 -type f | wc -l) == 10 ]]
printf 'tampered\n' >> "$history/$v11"
for arguments in "--restore $v11" "--restore $(printf '0%.0s' {1..64})" "--restore ../Jarvis.md" \
    "--restore $latest --source $fixture/private" "--restore $latest --expect-sha256 $latest"; do
    # shellcheck disable=SC2086 # deliberate word splitting of fixed arguments
    if bash "$installer" $arguments 2>/dev/null; then
        echo "persona restore accepted: $arguments" >&2
        exit 1
    fi
done
[[ $(installed_hash) == "$v12" ]]
# A damaged archive of the replaced version is rewritten from the installed file.
printf 'damaged\n' > "$history/$v12"
v14=$(write_persona 14)
bash "$installer" --source "$fixture/private" >/dev/null
[[ $(sha256sum "$history/$v12" | awk '{print $1}') == "$v12" ]]
# Pruning keeps the installed and the replaced version even when other entries
# look newer, for example after the clock moved backwards.
find "$history" -mindepth 1 -type f -exec touch -d '2099-01-01' {} +
v15=$(write_persona 15)
bash "$installer" --source "$fixture/private" >/dev/null
[[ -f $history/$v14 && -f $history/$v15 && $(installed_hash) == "$v15" ]]

# An identical persona with an unsafe mode is reinstalled, not left as is.
chmod 0660 /etc/jarvis/Jarvis.md
[[ $(bash "$installer" --source "$fixture/private") == "UPDATE protected persona sha256=${v15:0:12}" ]]
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]]

# Content production Core would refuse is never installed, the installer never
# prints it and a refusal leaves no staged copy behind.
refuse_persona() {
    local reason=$1
    shift
    if bash "$installer" "$@" >"$fixture/refused.log" 2>&1; then
        echo "installer accepted: $reason" >&2
        exit 1
    fi
    grep -Fq "$reason" "$fixture/refused.log"
    if grep -Fq 'SYNTHETIC-INVALID-BODY' "$fixture/refused.log"; then
        echo "installer printed persona text" >&2
        exit 1
    fi
    [[ $(installed_hash) == "$v15" ]]
    if compgen -G '/etc/jarvis/.Jarvis.md.*' >/dev/null; then
        echo "refused install left a staged persona" >&2
        exit 1
    fi
}
printf 'SYNTHETIC-INVALID-BODY without a heading\n' > "$persona"
refuse_persona 'persona failed validation' --source "$fixture/private"
{ printf '# Synthetic persona\nSYNTHETIC-INVALID-BODY\n'; head -c 40000 /dev/zero | tr '\0' a; } > "$persona"
refuse_persona 'persona failed validation' --source "$fixture/private"
# A root-owned, correctly named history entry is still validated on restore.
printf 'SYNTHETIC-INVALID-BODY in the history\n' > "$fixture/invalid-entry"
invalid_entry=$(sha256sum "$fixture/invalid-entry" | awk '{print $1}')
install -o root -g jarvis -m 0640 "$fixture/invalid-entry" "$history/$invalid_entry"
refuse_persona 'persona failed validation' --restore "$invalid_entry"
# The validator must be trusted and new enough.
install -o root -g root -m 0775 "$fixture/bin/jarvis" "$fixture/bin/writable-jarvis"
JARVIS_PERSONA_VALIDATOR="$fixture/bin/writable-jarvis"
refuse_persona 'persona validator must be root-owned' --restore "$v14"
printf '#!/bin/sh\necho "error: unrecognized subcommand" >&2\nexit 2\n' \
    | install -o root -g root -m 0755 /dev/stdin "$fixture/bin/old-jarvis"
JARVIS_PERSONA_VALIDATOR="$fixture/bin/old-jarvis"
refuse_persona 'release predates persona validation' --restore "$v14"
JARVIS_PERSONA_VALIDATOR="$fixture/bin/jarvis"

JARVIS_AGENT_BUNDLE_VALIDATOR="$fixture/validator" bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"
bundle=$(readlink -f /var/lib/jarvis/agents/current)
[[ -f $bundle/manifest.json ]]
[[ $(jq -r '.agents[0].name' "$bundle/manifest.json") == 'Test Agent' ]]
[[ $(jq -r '.agents[0].model_policy' "$bundle/manifest.json") == default ]]
[[ $(jq -r '.agents[0].group // "Ungrouped"' "$bundle/manifest.json") == Ungrouped ]]
profile_lines=$(jq -r '.agents[0].profile_lines' "$bundle/manifest.json")
[[ $profile_lines == 13 ]] || {
    echo "safe manifest profile line count mismatch: $profile_lines" >&2
    exit 1
}
source_updated_at=$(jq -r '.agents[0].source_updated_at' "$bundle/manifest.json")
[[ $source_updated_at == '2026-08-29T12:32:00Z' ]] || {
    echo "safe manifest source timestamp mismatch: $source_updated_at" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /var/lib/jarvis/agents) == root:jarvis:750 ]]
[[ $(stat -c '%U:%G:%a' /var/lib/jarvis/agents/releases) == root:jarvis:750 ]]
[[ $(stat -c '%U:%G:%a' "$bundle") == root:jarvis:750 ]]
[[ $(stat -c '%U:%G:%a' "$bundle/agents") == root:jarvis:750 ]]
[[ $(stat -c '%U:%G:%a' "$bundle/manifest.json") == root:jarvis:640 ]]
agent=$(find "$bundle/agents" -type f -name '*.json' -print -quit)
[[ -n $agent && $(stat -c '%U:%G:%a' "$agent") == root:jarvis:640 ]]
runuser -u jarvis -- test -r "$bundle/manifest.json"
runuser -u jarvis -- test ! -w "$bundle/manifest.json"
runuser -u jarvis -- test -r "$agent"
runuser -u jarvis -- test ! -w "$agent"
# shellcheck disable=SC2016 # $1 is expanded by the child shell.
runuser -u jarvis -- bash -c '! touch -- "$1/.jarvis-test"' _ "$bundle"
# shellcheck disable=SC2016 # $1 is expanded by the child shell.
runuser -u jarvis -- bash -c '! ln -s releases/nope "$1/.current-test"' _ /var/lib/jarvis/agents
install -o root -g root -m 0600 /dev/null /etc/jarvis/surrealdb.env
runuser -u jarvis -- test ! -r /etc/jarvis/surrealdb.env
# An idempotent private update repairs the restrictive v0.0.7 bundle metadata
# without changing its content or activation target.
chown -R root:root "$bundle"
chmod 0700 "$bundle"
JARVIS_AGENT_BUNDLE_VALIDATOR="$fixture/validator" bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"
[[ $(stat -c '%U:%G:%a' "$bundle") == root:jarvis:750 ]]
runuser -u jarvis -- test -r "$bundle/manifest.json"
ln -s /etc/passwd "$fixture/private/agents/02_EVIL.md"
if JARVIS_AGENT_BUNDLE_VALIDATOR="$fixture/validator" bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"; then
    echo "agent symlink was accepted" >&2
    exit 1
fi
[[ $(readlink -f /var/lib/jarvis/agents/current) == "$bundle" ]]
echo "Private installer fixture tests passed"
