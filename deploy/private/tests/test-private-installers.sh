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
printf '# Synthetic agent\n' > "$fixture/private/agents/01_TEST_AGENT.md"

bash "$repo_dir/deploy/private/install-private-config.sh" --source "$fixture/private"
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]]
before=$(sha256sum /etc/jarvis/Jarvis.md | awk '{print $1}')
rm "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
ln -s /etc/passwd "$fixture/private/personaljarvis/jarvis-core/Jarvis.md"
if bash "$repo_dir/deploy/private/install-private-config.sh" --source "$fixture/private"; then
    echo "persona symlink was accepted" >&2
    exit 1
fi
[[ $(sha256sum /etc/jarvis/Jarvis.md | awk '{print $1}') == "$before" ]]

bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"
bundle=$(readlink -f /var/lib/jarvis/agents/current)
[[ -f $bundle/manifest.json ]]
[[ $(stat -c '%U:%G:%a' "$bundle/manifest.json") == root:root:644 ]]
ln -s /etc/passwd "$fixture/private/agents/02_EVIL.md"
if bash "$repo_dir/deploy/private/install-agent-bundle.sh" --source "$fixture/private"; then
    echo "agent symlink was accepted" >&2
    exit 1
fi
[[ $(readlink -f /var/lib/jarvis/agents/current) == "$bundle" ]]
echo "Private installer fixture tests passed"
