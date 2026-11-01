#!/usr/bin/env bash
# Deprecated compatibility helper behind the typed Rust `sudo jarvis credentials`
# interface. Values are accepted only from the
# controlling TTY and are never rendered, logged, or supplied in argv.
set -euo pipefail

readonly secret_dir=/etc/jarvis/secrets
readonly core_env=/etc/jarvis/core.env
readonly ollama_cloud_default_base_url=https://ollama.com/v1
readonly ollama_cloud_tags_url=https://ollama.com/api/tags
readonly huggingface_default_base_url=https://router.huggingface.co/v1

fail() { echo "jarvis-credentials: $*" >&2; exit 1; }

usage() {
    cat >&2 <<'EOF'
Usage: sudo jarvis-credentials <set|list|test|remove> [provider]
       sudo jarvis-credentials <set|status> ibkr-oauth

Providers: anthropic, openai, deepseek, xai, zai, ollama-cloud, huggingface, jev.
Local Ollama has no credential; configure its loopback URL/model in core.env.
ibkr-oauth: the read-only IBKR broker; never readable by Core.
EOF
    exit 64
}

# IBKR OAuth for the read-only broker. The values sit beside the owner-made
# keys in a root-only directory; systemd hands them to the broker with
# LoadCredential=. Core never reads them. Tests may point these elsewhere.
ibkr_oauth_dir=$secret_dir/ibkr-oauth
ibkr_root_uid=0
readonly -a ibkr_oauth_items=(consumer_key access_token access_token_secret private_signature.pem dhparam.pem)

# Regular, not a link, non-empty, root-owned, no group/other permissions.
ibkr_private_ok() {
    local metadata
    [[ -e $1 && ! -L $1 ]] || return 1
    metadata=$(stat -c '%u:%a' "$1") || return 1
    [[ ${metadata%%:*} == "$ibkr_root_uid" ]] && (( (8#${metadata#*:} & 077) == 0 ))
}

ibkr_read_hidden() {
    local value
    printf 'Enter IBKR %s (input hidden): ' "$1" >/dev/tty
    IFS= read -r -s value </dev/tty || return 1
    printf '\n' >/dev/tty
    printf '%s' "$value"
}

# Base64 ciphertext on stdin, lowercase hex of the secret on stdout. Implicit
# rejection is off: OpenSSL 3.2+ otherwise answers a wrong key or damaged
# ciphertext with random bytes instead of an error.
ibkr_decrypt_secret() {
    base64 -d 2>/dev/null |
        openssl pkeyutl -decrypt -inkey "$1" -pkeyopt rsa_padding_mode:pkcs1 \
            -pkeyopt rsa_pkcs1_implicit_rejection:0 2>/dev/null |
        od -An -v -tx1 | tr -d ' \n'
}

ensure_ibkr_identity() {
    getent passwd jarvis-ibkr >/dev/null ||
        useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin jarvis-ibkr
}

# NAME VALUE ...: stage every file root-only 0400, then rename all in place.
ibkr_install_values() (
    local -a names=() staged=()
    local tmp index
    trap 'rm -f -- "${staged[@]}"' EXIT
    while (($# >= 2)); do
        tmp=$(mktemp "$ibkr_oauth_dir/.$1.XXXXXX") || return 1
        staged+=("$tmp")
        names+=("$1")
        printf '%s\n' "$2" > "$tmp" && chown root:root "$tmp" && chmod 0400 "$tmp" || return 1
        shift 2
    done
    for index in "${!names[@]}"; do
        mv -Tf -- "${staged[$index]}" "$ibkr_oauth_dir/${names[$index]}" || return 1
    done
)

set_ibkr_oauth() (
    local consumer_key access_token encrypted prepend key
    require_tty
    command -v openssl >/dev/null 2>&1 && command -v base64 >/dev/null 2>&1 ||
        fail "openssl and base64 are required"
    [[ -d $ibkr_oauth_dir ]] && ibkr_private_ok "$ibkr_oauth_dir" ||
        fail "$ibkr_oauth_dir must be a root-only directory holding the generated keys"
    for key in private_encryption.pem private_signature.pem dhparam.pem; do
        [[ -f $ibkr_oauth_dir/$key && -s $ibkr_oauth_dir/$key ]] && ibkr_private_ok "$ibkr_oauth_dir/$key" ||
            fail "$key is missing or not root-only in $ibkr_oauth_dir"
    done
    trap 'unset consumer_key access_token encrypted prepend' EXIT
    umask 077
    consumer_key=$(ibkr_read_hidden "consumer key") || fail "could not read the consumer key"
    [[ $consumer_key =~ ^[A-Z0-9]{1,32}$ ]] || fail "the consumer key must be 1-32 uppercase letters or digits"
    access_token=$(ibkr_read_hidden "access token") || fail "could not read the access token"
    [[ $access_token =~ ^[A-Za-z0-9]{1,128}$ ]] || fail "the access token must be 1-128 letters or digits"
    encrypted=$(ibkr_read_hidden "access token secret (encrypted, base64)") ||
        fail "could not read the access token secret"
    [[ $encrypted =~ ^[A-Za-z0-9+/]+={0,2}$ && ${#encrypted} -le 4096 ]] ||
        fail "the access token secret must be the base64 text shown by IBKR"
    prepend=$(printf '%s' "$encrypted" | ibkr_decrypt_secret "$ibkr_oauth_dir/private_encryption.pem") &&
        [[ $prepend =~ ^[0-9a-f]+$ ]] ||
        fail "the access token secret does not decrypt with private_encryption.pem (wrong key, damaged text, or OpenSSL older than 3.2); nothing was changed"
    ensure_ibkr_identity || fail "could not create the jarvis-ibkr service identity"
    ibkr_install_values consumer_key "$consumer_key" access_token "$access_token" \
        access_token_secret "$prepend" || fail "could not save the IBKR credentials"
    # A running broker loads the new values; a stopped one stays stopped.
    systemctl try-restart jarvis-ibkr-broker.service >/dev/null 2>&1 || true
    echo "jarvis-credentials: IBKR OAuth credentials saved. Enable the broker with: sudo systemctl enable --now jarvis-ibkr-broker.service"
)

ibkr_oauth_status() {
    local item state
    for item in "${ibkr_oauth_items[@]}"; do
        state=empty
        [[ -f $ibkr_oauth_dir/$item && -s $ibkr_oauth_dir/$item ]] && ibkr_private_ok "$ibkr_oauth_dir/$item" && state=set
        printf '%-24s %s\n' "$item" "$state"
    done
}

provider_var() {
    case $1 in
        anthropic) printf '%s\n' JARVIS_LLM_API_KEY ;;
        openai) printf '%s\n' JARVIS_LLM_OPENAI_API_KEY ;;
        deepseek) printf '%s\n' JARVIS_LLM_DEEPSEEK_API_KEY ;;
        xai) printf '%s\n' JARVIS_LLM_XAI_API_KEY ;;
        zai) printf '%s\n' JARVIS_LLM_ZAI_API_KEY ;;
        ollama-cloud) printf '%s\n' JARVIS_LLM_OLLAMA_CLOUD_API_KEY ;;
        huggingface) printf '%s\n' JARVIS_LLM_HUGGINGFACE_API_KEY ;;
        jev) printf '%s\n' JARVIS_LLM_JEV_API_KEY ;;
        *) return 1 ;;
    esac
}

credential_file() {
    provider_var "$1" >/dev/null || return 1
    printf '%s/%s.env\n' "$secret_dir" "$1"
}

read_credential_value() {
    local file=$1 variable=$2 line
    [[ $(wc -l < "$file") -eq 1 ]] || return 1
    IFS= read -r line < "$file" || [[ -n $line ]] || return 1
    [[ $line == "$variable="* ]] || return 1
    printf '%s' "${line#*=}"
}

require_tty() {
    [[ -t 0 && -t 1 && -r /dev/tty && -w /dev/tty ]] ||
        fail "requires a controlling TTY; secrets are never accepted through argv/stdin"
}

ensure_core_env_setting() {
    local key=$1 value=$2 count existing tmp
    [[ $key =~ ^JARVIS_[A-Z0-9_]+$ && -n $value && $value != *$'\n'* ]] ||
        fail "refusing malformed protected Core configuration default"
    [[ -f $core_env && ! -L $core_env ]] || fail "missing protected Core configuration"
    [[ $(stat -c '%U:%G:%a' "$core_env") == root:jarvis:640 ]] ||
        fail "protected Core configuration permissions are unsafe"
    count=$(grep -c "^${key}=" "$core_env" || true)
    ((count <= 1)) || fail "protected Core configuration contains duplicate $key"
    if ((count == 1)); then
        existing=$(sed -n "s/^${key}=//p" "$core_env")
        [[ -n $existing ]] && return 0
    fi
    tmp=$(mktemp /etc/jarvis/.core.env.provider.XXXXXX)
    trap 'rm -f -- "$tmp"' RETURN
    umask 077
    awk -v key="$key" -v value="$value" '
        BEGIN { replaced = 0 }
        index($0, key "=") == 1 {
            if (!replaced) {
                print key "=" value
                replaced = 1
            }
            next
        }
        { print }
        END {
            if (!replaced) print key "=" value
        }
    ' "$core_env" > "$tmp"
    chown root:jarvis "$tmp"
    chmod 0640 "$tmp"
    mv -f -- "$tmp" "$core_env"
    trap - RETURN
}

ensure_provider_defaults() {
    case $1 in
        ollama-cloud)
            ensure_core_env_setting JARVIS_LLM_OLLAMA_CLOUD_BASE_URL "$ollama_cloud_default_base_url"
            ;;
        huggingface)
            ensure_core_env_setting JARVIS_LLM_HUGGINGFACE_BASE_URL "$huggingface_default_base_url"
            ;;
    esac
}

wait_healthy() {
    local _attempt
    for _attempt in {1..20}; do
        if systemctl is-active --quiet jarvis-core.service \
            && curl --fail --silent --output /dev/null --max-time 2 http://127.0.0.1:8080/livez 2>/dev/null \
            && curl --fail --silent --output /dev/null --max-time 2 http://127.0.0.1:8080/readyz 2>/dev/null; then
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
    echo "jarvis-credentials: inspect 'sudo jarvis health' for recovery status." >&2
    return 1
}

set_credential() (
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
    trap 'rm -f -- "$tmp"; unset secret' EXIT
    umask 077
    printf '%s=%s\n' "$var" "$secret" > "$tmp"
    unset secret
    install_credential_candidate "$provider" "$tmp"
)

# Called only with a hidden-input temporary, never with a caller-selected path.
# Keep the old credential untouched until the metadata-only probe succeeds.
install_credential_candidate() (
    local provider=$1 tmp=$2 file backup='' had_backup=no
    file=$(credential_file "$provider")
    trap 'rm -f -- "$tmp"; [[ -z $backup ]] || rm -f -- "$backup"' EXIT
    echo "jarvis-credentials: checking $provider credential; no generation request is made."
    if ! probe_provider "$provider" "$tmp"; then
        fail "$provider credential could not be verified (rejected, unavailable, or invalid metadata); previous credential unchanged"
    fi
    ensure_provider_defaults "$provider"
    backup=$(mktemp "$secret_dir/.${provider}.backup.XXXXXX")
    if [[ -e $file || -L $file ]]; then
        [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file") == root:jarvis:640 ]] || fail "existing credential permissions are unsafe"
        cp --preserve=mode,ownership "$file" "$backup"
        had_backup=yes
    fi
    chown root:jarvis "$tmp"
    chmod 0640 "$tmp"
    mv -f -- "$tmp" "$file"
    restart_or_rollback "$file" "$backup" "$had_backup" || fail "credential change rolled back"
    rm -f -- "$backup"
    echo "jarvis-credentials: $provider credential probe and Core health check succeeded; credential saved."
)

list_credentials() {
    local provider file configured
    printf '%-16s %-12s %s\n' PROVIDER CONFIGURED STATUS
    for provider in anthropic openai deepseek xai zai ollama-cloud huggingface jev; do
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
    ensure_provider_defaults "$provider"
    file=$(credential_file "$provider")
    [[ -f $file && ! -L $file && $(stat -c '%U:%G:%a' "$file") == root:jarvis:640 ]] || fail "not configured or unsafe permissions"
    if ! probe_provider "$provider" "$file"; then
        fail "$provider credential probe failed; provider rejected or did not answer the bounded metadata request"
    fi
    if ! systemctl is-active --quiet jarvis-core.service || ! wait_healthy; then
        fail "Core is not healthy"
    fi
    echo "jarvis-credentials: $provider credential probe and Core health check succeeded; no generation request was made."
}

# Authenticated, read-only provider probe. The key is written only into an
# ephemeral mode-0600 curl config in /run, never onto curl's argv or stdout.
# Every endpoint below is metadata-only and must not create a paid generation.
probe_provider() (
    local provider=$1 file=$2 variable key escaped_key base config http_code url response_file=''
    command -v curl >/dev/null 2>&1 || fail "curl is required for credential testing"
    variable=$(provider_var "$provider") || return 1
    key=$(read_credential_value "$file" "$variable") || return 1
    [[ -n $key ]] || return 1
    escaped_key=$(curl_config_escape "$key")
    config=$(mktemp /run/jarvis-credential-test.XXXXXX)
    trap 'rm -f -- "$config"; [[ -z ${response_file:-} ]] || rm -f -- "$response_file"; unset key escaped_key' EXIT
    umask 077
    case $provider in
        anthropic)
            base=https://api.anthropic.com
            [[ -f $core_env && ! -L $core_env ]] && {
                # shellcheck disable=SC1090,SC1091 # root-managed Core config
                source "$core_env"
                base=${JARVIS_LLM_ANTHROPIC_BASE_URL:-$base}
            }
            [[ $base =~ ^https://[A-Za-z0-9._:/-]+$ ]] || return 1
            printf 'url = "%s/v1/models?limit=1"\nheader = "x-api-key: %s"\nheader = "anthropic-version: 2023-06-01"\n' "${base%/}" "$escaped_key" > "$config"
            ;;
        ollama-cloud)
            # Ollama Cloud chat is OpenAI-compatible at /v1, but authenticated
            # account/model metadata is served through the native /api/tags route.
            url=$ollama_cloud_tags_url
            printf 'url = "%s"\nheader = "Authorization: Bearer %s"\n' "$url" "$escaped_key" > "$config"
            ;;
        jev)
            printf 'url = "https://api.typesafe.ai/v1/models"\nheader = "Authorization: Bearer %s"\n' "$escaped_key" > "$config"
            ;;
        openai|deepseek|xai|zai|huggingface)
            base=$(openai_compatible_base_url "$provider") || return 1
            [[ $base =~ ^https://[A-Za-z0-9._:/-]+$ ]] || return 1
            printf 'url = "%s/models"\nheader = "Authorization: Bearer %s"\n' "${base%/}" "$escaped_key" > "$config"
            ;;
        *) return 1 ;;
    esac
    unset key escaped_key
    response_file=$(mktemp /run/jarvis-credential-response.XXXXXX)
    chmod 0600 "$response_file"
    # Do not echo provider bodies or curl diagnostics: either can contain
    # attacker-controlled data, including a reflected Authorization value.
    http_code=$(curl --config "$config" --output "$response_file" --silent --max-time 10 --max-filesize 8388608 --write-out '%{http_code}' 2>/dev/null) || return 1
    [[ $http_code =~ ^2[0-9]{2}$ ]] || return 1
    if [[ $provider == ollama-cloud ]]; then
        jq -e '(.models | type == "array") and any(.models[]?; (.name | type == "string") and (.name | length > 0))' "$response_file" >/dev/null 2>&1
    elif [[ $provider == jev ]]; then
        valid_jev_model_response "$response_file" 2>/dev/null
    else
        valid_huggingface_model_response "$response_file" 2>/dev/null
    fi
)

valid_huggingface_model_response() {
    jq -e '(.data | type == "array") and any(.data[]?; (.id | type == "string") and (.id | length > 0) and (.id | length <= 256) and (.id | test("[[:cntrl:]]") | not))' "$1" >/dev/null
}

valid_jev_model_response() {
    jq -e '(.models | type == "array") and any(.models[]?; (.name | type == "string") and (.name | test("^[A-Za-z0-9._:/-]{1,128}$")))' "$1" >/dev/null
}

# curl config uses quoted values. Escape the only two metacharacters that may
# occur in an opaque provider credential without ever echoing it to a terminal.
curl_config_escape() {
    local value=$1
    value=${value//\\/\\\\}
    value=${value//\"/\\\"}
    printf '%s' "$value"
}

openai_compatible_base_url() {
    local provider=$1
    [[ -f $core_env && ! -L $core_env ]] || return 1
    # shellcheck disable=SC1090,SC1091 # root-managed Core config
    source "$core_env"
    case $provider in
        openai) printf '%s\n' "${JARVIS_LLM_OPENAI_BASE_URL:-https://api.openai.com/v1}" ;;
        deepseek) printf '%s\n' "${JARVIS_LLM_DEEPSEEK_BASE_URL:-https://api.deepseek.com/v1}" ;;
        xai) printf '%s\n' "${JARVIS_LLM_XAI_BASE_URL:-https://api.x.ai/v1}" ;;
        zai) printf '%s\n' "${JARVIS_LLM_ZAI_BASE_URL:-https://api.z.ai/api/paas/v4}" ;;
        ollama-cloud) printf '%s\n' "${JARVIS_LLM_OLLAMA_CLOUD_BASE_URL:-$ollama_cloud_default_base_url}" ;;
        huggingface) printf '%s\n' "${JARVIS_LLM_HUGGINGFACE_BASE_URL:-$huggingface_default_base_url}" ;;
        *) return 1 ;;
    esac
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

main() {
    [[ ${EUID} -eq 0 ]] || fail "must run as root"
    case ${1:-} in
        set)
            (($# == 2)) || usage
            if [[ $2 == ibkr-oauth ]]; then set_ibkr_oauth; else set_credential "$2"; fi
            ;;
        status) (($# == 2)) && [[ $2 == ibkr-oauth ]] || usage; ibkr_oauth_status ;;
        list) (($# == 1)) || usage; list_credentials ;;
        test) (($# == 2)) || usage; test_credential "$2" ;;
        remove) (($# == 2)) || usage; remove_credential "$2" ;;
        *) usage ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
