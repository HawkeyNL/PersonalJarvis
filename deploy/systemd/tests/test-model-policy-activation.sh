#!/usr/bin/env bash
# Rootless fake-systemd coverage; no protected state or real services accessed.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
source "$repo/deploy/systemd/jarvis-models.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
systemctl() {
    printf '%s\n' "$*" >> "$fixture/calls"
    case $1 in
        try-restart) [[ $scenario == success ]] ;;
        stop) [[ $scenario != stop-failed ]] ;;
        *) return 99 ;;
    esac
}
scenario=success
activate_model_policy
[[ $(<"$fixture/calls") == 'try-restart jarvis-core.service' ]]
for scenario in restart-failed stop-failed; do
    : > "$fixture/calls"
    if (activate_model_policy) > "$fixture/output" 2>&1; then
        echo 'model activation incorrectly reported success' >&2; exit 1
    fi
    [[ $(wc -l < "$fixture/calls") == 2 ]]
    grep -Fxq 'stop jarvis-core.service' "$fixture/calls"
    if [[ $scenario == restart-failed ]]; then
        grep -Fq 'Core stopped safely' "$fixture/output"
    else
        grep -Fq 'owner recovery required' "$fixture/output"
    fi
done
echo 'CLI model activation fails closed without restoring old grants'
