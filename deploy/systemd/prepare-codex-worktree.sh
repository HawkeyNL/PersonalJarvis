#!/usr/bin/env bash
# Prepares an isolated, detached Codex worktree on the Home Node.
#
# This is an operator-only root helper. It is intentionally not setuid, exposed
# through the Jarvis API, or callable by an agent tool.
set -euo pipefail

readonly WORKTREE_PARENT="/var/lib/jarvis-engineering"
readonly CODEX_USER="jarvis-codex"
readonly PROTECTED_PATHS=("jarvis-core" ".git")

usage() {
  echo "usage: $0 <primary-git-checkout> <task-uuid> <immutable-commit>" >&2
  exit 64
}

[[ "${EUID}" -eq 0 ]] || {
  echo "must run as root" >&2
  exit 77
}
[[ "$#" -eq 3 ]] || usage

source_checkout="$(realpath -e -- "$1")"
task_id="$2"
commit="$3"

[[ -d "${source_checkout}/.git" ]] || {
  echo "source checkout must be a primary Git checkout" >&2
  exit 65
}
[[ "${task_id}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] || {
  echo "task UUID is invalid" >&2
  exit 65
}
[[ "${commit}" =~ ^[0-9a-fA-F]{7,64}$ ]] || {
  echo "commit must be an immutable hexadecimal revision" >&2
  exit 65
}

command -v git >/dev/null
command -v chattr >/dev/null || {
  echo "chattr is required to protect jarvis-core and .git" >&2
  exit 69
}
id -u "${CODEX_USER}" >/dev/null

source_toplevel="$(git -C "${source_checkout}" rev-parse --show-toplevel)"
[[ "${source_toplevel}" == "${source_checkout}" ]] || {
  echo "source checkout must not be a linked worktree" >&2
  exit 65
}
git -C "${source_checkout}" cat-file -e "${commit}^{commit}"

install -d -o root -g "${CODEX_USER}" -m 0770 "${WORKTREE_PARENT}"
target="${WORKTREE_PARENT}/${task_id}"
[[ ! -e "${target}" && ! -L "${target}" ]] || {
  echo "target worktree already exists" >&2
  exit 73
}

created_worktree=false
cleanup() {
  if [[ "${created_worktree}" == true ]]; then
    git -C "${source_checkout}" worktree remove --force "${target}" || true
  fi
}
trap cleanup ERR

git -C "${source_checkout}" worktree add --detach "${target}" "${commit}"
created_worktree=true

# The reviewed, versioned .env.example template is not a credential. Every
# actual environment file (including .env.production) remains disallowed.
if find "${target}" -xdev \
  \( -name '.env' -o \( -name '.env.*' ! -name '.env.example' \) -o -name '*.key' -o -name '*.pem' -o -path '*/.ssh/*' \) \
  -print -quit | grep -q .; then
  echo "refusing a worktree containing secret-like files" >&2
  exit 65
fi

chown -R "${CODEX_USER}:${CODEX_USER}" "${target}"
for protected in "${PROTECTED_PATHS[@]}"; do
  protected_path="${target}/${protected}"
  [[ ! -L "${protected_path}" ]] || {
    echo "protected path must not be a symlink: ${protected}" >&2
    exit 65
  }
  [[ -e "${protected_path}" ]] || {
    echo "missing protected path: ${protected}" >&2
    exit 65
  }
  chown -R root:root "${protected_path}"
  chmod -R a-w "${protected_path}"
  chattr -R +i "${protected_path}"
done

trap - ERR
echo "${target}"
