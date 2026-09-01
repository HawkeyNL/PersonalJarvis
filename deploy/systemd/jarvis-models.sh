#!/usr/bin/env bash
# Deprecated compatibility helper behind the typed Rust `sudo jarvis models`
# interface.  This command intentionally never reads
# or displays provider credentials; Core receives only the resulting protected
# JSON policy through its normal root:jarvis read-only configuration boundary.
set -euo pipefail

readonly policy_file=/etc/jarvis/model-policy.json
readonly policy_dir=/etc/jarvis
readonly core_env=/etc/jarvis/core.env
readonly ollama_cloud_default_base_url=https://ollama.com/v1
readonly ollama_cloud_tags_url=https://ollama.com/api/tags

fail() { echo "jarvis-models: $*" >&2; exit 1; }

usage() {
    cat >&2 <<'EOF'
Usage:
  sudo jarvis-models refresh [provider]
  sudo jarvis-models list [provider]
  sudo jarvis-models enable <provider> <model>
  sudo jarvis-models disable <provider> <model>
  sudo jarvis-models show <provider> <model>

`refresh` records configured models as discovered but leaves every remote or
subscription-backed model disabled. Local Ollama remains enabled by default;
all model choices remain exact provider/model matches.
EOF
    exit 64
}

valid_provider() {
    [[ $1 =~ ^(anthropic-api|openai-api|deepseek-api|xai-api|zai-api|ollama|ollama-cloud|claude-cli)$ ]]
}

# Older pre-release installs could leave /etc/jarvis or model-policy.json with
# the more restrictive root:root ownership.  That is not an escalation, but it
# prevents Core/Admin from traversing/reading the policy. Normalize only this
# exact known-safe legacy state. Symlinks, non-root ownership, writable modes,
# malformed policy data or any other surprising state still fail closed.
normalize_model_policy_boundary() {
    local dir_state policy_state
    [[ -d $policy_dir && ! -L $policy_dir ]] || fail "policy directory is unavailable or unsafe"
    dir_state=$(stat -c '%U:%G:%a' "$policy_dir")
    case $dir_state in
        root:jarvis:750) ;;
        root:root:750)
            chown root:jarvis "$policy_dir"
            ;;
        *) fail "policy directory permissions are unsafe" ;;
    esac

    [[ -e $policy_file || -L $policy_file ]] || return 0
    [[ -f $policy_file && ! -L $policy_file ]] || fail "policy file is not a safe regular file"
    policy_state=$(stat -c '%U:%G:%a' "$policy_file")
    case $policy_state in
        root:jarvis:640) ;;
        root:root:600|root:root:640|root:jarvis:600)
            jq -e '.version == 1 and (.models | type == "array")' "$policy_file" >/dev/null ||
                fail "legacy policy is malformed; refusing permission migration"
            chown root:jarvis "$policy_file"
            chmod 0640 "$policy_file"
            ;;
        *) fail "policy permissions are unsafe" ;;
    esac
}

atomic_write() {
    local content=$1 tmp
    normalize_model_policy_boundary
    tmp=$(mktemp "$policy_dir/.model-policy.XXXXXX")
    trap 'rm -f -- "$tmp"' RETURN
    umask 077
    printf '%s\n' "$content" > "$tmp"
    chown root:jarvis "$tmp"
    chmod 0640 "$tmp"
    jq -e '.version == 1 and (.models | type == "array")' "$tmp" >/dev/null
    mv -f -- "$tmp" "$policy_file"
    trap - RETURN
}

empty_policy() { printf '%s\n' '{"version":1,"models":[]}'; }

require_policy() {
    normalize_model_policy_boundary
    [[ -f $policy_file && ! -L $policy_file ]] || fail "no policy; run 'sudo jarvis-models refresh' first"
    [[ $(stat -c '%U:%G:%a' "$policy_file") == root:jarvis:640 ]] || fail "policy permissions are unsafe"
    jq -e '.version == 1 and (.models | type == "array")' "$policy_file" >/dev/null || fail "policy is malformed"
}

configured_models() {
    # `core.env` may contain database credentials.  It is sourced only by root
    # in this process and none of its values are printed or passed as argv.
    [[ -f $core_env && ! -L $core_env ]] || fail "missing protected Core configuration"
    set -a
    # shellcheck source=/etc/jarvis/core.env
    # shellcheck disable=SC1091
    source "$core_env"
    set +a
    jq -n \
        --arg anthropic_default "${JARVIS_LLM_MODEL:-}" \
        --arg anthropic_hard "${JARVIS_LLM_MODEL_HARD:-}" \
        --arg anthropic_cheap "${JARVIS_LLM_MODEL_CHEAP:-}" \
        --arg openai_default "${JARVIS_LLM_OPENAI_MODEL:-}" \
        --arg openai_hard "${JARVIS_LLM_OPENAI_MODEL_HARD:-}" \
        --arg openai_cheap "${JARVIS_LLM_OPENAI_MODEL_CHEAP:-}" \
        --arg deepseek_default "${JARVIS_LLM_DEEPSEEK_MODEL:-}" \
        --arg deepseek_hard "${JARVIS_LLM_DEEPSEEK_MODEL_HARD:-}" \
        --arg deepseek_cheap "${JARVIS_LLM_DEEPSEEK_MODEL_CHEAP:-}" \
        --arg xai_default "${JARVIS_LLM_XAI_MODEL:-}" \
        --arg xai_hard "${JARVIS_LLM_XAI_MODEL_HARD:-}" \
        --arg xai_cheap "${JARVIS_LLM_XAI_MODEL_CHEAP:-}" \
        --arg zai_default "${JARVIS_LLM_ZAI_MODEL:-}" \
        --arg zai_hard "${JARVIS_LLM_ZAI_MODEL_HARD:-}" \
        --arg zai_cheap "${JARVIS_LLM_ZAI_MODEL_CHEAP:-}" \
        --arg ollama_cloud_default "${JARVIS_LLM_OLLAMA_CLOUD_MODEL:-}" \
        --arg ollama_cloud_hard "${JARVIS_LLM_OLLAMA_CLOUD_MODEL_HARD:-}" \
        --arg ollama_cloud_cheap "${JARVIS_LLM_OLLAMA_CLOUD_MODEL_CHEAP:-}" \
        --arg ollama "${JARVIS_LLM_OLLAMA_MODEL:-}" \
        '[
          ["anthropic-api", $anthropic_default], ["anthropic-api", $anthropic_hard], ["anthropic-api", $anthropic_cheap],
          ["openai-api", $openai_default], ["openai-api", $openai_hard], ["openai-api", $openai_cheap],
          ["deepseek-api", $deepseek_default], ["deepseek-api", $deepseek_hard], ["deepseek-api", $deepseek_cheap],
          ["xai-api", $xai_default], ["xai-api", $xai_hard], ["xai-api", $xai_cheap],
          ["zai-api", $zai_default], ["zai-api", $zai_hard], ["zai-api", $zai_cheap],
          ["ollama-cloud", $ollama_cloud_default], ["ollama-cloud", $ollama_cloud_hard], ["ollama-cloud", $ollama_cloud_cheap],
          ["ollama", $ollama]
        ] | map(select(.[1] != "")) | unique'
}

provider_secret_var() {
    case $1 in
        openai-api) printf '%s\n' JARVIS_LLM_OPENAI_API_KEY ;;
        deepseek-api) printf '%s\n' JARVIS_LLM_DEEPSEEK_API_KEY ;;
        xai-api) printf '%s\n' JARVIS_LLM_XAI_API_KEY ;;
        zai-api) printf '%s\n' JARVIS_LLM_ZAI_API_KEY ;;
        ollama-cloud) printf '%s\n' JARVIS_LLM_OLLAMA_CLOUD_API_KEY ;;
        *) return 1 ;;
    esac
}

provider_base_url() {
    case $1 in
        openai-api) printf '%s\n' "${JARVIS_LLM_OPENAI_BASE_URL:-https://api.openai.com/v1}" ;;
        deepseek-api) printf '%s\n' "${JARVIS_LLM_DEEPSEEK_BASE_URL:-https://api.deepseek.com/v1}" ;;
        xai-api) printf '%s\n' "${JARVIS_LLM_XAI_BASE_URL:-https://api.x.ai/v1}" ;;
        zai-api) printf '%s\n' "${JARVIS_LLM_ZAI_BASE_URL:-https://api.z.ai/api/paas/v4}" ;;
        ollama-cloud) printf '%s\n' "${JARVIS_LLM_OLLAMA_CLOUD_BASE_URL:-$ollama_cloud_default_base_url}" ;;
        *) return 1 ;;
    esac
}

# Convert the provider response to newline-delimited JSON arrays. Keep this
# transformation separate from network access so the exact parser/aggregation
# pipeline can be exercised without credentials or a live provider.
parse_remote_model_response() {
    local provider=$1 response=$2 jq_filter
    case $provider in
        ollama-cloud) jq_filter='.models[]?.name?' ;;
        openai-api|deepseek-api|xai-api|zai-api) jq_filter='.data[]?.id?' ;;
        *) return 1 ;;
    esac
    jq -c --arg provider "$provider" \
        "$jq_filter | select(type == \"string\" and length > 0 and length <= 256) | [\$provider, .]" \
        <<<"$response" || true
}

aggregate_discovered_models() {
    jq -s '
      map(select(
        type == "array"
        and length == 2
        and all(.[]; type == "string" and length > 0)
      ))
      | map(. + ["provider_api"])'
}

merge_model_policy() {
    local old=$1 known=$2
    jq -n --argjson old "$old" --argjson known "$known" '
      reduce $known[] as $item ($old;
        if any(.models[]; .provider == $item[0] and .model == $item[1]) then .
        else .models += [{provider:$item[0], model:$item[1], enabled:($item[0] == "ollama"), source:($item[2] // "configured")}]
        end)
      | .version = 1
      | .models |= sort_by(.provider, .model)'
}

# Discover models through provider metadata only. Credentials live only in an
# ephemeral mode-0600 curl config, never in argv/output/policy. OpenAI-compatible
# providers expose `data[].id`; Ollama Cloud exposes its native authenticated
# `/api/tags` metadata as `models[].name` even though chat uses `/v1`.
discover_remote_models() {
    local provider=$1 variable credential_file key config response url
    variable=$(provider_secret_var "$provider") || return 0
    credential_file="/etc/jarvis/secrets/${provider%-api}.env"
    [[ $provider != ollama-cloud ]] || credential_file=/etc/jarvis/secrets/ollama-cloud.env
    [[ -f $credential_file && ! -L $credential_file ]] || return 0
    set -a
    # shellcheck disable=SC1090,SC1091 # root-managed credential input
    source "$credential_file"
    set +a
    key=${!variable:-}
    [[ -n $key ]] || return 0
    config=$(mktemp /run/jarvis-model-discovery.XXXXXX)
    trap 'rm -f -- "$config"; unset key' RETURN
    umask 077
    if [[ $provider == ollama-cloud ]]; then
        url=$ollama_cloud_tags_url
    else
        local base
        base=$(provider_base_url "$provider")
        [[ -n $base && $base =~ ^https://[A-Za-z0-9._:/-]+$ ]] || return 0
        url="${base%/}/models"
    fi
    printf 'url = "%s"\nheader = "Authorization: Bearer %s"\n' "$url" "$key" > "$config"
    unset key
    response=$(curl --config "$config" --fail --silent --show-error --max-time 10 2>/dev/null) || {
        trap - RETURN
        rm -f -- "$config"
        return 0
    }
    trap - RETURN
    rm -f -- "$config"
    parse_remote_model_response "$provider" "$response"
}

refresh() {
    local provider=${1:-} old known discovered merged
    [[ -z $provider ]] || valid_provider "$provider" || fail "unknown provider"
    normalize_model_policy_boundary
    old=$(if [[ -f $policy_file ]]; then cat "$policy_file"; else empty_policy; fi)
    jq -e '.version == 1 and (.models | type == "array")' <<<"$old" >/dev/null || fail "existing policy is malformed"
    known=$(configured_models)
    if [[ -n $provider ]]; then
        known=$(jq --arg provider "$provider" '[.[] | select(.[0] == $provider)]' <<<"$known")
    fi
    discovered='[]'
    for candidate in openai-api deepseek-api xai-api zai-api ollama-cloud; do
        [[ -z $provider || $provider == "$candidate" ]] || continue
        discovered=$(discover_remote_models "$candidate" | aggregate_discovered_models 2>/dev/null || printf '[]')
        if [[ -n $provider && $provider == "$candidate" && $discovered == '[]' ]]; then
            fail "$candidate model discovery returned no models; credential or provider metadata endpoint is unavailable"
        fi
        [[ $discovered != '[]' ]] && known=$(jq -n --argjson known "$known" --argjson discovered "$discovered" '$known + $discovered | unique')
    done
    # Retain all existing records (including disabled discovered models) across
    # best-effort all-provider refresh failures. New remote entries are disabled.
    # Explicit remote-provider refreshes above fail instead of reporting false success.
    merged=$(merge_model_policy "$old" "$known")
    atomic_write "$merged"
    echo "jarvis-models: refreshed policy; new remote models remain disabled."
}

list_models() {
    require_policy
    local provider=${1:-}
    if [[ -n $provider ]]; then valid_provider "$provider" || fail "unknown provider"; fi
    printf '%-16s %-36s %-8s %s\n' PROVIDER MODEL ENABLED SOURCE
    jq -r --arg provider "$provider" '
      .models[] | select($provider == "" or .provider == $provider) |
      [.provider, .model, (if .enabled then "yes" else "no" end), .source] | @tsv' "$policy_file" |
      while IFS=$'\t' read -r provider_id model enabled source; do
        printf '%-16s %-36s %-8s %s\n' "$provider_id" "$model" "$enabled" "$source"
      done
}

set_state() {
    local provider=$1 model=$2 enabled=$3 updated
    valid_provider "$provider" || fail "unknown provider"
    [[ -n $model && ${#model} -le 256 && $model != *$'\n'* ]] || fail "invalid model"
    require_policy
    jq -e --arg provider "$provider" --arg model "$model" \
        'any(.models[]; .provider == $provider and .model == $model)' "$policy_file" >/dev/null ||
        fail "model is not discovered; run refresh before changing access"
    updated=$(jq --arg provider "$provider" --arg model "$model" --argjson enabled "$enabled" \
        '(.models[] | select(.provider == $provider and .model == $model) | .enabled) = $enabled' "$policy_file")
    atomic_write "$updated"
    echo "jarvis-models: $provider/$model is now $( [[ $enabled == true ]] && echo enabled || echo disabled )."
    systemctl try-restart jarvis-core.service >/dev/null 2>&1 || true
}

show_model() {
    local provider=$1 model=$2
    valid_provider "$provider" || fail "unknown provider"
    require_policy
    jq -e --arg provider "$provider" --arg model "$model" \
        '.models[] | select(.provider == $provider and .model == $model)' "$policy_file"
}

main() {
    [[ ${EUID} -eq 0 ]] || fail "must run as root"
    command -v jq >/dev/null 2>&1 || fail "jq is required"
    command -v curl >/dev/null 2>&1 || fail "curl is required"

    case ${1:-} in
        refresh) (($# == 1 || $# == 2)) || usage; refresh "${2:-}" ;;
        list) (($# == 1 || $# == 2)) || usage; list_models "${2:-}" ;;
        enable) (($# == 3)) || usage; set_state "$2" "$3" true ;;
        disable) (($# == 3)) || usage; set_state "$2" "$3" false ;;
        show) (($# == 3)) || usage; show_model "$2" "$3" ;;
        *) usage ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
