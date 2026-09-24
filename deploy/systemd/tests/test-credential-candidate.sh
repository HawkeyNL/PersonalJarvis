#!/usr/bin/env bash
# All files/calls are fixtures: never access host credentials or services.
set -euo pipefail
trap 'echo "Credential candidate assertion failed at line $LINENO" >&2' ERR
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
source "$repo/deploy/systemd/jarvis-credentials.sh"
credential_file() { printf '%s/installed\n' "$fixture"; }
ensure_provider_defaults() { :; }
chown() { :; }
stat() { printf 'root:jarvis:640\n'; }
mktemp() { command mktemp "$fixture/backup.XXXXXX"; }
probe_provider() { [[ $probe_ok == yes ]]; }
systemctl() { printf '%s\n' "$*" >> "$fixture/service-calls"; }
wait_healthy() { [[ $healthy == yes ]]; }

probe_ok=no healthy=yes
printf 'old-fixture-value\n' > "$fixture/installed"
printf 'new-fixture-value\n' > "$fixture/candidate"
if install_credential_candidate openai "$fixture/candidate" > "$fixture/result" 2>&1; then
    echo 'rejected credential was accepted' >&2; exit 1
fi
grep -Fxq old-fixture-value "$fixture/installed"
[[ ! -e $fixture/service-calls && ! -e $fixture/candidate ]]

probe_ok=yes healthy=no
printf 'new-fixture-value\n' > "$fixture/candidate"
if install_credential_candidate openai "$fixture/candidate" >> "$fixture/result" 2>&1; then
    echo 'failed Core restart was accepted' >&2; exit 1
fi
grep -Fxq old-fixture-value "$fixture/installed"
[[ $(wc -l < "$fixture/service-calls") == 2 ]]

healthy=yes
printf 'new-fixture-value\n' > "$fixture/candidate"
install_credential_candidate openai "$fixture/candidate" >> "$fixture/result" 2>&1
grep -Fxq new-fixture-value "$fixture/installed"
[[ $(wc -l < "$fixture/service-calls") == 3 ]]
[[ ! -e $fixture/candidate ]]
[[ -z $(find "$fixture" -name 'backup.*' -print -quit) ]]
if grep -Eq '(old|new)-fixture-value' "$fixture/result" "$fixture/service-calls"; then
    echo 'credential leaked into output or service arguments' >&2; exit 1
fi
echo 'Credential candidate validation/rollback tests passed'
