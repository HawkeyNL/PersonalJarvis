#!/usr/bin/env bash
# Deterministic interface/security checks for the root-only owner CLI.  This
# intentionally performs no update, credential, agent, or systemd mutation.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
admin="$repo_dir/deploy/systemd/jarvis-admin.sh"
updater="$repo_dir/deploy/systemd/update-core-release.sh"

[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || { echo "CI root fixture only" >&2; exit 1; }
bash -n "$admin" "$updater"

help=$(bash "$admin" --help)
[[ $help == *'update [--latest|--version TAG|--check|--status|--rollback [--yes]]'* ]]
[[ $help == *'credentials <list|set|test|remove>'* ]]
[[ $help == *'logs <core|config-broker|surrealdb|updater|agents>'* ]]
if bash "$admin" arbitrary-command >/dev/null 2>&1; then
    echo "arbitrary command was accepted" >&2
    exit 1
fi
if bash "$admin" logs arbitrary-unit >/dev/null 2>&1; then
    echo "arbitrary log unit was accepted" >&2
    exit 1
fi

# The wrapper is an allowlist dispatcher. Never reintroduce eval, shell input,
# arbitrary systemctl units, paths, or credential arguments.
if grep -Eq '(^|[^[:alnum:]_])eval([[:space:]]|$)' "$admin"; then
    echo "admin CLI must not evaluate user input" >&2
    exit 1
fi
grep -Fq 'jarvis-core.service jarvis-surrealdb.service' "$admin"
grep -Fq 'core) unit=jarvis-core.service' "$admin"
grep -Fq 'config-broker) unit=jarvis-config-broker.service' "$admin"
grep -Fq 'credentials) ' "$admin"
grep -Fq 'models status takes no options' "$admin"
grep -Fq "with_lock \"\$config_lock\"" "$admin"
! grep -Fq 'with_lock /run/jarvis-private-agent-update.lock "$libexec/private-agent-poll"' "$admin"
grep -Fq 'requires a controlling TTY' "$repo_dir/deploy/systemd/jarvis-credentials.sh"
grep -Fq 'mode=check' "$updater"
grep -Fq 'mode=rollback' "$updater"
grep -Fq 'refusing downgrade' "$updater"
echo "Jarvis admin CLI checks passed"
