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

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
command -v ss >/dev/null 2>&1 || { echo "ss is required" >&2; exit 1; }

check getent passwd jarvis
check bash -c '! id -nG jarvis | tr " " "\\n" | grep -qx docker'
check expect_mode /var/lib/jarvis jarvis:jarvis:750
check expect_mode /var/lib/jarvis/surrealdb root:root:700
check expect_mode /etc/jarvis root:root:750
check expect_mode /etc/jarvis/core.env root:jarvis:640
check expect_mode /etc/jarvis/surrealdb.env root:root:600
check systemctl is-active --quiet docker.service
check systemctl is-active --quiet jarvis-surrealdb.service
check systemctl is-active --quiet jarvis-core.service
check bash -c 'docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml ps --status running --services | grep -qx surrealdb'
check loopback_only 8000
check loopback_only 8080
check curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/livez
check curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8080/readyz
check runuser -u jarvis -- test ! -r /var/run/docker.sock
check bash -c '[[ $(systemctl show -p User --value jarvis-core.service) == jarvis ]]'
check bash -c '[[ $(systemctl show -p NoNewPrivileges --value jarvis-core.service) == yes ]]'
check runuser -u jarvis -- test ! -w /opt/jarvis/current/jarvis-api
check runuser -u jarvis -- test ! -w /opt/jarvis/current/jarvis-core/Jarvis.md
check bash -c '! systemctl is-enabled --quiet jarvis-opensandbox.service'
check bash -c '! systemctl is-active --quiet jarvis-opensandbox.service'

if ((failures)); then
    echo "Home Node verification failed: $failures check(s). Do not enable public ingress." >&2
    exit 1
fi
echo "Home Node verification passed: Core and SurrealDB are private and locally ready."
