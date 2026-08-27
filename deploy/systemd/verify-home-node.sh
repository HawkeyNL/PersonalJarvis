#!/usr/bin/env bash
# Read-only post-install verification for the first, local-only Home Node run.
# It never changes firewall, router, service state, configuration, or releases.
set -euo pipefail

failures=0
check() {
    if "$@"; then
        printf 'ok: %s\n' "$*"
    else
        printf 'FAIL: %s\n' "$*" >&2
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
    runuser -u jarvis -- bash -c '! touch -- "$1/.jarvis-permission-probe"' _ "$1"
}
jarvis_cannot_replace_current() {
    # shellcheck disable=SC2016 # $1 is expanded by the child shell.
    runuser -u jarvis -- bash -c '! ln -s releases/not-a-bundle "$1/.current-permission-probe"' _ /var/lib/jarvis/agents
}

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
command -v ss >/dev/null 2>&1 || { echo "ss is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }

check getent passwd jarvis
check bash -c '! id -nG jarvis | tr " " "\\n" | grep -qx docker'
check expect_mode /var/lib/jarvis jarvis:jarvis:750
check expect_mode /var/lib/jarvis/agents root:jarvis:750
check expect_mode /var/lib/jarvis/agents/releases root:jarvis:750
check expect_mode /var/lib/jarvis/surrealdb root:root:700
check expect_mode /etc/jarvis root:jarvis:750
check expect_mode /etc/jarvis/core.env root:jarvis:640
check expect_mode /etc/jarvis/Jarvis.md root:jarvis:640
check agent_bundle_valid
check expect_mode /etc/jarvis/surrealdb.env root:root:600
check systemctl is-active --quiet docker.service
check systemctl is-active --quiet jarvis-surrealdb.service
check systemctl is-active --quiet jarvis-core.service
check systemctl is-enabled --quiet jarvis-updater.timer
check bash -c 'docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml ps --status running --services | grep -qx surrealdb'
check loopback_only 8000
check loopback_only 8080
check curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/livez
check curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/readyz
check runuser -u jarvis -- test ! -r /var/run/docker.sock
check bash -c '[[ $(systemctl show -p User --value jarvis-core.service) == jarvis ]]'
check bash -c '[[ $(systemctl show -p NoNewPrivileges --value jarvis-core.service) == yes ]]'
check runuser -u jarvis -- test ! -w /opt/jarvis/current/jarvis-api
check jarvis_reads /etc/jarvis/Jarvis.md
check jarvis_cannot_write /etc/jarvis/Jarvis.md
check jarvis_reads /var/lib/jarvis/agents/current/manifest.json
check jarvis_cannot_write /var/lib/jarvis/agents/current/manifest.json
check jarvis_reads "$(first_agent_definition)"
check jarvis_cannot_write "$(first_agent_definition)"
check jarvis_cannot_create_in "$(active_bundle)"
check jarvis_cannot_replace_current
check runuser -u jarvis -- test ! -r /etc/jarvis/surrealdb.env
if [[ -e /etc/jarvis/updater.env ]]; then
    check expect_mode /etc/jarvis/updater.env root:root:600
    check runuser -u jarvis -- test ! -r /etc/jarvis/updater.env
fi
check bash -c 'sha256sum /etc/jarvis/Jarvis.md | cut -c1-12 >/dev/null'
check bash -c '! systemctl is-enabled --quiet jarvis-opensandbox.service'
check bash -c '! systemctl is-active --quiet jarvis-opensandbox.service'

if ((failures)); then
    echo "Home Node verification failed: $failures check(s). Do not enable public ingress." >&2
    exit 1
fi
echo "Home Node verification passed: Core and SurrealDB are private and locally ready."
