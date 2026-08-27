#!/usr/bin/env bash
# Root-operated owner model allowlist.  This command intentionally never reads
# or displays provider credentials; Core receives only the resulting protected
# JSON policy through its normal root:jarvis read-only configuration boundary.
set -euo pipefail

readonly policy_file=/etc/jarvis/model-policy.json
readonly policy_dir=/etc/jarvis
readonly core_env=/etc/jarvis/core.env

fail() { echo "jarvis-models: $*" >&2; exit 1; }
[[ ${EUID} -eq 0 ]] || fail "must run as root"
command -v jq >/dev/null 2>&1 || fail "jq is required"

usage() {
    cat >&2 <<'EOF'
Usage:
  sudo jarvis-models refresh
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

atomic_write() {
    local content=$1 tmp
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
        --arg ollama "${JARVIS_LLM_OLLAMA_MODEL:-}" \
        '[
          ["anthropic-api", $anthropic_default], ["anthropic-api", $anthropic_hard], ["anthropic-api", $anthropic_cheap],
          ["openai-api", $openai_default], ["openai-api", $openai_hard], ["openai-api", $openai_cheap],
          ["deepseek-api", $deepseek_default], ["deepseek-api", $deepseek_hard], ["deepseek-api", $deepseek_cheap],
          ["ollama", $ollama]
        ] | map(select(.[1] != "")) | unique'
}

refresh() {
    local old known merged
    old=$(if [[ -f $policy_file ]]; then cat "$policy_file"; else empty_policy; fi)
    jq -e '.version == 1 and (.models | type == "array")' <<<"$old" >/dev/null || fail "existing policy is malformed"
    known=$(configured_models)
    # Retain all existing records (including disabled discovered models) across
    # refresh failures.  New remote entries are disabled.  Local Ollama is the
    # explicit offline default, not an accidental cloud authorization.
    merged=$(jq -n --argjson old "$old" --argjson known "$known" '
      reduce $known[] as $item ($old;
        if any(.models[]; .provider == $item[0] and .model == $item[1]) then .
        else .models += [{provider:$item[0], model:$item[1], enabled:($item[0] == "ollama"), source:"configured"}]
        end)
      | .version = 1
      | .models |= sort_by(.provider, .model)')
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

case ${1:-} in
    refresh) (($# == 1)) || usage; refresh ;;
    list) (($# == 1 || $# == 2)) || usage; list_models "${2:-}" ;;
    enable) (($# == 3)) || usage; set_state "$2" "$3" true ;;
    disable) (($# == 3)) || usage; set_state "$2" "$3" false ;;
    show) (($# == 3)) || usage; show_model "$2" "$3" ;;
    *) usage ;;
esac
