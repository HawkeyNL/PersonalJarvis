#!/usr/bin/env bash
# Root-only synthetic fixture. It contains no owner content and is limited to
# GitHub Actions because it deliberately exercises canonical system paths.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || { echo "CI root fixture only" >&2; exit 1; }

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
cleanup() {
    rm -rf -- "$fixture" /etc/jarvis /var/lib/jarvis/agents
    if [[ ${created_user:-false} == true ]]; then userdel jarvis 2>/dev/null || true; fi
}
trap cleanup EXIT

created_user=false
if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin jarvis
    created_user=true
fi
mkdir -p "$fixture/private/personaljarvis/jarvis-core" "$fixture/private/agents"
printf 'Synthetic owner persona.\n' > "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
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

JARVIS_AGENT_BUNDLE_VALIDATOR="$fixture/validator" bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"
bundle=$(readlink -f /var/lib/jarvis/agents/current)
[[ -f $bundle/manifest.json ]]
[[ $(jq -r '.agents[0].name' "$bundle/manifest.json") == 'Test Agent' ]]
[[ $(jq -r '.agents[0].model_policy' "$bundle/manifest.json") == default ]]
[[ $(jq -r '.agents[0].group // "Ungrouped"' "$bundle/manifest.json") == Ungrouped ]]
[[ $(jq -r '.agents[0].profile_lines' "$bundle/manifest.json") == 13 ]]
[[ $(jq -r '.agents[0].source_updated_at' "$bundle/manifest.json") == '2026-08-29T12:32:00+00:00' ]]
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
