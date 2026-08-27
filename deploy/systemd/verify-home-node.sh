#!/usr/bin/env bash
# Read-only post-install verification for the first, local-only Home Node run.
# It never changes firewall, router, service state, configuration, or releases.
set -euo pipefail
if [[ -r /usr/local/libexec/jarvis/ui.sh ]]; then
    # shellcheck disable=SC1091 # installed root-owned helper
    source /usr/local/libexec/jarvis/ui.sh
else
    repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
    # shellcheck disable=SC1091 # dynamic repository root
    source "$repo_dir/deploy/lib/ui.sh"
fi

failures=0
passed=0
check() {
    local label=$1
    shift
    if "$@"; then
        ui_success "$label"
        passed=$((passed + 1))
    else
        ui_error "$label"
        failures=$((failures + 1))
    fi
}
expect_mode() {
    local path=$1 expected=$2 actual
    actual=$(stat -c '%U:%G:%a' "$path" 2>/dev/null || true)
    [[ $actual == "$expected" ]]
}
loopback_only() {
    local port=$1 line local_address
    while IFS= read -r line; do
        local_address=$(awk '{print $4}' <<<"$line")
        [[ $local_address =~ ^127\.0\.0\.1: ]] || [[ $local_address =~ ^\[::1\]: ]] || return 1
    done < <(ss -ltnH "sport = :$port")
    return 0
}
agent_bundle_valid() {
    local bundle count
    [[ -L /var/lib/jarvis/agents/current ]] || return 1
    bundle=$(readlink -f /var/lib/jarvis/agents/current) || return 1
    [[ $bundle == /var/lib/jarvis/agents/releases/* && -f $bundle/manifest.json && ! -L $bundle/manifest.json ]] || return 1
    [[ $(stat -c '%U:%G:%a' "$bundle") == root:jarvis:750 ]] || return 1
    [[ $(stat -c '%U:%G:%a' "$bundle/agents") == root:jarvis:750 ]] || return 1
    [[ $(stat -c '%U:%G:%a' "$bundle/manifest.json") == root:jarvis:640 ]] || return 1
    count=$(jq -er '.agents | length | select(. > 0)' "$bundle/manifest.json") || return 1
    printf 'agent bundle: %s (%s agents)\n' "${bundle##*/}" "$count"
}
active_bundle() {
    readlink -f /var/lib/jarvis/agents/current
}
first_agent_definition() {
    local bundle
    bundle=$(active_bundle) || return 1
    find "$bundle/agents" -maxdepth 1 -type f -name '*.json' -print -quit
}
jarvis_reads() { runuser -u jarvis -- test -r "$1"; }
jarvis_cannot_write() { runuser -u jarvis -- test ! -w "$1"; }
jarvis_cannot_create_in() {
    # shellcheck disable=SC2016 # $1 is expanded by the child shell.
    runuser -u jarvis -- bash -c '! touch -- "$1/.jarvis-permission-probe" 2>/dev/null' _ "$1"
}
jarvis_cannot_replace_current() {
    # shellcheck disable=SC2016 # $1 is expanded by the child shell.
    runuser -u jarvis -- bash -c '! ln -s releases/not-a-bundle "$1/.current-permission-probe" 2>/dev/null' _ /var/lib/jarvis/agents
}

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
command -v ss >/dev/null 2>&1 || { echo "ss is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

ui_heading "Jarvis Home Node security verification"
ui_step "Identities and permissions"
check "jarvis service identity exists" getent passwd jarvis
check "jarvis has no Docker access" bash -c '! id -nG jarvis | tr " " "\\n" | grep -qx docker'
check "Jarvis state directory permissions" expect_mode /var/lib/jarvis jarvis:jarvis:750
check "Protected configuration permissions" expect_mode /etc/jarvis root:jarvis:750
check "Provider secret directory permissions" expect_mode /etc/jarvis/secrets root:jarvis:750
check "Protected persona permissions" expect_mode /etc/jarvis/Jarvis.md root:jarvis:640
check "Agent bundle is valid and immutable" agent_bundle_valid
ui_step "Services and network"
check "Docker active" systemctl is-active --quiet docker.service
check "SurrealDB service active" systemctl is-active --quiet jarvis-surrealdb.service
check "Jarvis Core active" systemctl is-active --quiet jarvis-core.service
check "Updater timer enabled" systemctl is-enabled --quiet jarvis-updater.timer
check "SurrealDB is loopback-only" loopback_only 8000
check "Jarvis API is loopback-only" loopback_only 8080
ui_step "Health and protected inputs"
check "/livez responds" curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/livez
check "/readyz responds" curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/readyz
check "Persona readable by Core" jarvis_reads /etc/jarvis/Jarvis.md
check "Persona is read-only to Core" jarvis_cannot_write /etc/jarvis/Jarvis.md
if [[ -e /etc/jarvis/model-policy.json ]]; then
    check "Model policy permissions" expect_mode /etc/jarvis/model-policy.json root:jarvis:640
    check "Model policy readable by Core" jarvis_reads /etc/jarvis/model-policy.json
    check "Model policy read-only to Core" jarvis_cannot_write /etc/jarvis/model-policy.json
fi
check "Agent manifest readable by Core" jarvis_reads /var/lib/jarvis/agents/current/manifest.json
check "Agent bundle is not writable by jarvis" jarvis_cannot_create_in "$(active_bundle)"
check "Agent activation cannot be replaced by jarvis" jarvis_cannot_replace_current
ui_step "Secrets and optional services"
check "Docker socket is unreadable by jarvis" runuser -u jarvis -- test ! -r /var/run/docker.sock
check "Root SurrealDB credentials are unreadable" runuser -u jarvis -- test ! -r /etc/jarvis/surrealdb.env
for provider_secret in /etc/jarvis/secrets/*.env; do
    [[ -e $provider_secret ]] || continue
    check "Provider credential permissions (${provider_secret##*/})" expect_mode "$provider_secret" root:jarvis:640
done
if [[ -e /etc/jarvis/updater.env ]]; then
    check "Updater credentials permissions" expect_mode /etc/jarvis/updater.env root:root:600
    check "Updater credentials are unreadable" runuser -u jarvis -- test ! -r /etc/jarvis/updater.env
fi
check "OpenSandbox remains disabled" bash -c '! systemctl is-enabled --quiet jarvis-opensandbox.service'
check "OpenSandbox remains inactive" bash -c '! systemctl is-active --quiet jarvis-opensandbox.service'

if ((failures)); then
    ui_error "Security verification: $passed passed, $failures failed. Do not enable public ingress."
    exit 1
fi
ui_success "Security verification: $passed passed, 0 failed"
ui_success "Home Node verification PASSED"
