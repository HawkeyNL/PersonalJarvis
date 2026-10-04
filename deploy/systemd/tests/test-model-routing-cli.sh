#!/usr/bin/env bash
# Rootless fixture for `jarvis-models route`: no protected state, services or
# provider calls. A private copy of the helper points at a temp directory.
set -euo pipefail
trap 'echo "model routing CLI assertion failed at line $LINENO" >&2' ERR
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/etc/model-policy"
sed "s#/etc/jarvis#$fixture/etc#g" "$repo/deploy/systemd/jarvis-models.sh" > "$fixture/jarvis-models.sh"
# shellcheck source=/dev/null
source "$fixture/jarvis-models.sh"
[[ $routing_file == "$fixture/etc/model-policy/routing.json" ]]

# Rootless stand-ins: the fixture user plays root:jarvis; systemd is recorded.
protected_file_state() { printf 'root:jarvis:%s\n' "$(stat -c %a "$1")"; }
chown() { :; }
# Records each flushed file; it must still be the staged file, not yet renamed.
sync() { [[ -f ${!#} ]] && printf '%s\n' "${!#}" >> "$fixture/synced"; }
restart=ok
systemctl() {
    printf '%s\n' "$*" >> "$fixture/calls"
    [[ $1 == stop || $restart == ok ]]
}

cat > "$policy_file" <<'JSON'
{"version":1,"models":[
 {"provider":"zai-api","model":"glm-5.3-flash","enabled":false,"source":"fixture"},
 {"provider":"claude-cli","model":"claude-haiku-4-5","enabled":true,"source":"fixture"},
 {"provider":"claude-cli","model":"claude-opus-5","enabled":true,"source":"fixture"},
 {"provider":"anthropic-api","model":"claude-opus-5","enabled":false,"source":"fixture"},
 {"provider":"ollama","model":"llama3.2","enabled":true,"source":"fixture"}]}
JSON
chmod 0640 "$policy_file"

refused() {
    local expected=$1 before
    shift
    before=$(cat "$routing_file" 2>/dev/null || printf absent)
    : > "$fixture/calls"
    if ("$@") > "$fixture/out" 2>&1; then
        echo "accepted: $*" >&2; exit 1
    fi
    grep -Fq -- "$expected" "$fixture/out" || { cat "$fixture/out" >&2; exit 1; }
    [[ $(cat "$routing_file" 2>/dev/null || printf absent) == "$before" ]]
    [[ ! -s $fixture/calls ]]
    [[ $(find "$policy_dir" -name '.routing.*' | wc -l) == 0 ]]
}

# Absent file: built-in order everywhere, nothing written or restarted.
route_command list > "$fixture/out"
grep -Fxq 'paid API: allowed' "$fixture/out"
[[ $(grep -c 'built-in order' "$fixture/out") == 3 ]]
[[ ! -e $routing_file && ! -e $fixture/calls ]]
[[ $(route_command show hard) == '"built-in order"' ]]

# Set: exact chain, root:jarvis 0640, Core restarted to activate.
route_command set cheap zai-api glm-5.3-flash claude-cli claude-haiku-4-5 > "$fixture/out"
[[ $(stat -c %a "$routing_file") == 640 ]]
jq -e '.version == 1 and .paid_api == "allowed"
    and .tiers.cheap == {chain: [{provider: "zai-api", model: "glm-5.3-flash"},
                                 {provider: "claude-cli", model: "claude-haiku-4-5"}],
                         metered_after_subscription: false}
    and (.tiers | keys == ["cheap"])' "$routing_file" >/dev/null
[[ $(<"$fixture/calls") == 'try-restart jarvis-core.service' ]]
grep -q '/\.routing\.' "$fixture/synced"
route_command show cheap | jq -e '.chain | length == 2' >/dev/null
route_command list > "$fixture/out"
grep -Eq '^cheap +2 +claude-cli +claude-haiku-4-5$' "$fixture/out"

# The policy writer flushes its staged file before the rename too.
(normalize_model_policy_boundary() { :; }; atomic_write "$(<"$policy_file")")
grep -q '/\.model-policy\.' "$fixture/synced"

# Every safety rule refuses without touching the file or Core.
refused 'metered entry after a subscription' \
    route_command set default claude-cli claude-opus-5 anthropic-api claude-opus-5
refused 'not discovered' route_command set default ollama not-discovered
refused 'unknown provider' route_command set default jev fixture
refused 'unknown provider' route_command set default Claude-CLI claude-opus-5
refused 'unknown tier' route_command set turbo ollama llama3.2
refused 'Usage' route_command set default ollama
refused 'pairs' route_command set default ollama llama3.2 ollama
refused 'at most 9' route_command set default $(for i in $(seq 10); do printf 'ollama m%s ' "$i"; done)
refused 'invalid route' route_command set default ollama llama3.2 ollama llama3.2
refused 'invalid route' route_command set default ollama $'llama\t3.2'
refused 'invalid route' route_command set default ollama "$(printf 'm%.0s' $(seq 257))"
refused 'invalid route' route_command set default ollama ''
refused 'allowed or off' route_command paid-api maybe
refused 'on or off' route_command research-web-search maybe
refused 'on or off' route_command research-web-search ON
refused 'unknown tier' route_command reset turbo

# The owner can explicitly approve a paid fallback after a subscription.
route_command set default claude-cli claude-opus-5 anthropic-api claude-opus-5 \
    --metered-after-subscription > /dev/null
jq -e '.tiers.default.metered_after_subscription == true and (.tiers.default.chain | length == 2)' "$routing_file" >/dev/null

route_command paid-api off > /dev/null
jq -e '.paid_api == "off" and (.tiers | keys == ["cheap", "default"])' "$routing_file" >/dev/null
route_command reset cheap > /dev/null
jq -e '.paid_api == "off" and (.tiers | keys == ["default"])' "$routing_file" >/dev/null

# Research web search: off by default, written only when on, kept by edits.
route_command list > "$fixture/out"
grep -Fxq 'research web search: off' "$fixture/out"
jq -e 'has("research_web_search") | not' "$routing_file" >/dev/null
route_command research-web-search on > /dev/null
jq -e '.research_web_search == "on" and .paid_api == "off"' "$routing_file" >/dev/null
route_command list > "$fixture/out"
grep -Fxq 'research web search: on' "$fixture/out"
route_command reset default > /dev/null
jq -e '.research_web_search == "on" and .tiers == {}' "$routing_file" >/dev/null
route_command research-web-search off > /dev/null
jq -e 'has("research_web_search") | not' "$routing_file" >/dev/null
route_command set default claude-cli claude-opus-5 anthropic-api claude-opus-5 \
    --metered-after-subscription > /dev/null

# A failed activation stops Core instead of keeping an old route live.
restart=failed
: > "$fixture/calls"
if (route_command paid-api allowed) > "$fixture/out" 2>&1; then
    echo 'failed activation reported success' >&2; exit 1
fi
grep -Fq 'Core stopped safely' "$fixture/out"
grep -Fxq 'stop jarvis-core.service' "$fixture/calls"
restart=ok

# Unsafe or invalid existing files are never edited.
saved=$(<"$routing_file")
chmod 0660 "$routing_file"
refused 'permissions are unsafe' route_command paid-api off
chmod 0640 "$routing_file"
printf '%s\n' '{"version":1,"paid_api":"maybe"}' > "$routing_file"
refused 'routing is invalid' route_command reset default
refused 'routing is invalid' route_command list
printf '%s\n' "$saved" > "$routing_file"
mv "$routing_file" "$fixture/target.json"
ln -s "$fixture/target.json" "$routing_file"
refused 'not a safe regular file' route_command paid-api off
rm "$routing_file"
mv "$fixture/target.json" "$routing_file"
mv "$policy_file" "$fixture/policy.json"
refused 'no policy' route_command set default ollama llama3.2
mv "$fixture/policy.json" "$policy_file"

# Register: subscription pairs only, the worker's model rule, recorded
# disabled, written atomically and flushed, and Core is not restarted.
policy_unchanged_after_refusal() {
    local expected=$1 before
    shift
    before=$(<"$policy_file")
    if (normalize_model_policy_boundary() { :; }; register_subscription_model "$@") > "$fixture/out" 2>&1; then
        echo "accepted register: $*" >&2; exit 1
    fi
    grep -Fq -- "$expected" "$fixture/out" || { cat "$fixture/out" >&2; exit 1; }
    [[ $(<"$policy_file") == "$before" ]]
    [[ $(find "$policy_dir" -name '.model-policy.*' | wc -l) == 0 ]]
}
: > "$fixture/calls"
: > "$fixture/synced"
(normalize_model_policy_boundary() { :; }; register_subscription_model codex-cli gpt-6-luna) > "$fixture/out"
grep -Fq 'recorded as discovered and disabled' "$fixture/out"
jq -e '[.models[] | select(.provider == "codex-cli")]
    == [{provider: "codex-cli", model: "gpt-6-luna", enabled: false, source: "owner_registered"}]' "$policy_file" >/dev/null
[[ $(stat -c %a "$policy_file") == 640 ]]
grep -q '/\.model-policy\.' "$fixture/synced"
[[ ! -s $fixture/calls ]]
# The registered pair is now routable (still disabled until `enable`).
route_command set hard codex-cli gpt-6-luna claude-cli claude-opus-5 > /dev/null
jq -e '.tiers.hard.chain[0] == {provider: "codex-cli", model: "gpt-6-luna"}' "$routing_file" >/dev/null
# Registering an existing pair never changes its access.
before=$(<"$policy_file")
(normalize_model_policy_boundary() { :; }; register_subscription_model claude-cli claude-opus-5) | grep -Fq 'already discovered'
[[ $(<"$policy_file") == "$before" ]]
policy_unchanged_after_refusal 'only for subscription providers' openai-api gpt-6-luna
policy_unchanged_after_refusal 'only for subscription providers' ollama llama3.2
policy_unchanged_after_refusal 'only for subscription providers' Codex-CLI gpt-6-luna
for model in -c --model '' 'a b' org/model 'x;sh' modèl $'a\nb' "$(printf 'm%.0s' $(seq 81))"; do
    policy_unchanged_after_refusal 'invalid model' codex-cli "$model"
done
(normalize_model_policy_boundary() { :; }; register_subscription_model codex-cli "$(printf 'm%.0s' $(seq 80))") > /dev/null
mv "$policy_file" "$fixture/policy.json"
printf '%s\n' '{"version":2,"models":[]}' > "$policy_file"
policy_unchanged_after_refusal 'malformed' codex-cli gpt-6-luna
mv "$fixture/policy.json" "$policy_file"

# Refresh records the Claude tier models the claude-cli worker runs; it does
# not touch other providers when scoped to claude-cli.
printf '%s\n' 'JARVIS_LLM_MODEL=claude-sonnet-5' 'JARVIS_LLM_MODEL_CHEAP=claude-haiku-4-5' \
    'JARVIS_LLM_OPENAI_MODEL=gpt-fixture' > "$core_env"
(normalize_model_policy_boundary() { :; }; refresh claude-cli) > /dev/null
jq -e 'any(.models[]; . == {provider: "claude-cli", model: "claude-sonnet-5", enabled: false, source: "configured"})
    and any(.models[]; . == {provider: "claude-cli", model: "claude-haiku-4-5", enabled: true, source: "fixture"})
    and ([.models[] | select(.provider == "openai-api")] | length == 0)' "$policy_file" >/dev/null

# Validator parity with jarvis_llm::ModelRouting::parse.
long_model=$(printf 'm%.0s' $(seq 256))
nine=$(jq -cn '[range(9) | {provider: "ollama", model: ("m" + tostring)}]')
ten=$(jq -cn '[range(10) | {provider: "ollama", model: ("m" + tostring)}]')
chain() { jq -cn --argjson chain "$1" --argjson approved "${2:-false}" \
    '{version: 1, tiers: {default: {chain: $chain, metered_after_subscription: $approved}}}'; }
for document in '{"version":1}' '{"version":1,"paid_api":"off"}' '{"version":1,"tiers":{}}' \
    '{"version":1,"tiers":{"cheap":null}}' \
    '{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama","model":"a"}]}}}' \
    "$(chain '[{"provider":"claude-cli","model":"a"},{"provider":"anthropic-api","model":"a"}]' true)" \
    "$(chain '[{"provider":"anthropic-api","model":"a"},{"provider":"claude-cli","model":"a"}]')" \
    "$(chain '[{"provider":"claude-cli","model":"a"},{"provider":"ollama","model":"b"}]')" \
    "$(chain '[{"provider":"claude-cli","model":"a"},{"provider":"claude-cli","model":"b"}]')" \
    "$(chain '[{"provider":"codex-cli","model":"gpt-6-luna"},{"provider":"claude-cli","model":"a"}]')" \
    "$(chain "[{\"provider\":\"ollama\",\"model\":\"$long_model\"}]")" \
    "$(chain "$nine")" \
    "$(printf '{"version":1}%65522s' '')"; do
    valid_routing "$document" || { echo "rejected valid routing: ${document:0:120}" >&2; exit 1; }
done
for document in '' '[]' '{"tiers":{}}' '{"version":2}' '{"version":"1"}' '{"version":1.0}' \
    '{"version":1.00}' '{"version":1,"extra":true}' \
    '{"version":1,"paid_api":"maybe"}' '{"version":1,"paid_api":"OFF"}' '{"version":1,"paid_api":null}' \
    '{"version":1}{"version":1}' '{"version":1,"tiers":null}' \
    '{"version":1,"tiers":{"turbo":{"chain":[]}}}' \
    '{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama","model":"a","x":1}]}}}' \
    '{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama"}]}}}' \
    '{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama","model":"a"}],"y":1}}}' \
    '{"version":1,"tiers":{"default":{"chain":[{"provider":"ollama","model":"a"}],"metered_after_subscription":"yes"}}}' \
    "$(chain '[]')" "$(chain "$ten")" \
    "$(chain '[{"provider":"jev","model":"a"}]')" \
    "$(chain '[{"provider":"ollama","model":""}]')" \
    "$(chain "[{\"provider\":\"ollama\",\"model\":\"m$long_model\"}]")" \
    "$(chain '[{"provider":"ollama","model":"a\nb"}]')" \
    "$(chain '[{"provider":"ollama","model":"a\u0085b"}]')" \
    "$(chain '[{"provider":"ollama","model":"a"},{"provider":"ollama","model":"a"}]')" \
    "$(chain '[{"provider":"claude-cli","model":"a"},{"provider":"anthropic-api","model":"a"}]')" \
    "$(chain '[{"provider":"claude-cli","model":"a"},{"provider":"ollama","model":"b"},{"provider":"zai-api","model":"c"}]')" \
    "$(chain '[{"provider":"codex-cli","model":"gpt-6-luna"},{"provider":"openai-api","model":"gpt-6-luna"}]')" \
    "$(printf '{"version":1}%65523s' '')"; do
    if valid_routing "$document"; then echo "accepted invalid routing: ${document:0:120}" >&2; exit 1; fi
done

# Both dispatchers route `models route` under the shared policy-directory lock.
grep -Fq 'refresh|refresh-configured|register|enable|disable|set-route|route)' "$repo/deploy/systemd/jarvis-models.sh"
grep -Fq 'register) (($# == 3)) || usage; register_subscription_model "$2" "$3" ;;' "$repo/deploy/systemd/jarvis-models.sh"
grep -Fq 'route) shift; route_command "$@" ;;' "$repo/deploy/systemd/jarvis-models.sh"
echo 'Model routing CLI tests passed'
