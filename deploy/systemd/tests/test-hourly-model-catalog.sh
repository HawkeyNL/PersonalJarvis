#!/usr/bin/env bash
# Rootless behavioral orchestration tests: no secrets, provider calls or services.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
source "$repo/deploy/systemd/jarvis-models.sh"

credential_configured() { [[ $1 == openai-api || $1 == huggingface ]]; }
refresh() {
    printf '%s\n' "$1" >> "$fixture/calls"
    [[ $1 != openai-api ]] # Failure must not prevent HF refresh.
}
if refresh_configured > "$fixture/stdout" 2> "$fixture/stderr"; then
    echo "partial failure must fail the service visibly" >&2; exit 1
fi
[[ $(wc -l < "$fixture/calls") == 2 ]]
grep -Fxq openai-api "$fixture/calls"
grep -Fxq huggingface "$fixture/calls"
grep -Fq 'prior model choices retained' "$fixture/stderr"
credential_configured() { return 1; }
refresh_configured > "$fixture/empty"
[[ $(wc -l < "$fixture/calls") == 2 ]]
grep -Fq 'checked 0 configured providers' "$fixture/empty"

rows=$(parse_remote_model_response anthropic-api '{"data":[{"id":"claude-fixture"}],"has_more":false}' | aggregate_discovered_models)
old='{"version":1,"models":[{"provider":"anthropic-api","model":"owner-enabled","enabled":true,"source":"owner"},{"provider":"huggingface","model":"org/model","enabled":false,"route":"groq","source":"owner"}]}'
next=$(merge_model_policy "$old" "$rows")
jq -e 'any(.models[]; .model == "owner-enabled" and .enabled == true)
    and any(.models[]; .model == "claude-fixture" and .enabled == false)
    and any(.models[]; .model == "org/model" and .route == "groq" and .enabled == false)' <<<"$next" >/dev/null
if parse_remote_model_response openai-api '{"data":[{"id":"partial"}]} garbage' > "$fixture/malformed" 2>/dev/null; then
    echo "malformed catalog accepted" >&2; exit 1
fi
[[ ! -s $fixture/malformed ]]
for response in '{"data":[]} {"data":[{"id":"second-document"}]}' \
    "$(jq -cn '{data:[range(2001) | {id:("model-" + tostring)}]}')"; do
    if parse_remote_model_response openai-api "$response" > "$fixture/malformed" 2>/dev/null; then
        echo 'multiple-document or oversized catalog accepted' >&2; exit 1
    fi
    [[ ! -s $fixture/malformed ]]
done
grep -Fxq 'OnUnitInactiveSec=1h' "$repo/deploy/systemd/jarvis-model-catalog.timer"
grep -Fxq 'PartOf=jarvis-core.service' "$repo/deploy/systemd/jarvis-model-catalog.timer"
grep -Fxq 'Wants=jarvis-model-catalog.timer' "$repo/deploy/systemd/jarvis-core.service"
echo 'Hourly model catalog tests passed'
