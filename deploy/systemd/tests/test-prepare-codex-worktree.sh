#!/usr/bin/env bash
# Linux-only CI fixture for the root-only Codex worktree preparer.
set -euo pipefail

[[ ${GITHUB_ACTIONS:-} == true ]] || {
  echo "refusing to run outside GitHub Actions" >&2
  exit 1
}
[[ ${EUID} -eq 0 ]] || {
  echo "must run as root (use sudo in CI)" >&2
  exit 1
}

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
preparer="${repo_dir}/deploy/systemd/prepare-codex-worktree.sh"
fixture_dir=$(mktemp -d)
source_dir="${fixture_dir}/source"
task_id=11111111-1111-4111-8111-111111111111
worktree="/var/lib/jarvis-engineering/${task_id}"

cleanup() {
  chattr -R -i "${worktree}" 2>/dev/null || true
  rm -rf -- "${fixture_dir}" /var/lib/jarvis-engineering
  if id -u jarvis-codex >/dev/null 2>&1; then
    userdel jarvis-codex || true
  fi
}
trap cleanup EXIT

id -u jarvis-codex >/dev/null 2>&1 && {
  echo "fixture account already exists" >&2
  exit 1
}
useradd --system --user-group --home-dir /var/lib/jarvis-codex \
  --shell /usr/sbin/nologin jarvis-codex

git init --quiet "${source_dir}"
git -C "${source_dir}" config user.email fixture@example.invalid
git -C "${source_dir}" config user.name 'Jarvis CI fixture'
mkdir -p "${source_dir}/jarvis-core"
printf 'protected persona\n' > "${source_dir}/jarvis-core/Jarvis.md"
printf 'safe engineering file\n' > "${source_dir}/README.md"
printf 'PUBLIC_SETTING=replace-me\n' > "${source_dir}/.env.example"
git -C "${source_dir}" add .
git -C "${source_dir}" commit --quiet -m fixture
revision=$(git -C "${source_dir}" rev-parse HEAD)

bash "${preparer}" "${source_dir}" "${task_id}" "${revision}"
[[ -d "${worktree}" ]]
[[ $(stat -c '%U:%a' "${worktree}/jarvis-core") == root:555 ]]
[[ $(stat -c '%U:%a' "${worktree}/.git") == root:444 ]]

runuser -u jarvis-codex -- touch "${worktree}/engineering-note"
if runuser -u jarvis-codex -- touch "${worktree}/jarvis-core/forbidden"; then
  echo "Codex account unexpectedly wrote jarvis-core" >&2
  exit 1
fi
if runuser -u jarvis-codex -- rm -rf -- "${worktree}/jarvis-core"; then
  echo "Codex account unexpectedly removed jarvis-core" >&2
  exit 1
fi
if runuser -u jarvis-codex -- rm -- "${worktree}/.git"; then
  echo "Codex account unexpectedly removed .git" >&2
  exit 1
fi

printf 'must not enter a worktree\n' > "${source_dir}/.env"
git -C "${source_dir}" add .env
git -C "${source_dir}" commit --quiet -m secret-fixture
secret_revision=$(git -C "${source_dir}" rev-parse HEAD)
if bash "${preparer}" "${source_dir}" 22222222-2222-4222-8222-222222222222 "${secret_revision}"; then
  echo "secret-containing worktree unexpectedly succeeded" >&2
  exit 1
fi
[[ ! -e /var/lib/jarvis-engineering/22222222-2222-4222-8222-222222222222 ]]

echo "Codex worktree isolation fixture tests passed"
