#!/usr/bin/env bash
# Canonical, root-operated Home Node administration interface.  It deliberately
# dispatches only a small allowlist of typed operations to existing privileged
# helpers; it is never an arbitrary command or systemctl passthrough.
set -euo pipefail

readonly libexec=/usr/local/libexec/jarvis
readonly sbin=/usr/local/sbin
readonly config_lock=/run/jarvis-admin-config.lock
readonly agent_root=/var/lib/jarvis/agents

fail() { echo "jarvis: $*" >&2; exit 1; }
require_root() { [[ ${EUID} -eq 0 ]] || fail "must run as root (use: sudo jarvis ...)"; }
require_helper() { [[ -x $1 && ! -L $1 ]] || fail "required Home Node helper is unavailable: $1"; }

usage() {
    cat <<'EOF'
Usage: sudo jarvis <command> [options]

Commands:
  version                         Show active production release
  status                          Compact Home Node status (no secrets)
  health                          Run bounded production health checks
  update [--latest|--version TAG|--check|--status|--rollback [--yes]]
  models <list|refresh|enable|disable|show> ...
  credentials <list|set|test|remove> ...
  agents <status|check|update|rollback [--yes]>
  services status                 Show allowlisted service states
  logs <core|surrealdb|updater|agents> [--lines N] [--follow]
  help                            Show this help

Run 'sudo jarvis <command> --help' for command-specific help.
EOF
}

active_release() {
    local target
    target=$(readlink -f /opt/jarvis/current 2>/dev/null || true)
    [[ $target == /opt/jarvis/releases/v* && -f $target/release.json && ! -L $target/release.json ]] || return 1
    jq -er '.tag | strings | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))' "$target/release.json"
}

with_lock() {
    local lock=$1
    shift
    exec 9>"$lock"
    flock -n 9 || fail "another conflicting Jarvis administration operation is running"
    "$@"
}

confirm() {
    local prompt=$1
    [[ ${2:-} == --yes ]] && return 0
    [[ -t 0 && -t 1 ]] || fail "refusing non-interactive mutation; pass --yes after reviewing the target"
    local answer
    read -r -p "$prompt [y/N] " answer
    [[ $answer == y || $answer == Y || $answer == yes ]] || { echo "jarvis: unchanged"; exit 0; }
}

version() {
    printf 'Jarvis admin CLI: installed\n'
    printf 'Active Core:      %s\n' "$(active_release 2>/dev/null || printf 'unavailable')"
}

health() { require_helper "$libexec/verify-home-node"; exec "$libexec/verify-home-node"; }

services_status() {
    local unit
    printf '%-28s %s\n' SERVICE STATUS
    for unit in jarvis-core.service jarvis-surrealdb.service jarvis-updater.timer jarvis-private-agent-updater.timer jarvis-opensandbox.service; do
        printf '%-28s %s\n' "$unit" "$(systemctl is-active "$unit" 2>/dev/null || true)"
    done
}

agent_bundle() {
    readlink -f "$agent_root/current" 2>/dev/null || return 1
}

agents_status() {
    local bundle count source='unknown'
    bundle=$(agent_bundle) || fail "no active private agent bundle"
    [[ $bundle == "$agent_root"/releases/* && -f $bundle/manifest.json && ! -L $bundle/manifest.json ]] || fail "active agent bundle is unsafe"
    count=$(jq -er '.agents | length' "$bundle/manifest.json") || fail "active agent bundle is malformed"
    if [[ -f /etc/jarvis/private-agent-updater.env && ! -L /etc/jarvis/private-agent-updater.env ]]; then
        # Root-only configuration is parsed only for the path, never printed.
        # shellcheck disable=SC1091 # root-managed private updater input
        source /etc/jarvis/private-agent-updater.env
        if [[ ${JARVIS_PRIVATE_AGENT_SOURCE:-} == /* && -d ${JARVIS_PRIVATE_AGENT_SOURCE:-}/.git ]]; then
            source=$(git -C "$JARVIS_PRIVATE_AGENT_SOURCE" rev-parse --short HEAD 2>/dev/null || printf unknown)
        fi
    fi
    printf 'Agent bundle: %s (%s agents)\nSource commit: %s\n' "${bundle##*/}" "$count" "$source"
}

private_source() {
    [[ -f /etc/jarvis/private-agent-updater.env && ! -L /etc/jarvis/private-agent-updater.env ]] || fail "private agent updater is not configured"
    # shellcheck disable=SC1091
    source /etc/jarvis/private-agent-updater.env
    [[ ${JARVIS_PRIVATE_AGENT_SOURCE:-} == /* && ${JARVIS_PRIVATE_AGENT_REPOSITORY:-} == HawkeyNL/PersonalJarvisAgents ]] || fail "private agent updater configuration is invalid"
    [[ -d $JARVIS_PRIVATE_AGENT_SOURCE/.git ]] || fail "configured private agent checkout is unavailable"
    printf '%s\n' "$JARVIS_PRIVATE_AGENT_SOURCE"
}

agents_check() {
    local source current remote
    source=$(private_source)
    git -C "$source" fetch --quiet origin refs/heads/main || fail "could not check private-agent origin/main"
    current=$(git -C "$source" rev-parse HEAD)
    remote=$(git -C "$source" rev-parse FETCH_HEAD)
    printf 'Current: %s\nLatest:  %s\nUpdate:  %s\n' "${current:0:12}" "${remote:0:12}" "$( [[ $current == "$remote" ]] && printf up-to-date || printf available )"
    [[ $current == "$remote" ]]
}

agents_update() {
    private_source >/dev/null
    require_helper "$libexec/private-agent-poll"
    with_lock /run/jarvis-private-agent-update.lock "$libexec/private-agent-poll"
}

agents_rollback() {
    local yes=${1:-} current candidate
    confirm "Activate the previous verified private agent bundle?" "$yes"
    current=$(agent_bundle) || fail "no active private agent bundle"
    candidate=$(find "$agent_root/releases" -mindepth 1 -maxdepth 1 -type d -name 'bundle-*' ! -samefile "$current" -printf '%T@:%f\n' 2>/dev/null | LC_ALL=C sort -n | tail -n 1 | cut -d: -f2-)
    [[ -n $candidate && -d $agent_root/releases/$candidate && ! -L $agent_root/releases/$candidate ]] || fail "no verified historical agent bundle is available"
    jq -e '.version == 1 and (.agents | type == "array" and length > 0)' "$agent_root/releases/$candidate/manifest.json" >/dev/null || fail "historical agent bundle is malformed"
    with_lock /run/jarvis-private-agent-update.lock agent_activate "$candidate"
    systemctl try-restart jarvis-core.service >/dev/null 2>&1 || true
    echo "jarvis: activated historical private agent bundle $candidate"
}

agent_activate() {
    local candidate=$1 temporary="$agent_root/.current.new"
    rm -f -- "$temporary"
    ln -s "releases/$candidate" "$temporary"
    mv -Tf "$temporary" "$agent_root/current"
}

update() {
    require_helper "$libexec/update-core-release"
    case ${1:---latest} in
        --latest) (($# == 0 || $# == 1)) || fail "use: jarvis update --latest"; exec "$libexec/update-core-release" --latest ;;
        --version) (($# == 2)) || fail "use: jarvis update --version vMAJOR.MINOR.PATCH"; exec "$libexec/update-core-release" --version "$2" ;;
        --check) (($# == 1)) || fail "use: jarvis update --check"; exec "$libexec/update-core-release" --check ;;
        --status) (($# == 1)) || fail "use: jarvis update --status"; exec "$libexec/update-core-release" --status ;;
        --rollback)
            (($# == 1 || ($# == 2 && $2 == --yes))) || fail "use: jarvis update --rollback [--yes]"
            confirm "Rollback Core to the previous verified release?" "${2:-}"
            exec "$libexec/update-core-release" --rollback
            ;;
        --help|-h) cat <<'EOF'
Usage: sudo jarvis update [--latest|--version vMAJOR.MINOR.PATCH|--check|--status|--rollback [--yes]]
Downloads only published stable releases, verifies the existing artifact/checksum
contract, and rolls back automatically if Core readiness fails.
EOF
            ;;
        *) fail "unknown update option '$1'; run: sudo jarvis update --help" ;;
    esac
}

logs() {
    local target=${1:-} lines=80 follow=false unit
    shift || true
    case $target in
        core) unit=jarvis-core.service ;;
        surrealdb) unit=jarvis-surrealdb.service ;;
        updater) unit=jarvis-updater.service ;;
        agents) unit=jarvis-private-agent-updater.service ;;
        *) fail "unknown log target '$target'; allowed: core, surrealdb, updater, agents" ;;
    esac
    while (($#)); do
        case $1 in
            --lines) [[ ${2:-} =~ ^[1-9][0-9]{0,3}$ ]] || fail "--lines must be 1..9999"; lines=$2; shift 2 ;;
            --follow) follow=true; shift ;;
            --help|-h) echo "Usage: sudo jarvis logs <core|surrealdb|updater|agents> [--lines N] [--follow]"; return ;;
            *) fail "unknown logs option '$1'" ;;
        esac
    done
    if [[ $follow == true ]]; then exec journalctl --no-pager -u "$unit" -n "$lines" -f; fi
    exec journalctl --no-pager -u "$unit" -n "$lines"
}

status() {
    local providers=0 enabled=0
    echo "Jarvis Home Node"
    printf 'Core release       %s\n' "$(active_release 2>/dev/null || printf unavailable)"
    printf 'Core               %s\n' "$(systemctl is-active jarvis-core.service 2>/dev/null || true)"
    printf 'SurrealDB          %s\n' "$(systemctl is-active jarvis-surrealdb.service 2>/dev/null || true)"
    agents_status
    if [[ -f /etc/jarvis/model-policy.json && ! -L /etc/jarvis/model-policy.json ]]; then
        providers=$(jq -r '[.models[].provider] | unique | length' /etc/jarvis/model-policy.json 2>/dev/null || printf 0)
        enabled=$(jq -r '[.models[] | select(.enabled)] | length' /etc/jarvis/model-policy.json 2>/dev/null || printf 0)
    fi
    printf 'Configured AI       %s providers / %s enabled models\n' "$providers" "$enabled"
    printf 'Updater             %s\n' "$(systemctl is-enabled jarvis-updater.timer 2>/dev/null || true)"
    printf 'OpenSandbox         %s\n' "$(systemctl is-active jarvis-opensandbox.service 2>/dev/null || true)"
}

main() {
    require_root
    local command=${1:-help}
    shift || true
    case $command in
        help|--help|-h) (($# == 0)) || fail "help takes no arguments"; usage ;;
        version) (($# == 0)) || fail "version takes no arguments"; version ;;
        status) (($# == 0)) || fail "status takes no arguments"; status ;;
        health) (($# == 0)) || fail "health takes no arguments"; health ;;
        update) update "$@" ;;
        models)
            [[ ${1:-} != --help && ${1:-} != -h ]] || { echo "Usage: sudo jarvis models <list|refresh|enable|disable|show|status> ..."; return; }
            require_helper "$sbin/jarvis-models"
            if [[ ${1:-} == status ]]; then
                (($# == 1)) || fail "models status takes no options"
                with_lock "$config_lock" "$sbin/jarvis-models" list
            else
                with_lock "$config_lock" "$sbin/jarvis-models" "$@"
            fi
            ;;
        credentials) [[ ${1:-} != --help && ${1:-} != -h ]] || { echo "Usage: sudo jarvis credentials <list|set|test|remove> <provider>"; return; }; require_helper "$sbin/jarvis-credentials"; with_lock "$config_lock" "$sbin/jarvis-credentials" "$@" ;;
        agents)
            case ${1:-} in
                status) (($# == 1)) || fail "agents status takes no options"; agents_status ;;
                check) (($# == 1)) || fail "agents check takes no options"; agents_check ;;
                update) (($# == 1)) || fail "agents update takes no options"; agents_update ;;
                rollback) (($# == 1 || ($# == 2 && $2 == --yes))) || fail "use: jarvis agents rollback [--yes]"; agents_rollback "${2:-}" ;;
                --help|-h|'') echo "Usage: sudo jarvis agents <status|check|update|rollback [--yes]>" ;;
                *) fail "unknown agents command '$1'" ;;
            esac
            ;;
        services) [[ ${1:-} == status && $# == 1 ]] || fail "use: jarvis services status"; services_status ;;
        logs) logs "$@" ;;
        *) fail "unknown command '$command'; run: sudo jarvis --help" ;;
    esac
}

main "$@"
