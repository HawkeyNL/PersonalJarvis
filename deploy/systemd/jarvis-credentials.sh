#!/usr/bin/env bash
# Root-operated provider credential manager.  Values are accepted only from the
# controlling TTY and are never rendered, logged, or supplied in argv.
set -euo pipefail

readonly secret_dir=/etc/jarvis/secrets

fail() { echo "jarvis-credentials: $*" >&2; exit 1; }
[[ ${EUID} -eq 0 ]] || fail "must run as root"

usage() {
    cat >&2 <<'EOF'
Usage: sudo jarvis-credentials <set|list|test|remove> [provider]

Providers: anthropic, openai, deepseek, xai, zai, ollama-cloud.
Local Ollama has no credential; configure its loopback URL/model in core.env.
EOF
    exit 64
}

provider_var() {
    case $1 in
        anthropic) printf '%s\n' JARVIS_LLM_API_KEY ;;
        openai) printf '%s\n' JARVIS_LLM_OPENAI_API_KEY ;;
        deepseek) printf '%s\n' JARVIS_LLM_DEEPSEEK_API_KEY ;;
        xai) printf '%s\n' JARVIS_LLM_XAI_API_KEY ;;
        zai) printf '%s\n' JARVIS_LLM_ZAI_API_KEY ;;
        ollama-cloud) printf '%s\n' JARVIS_LLM_OLLAMA_CLOUD_API_KEY ;;
        *) return 1 ;;
    esac
}

credential_file() {
    provider_var "$1" >/dev/null || return 1
    printf '%s/%s.env\n' "$secret_dir" "$1"
}

require_tty() {
    [[ -t 0 && -t 1 && -r /dev/tty && -w /dev/tty ]] ||
        fail "requires a controlling TTY; secrets are never accepted through argv/stdin"
}

wait_healthy() {
    local _attempt
    for _attempt in {1..12}; do
        if curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/livez >/dev/null \
            && curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/readyz >/dev/null; then
            return 0
        fi
        sleep 1
    done
    return 1
}

restart_or_rollback() {
    local file=$1 backup=${2:-} had_backup=$3
    if systemctl restart jarvis-core.service && wait_healthy; then
        return 0
    fi
    echo "jarvis-credentials: Core did not become healthy; restoring prior credential state." >&2
    if [[ $had_backup == yes ]]; then
        mv -f -- "$backup" "$file"
    else
        rm -f -- "$file"
    fi
    systemctl restart jarvis-core.service >/dev/null 2>&1 || true
    systemctl --no-pager --full status jarvis-core.service 2>/dev/null | tail -n 20 >&2 || true
    return 1
}

set_credential() {
    local provider=$1 var file tmp backup='' had_backup=no secret
    [[ $provider != ollama ]] || fail "local Ollama has no credential; use ollama-cloud only for a remote API"
    var=$(provider_var "$provider") || fail "unknown provider"
    require_tty
    install -d -o root -g jarvis -m 0750 "$secret_dir"
    file=$(credential_file "$provider")
    printf 'Enter %s credential (input hidden): ' "$provider" >/dev/tty
    IFS= read -r -s secret </dev/tty || fail "could not read credential"
    printf '\n' >/dev/tty
    [[ -n $secret && $secret != *$'\n'* && ${#secret} -le 8192 ]] || fail "credential is empty or malformed"
    tmp=$(mktemp "$secret_dir/.${provider}.XXXXXX")
    backup=$(mktemp "$secret_dir/.${provider}.backup.XXXXXX")
    trap 'rm -f -- "$tmp" "$backup"; unset secret' RETURN
    umask 077
    printf '%s=%s\n' "$var" "$secret" > "$tmp"
    unset secret
    chown root:jarvis "$tmp"
    chmod 0640 "$tmp"
    if [[ -e $file ]]; then
        [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file") == root:jarvis:640 ]] || fail "existing credential permissions are unsafe"
        cp --preserve=mode,ownership "$file" "$backup"
        had_backup=yes
    fi
    mv -f -- "$tmp" "$file"
    restart_or_rollback "$file" "$backup" "$had_backup" || fail "credential change rolled back"
    rm -f -- "$backup"
    trap - RETURN
    echo "jarvis-credentials: $provider is configured; no credential value was displayed."
}

list_credentials() {
    local provider file configured
    printf '%-16s %-12s %s\n' PROVIDER CONFIGURED STATUS
    for provider in anthropic openai deepseek xai zai ollama-cloud; do
        file=$(credential_file "$provider")
        configured=no
        [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file" 2>/dev/null || true) == root:jarvis:640 ]] && configured=yes
        printf '%-16s %-12s %s\n' "$provider" "$configured" \
            "$( [[ $configured == yes ]] && echo configured || echo not-configured )"
    done
    printf '%-16s %-12s %s\n' ollama-local n/a "no credential required"
}

test_credential() {
    local provider=$1 file
    [[ $provider != ollama ]] || fail "local Ollama has no credential; check its loopback service instead"
    provider_var "$provider" >/dev/null || fail "unknown provider"
    file=$(credential_file "$provider")
    [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file") == root:jarvis:640 ]] || fail "not configured or unsafe permissions"
    # Do not source the credential or call a paid generation endpoint here.
    # A bounded Core health check proves file loading/restart viability; provider
    # discovery/health belongs to the authenticated router and is non-secret.
    if ! systemctl is-active --quiet jarvis-core.service || ! wait_healthy; then
        fail "Core is not healthy"
    fi
    echo "jarvis-credentials: $provider is installed with safe permissions; no paid request was made."
}

remove_credential() {
    local provider=$1 file backup
    [[ $provider != ollama ]] || fail "local Ollama has no credential to remove"
    provider_var "$provider" >/dev/null || fail "unknown provider"
    require_tty
    file=$(credential_file "$provider")
    [[ -e $file ]] || fail "not configured"
    [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file") == root:jarvis:640 ]] || fail "credential permissions are unsafe"
    printf 'Remove %s credential and restart Core? [y/N] ' "$provider" >/dev/tty
    local answer
    IFS= read -r answer </dev/tty || fail "could not read confirmation"
    [[ $answer == y || $answer == Y || $answer == yes ]] || { echo "jarvis-credentials: unchanged."; return; }
    backup=$(mktemp "$secret_dir/.${provider}.backup.XXXXXX")
    cp --preserve=mode,ownership "$file" "$backup"
    rm -f -- "$file"
    restart_or_rollback "$file" "$backup" yes || fail "credential removal rolled back"
    rm -f -- "$backup"
    echo "jarvis-credentials: $provider was removed."
}

case ${1:-} in
    set) (($# == 2)) || usage; set_credential "$2" ;;
    list) (($# == 1)) || usage; list_credentials ;;
    test) (($# == 2)) || usage; test_credential "$2" ;;
    remove) (($# == 2)) || usage; remove_credential "$2" ;;
    *) usage ;;
esac
