#!/usr/bin/env bash
# Behavioral coverage for the provider response parser and policy merge. This
# uses fixture metadata only: no live provider, credential or protected path.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
# shellcheck source=../jarvis-models.sh
source "$repo_dir/deploy/systemd/jarvis-models.sh"

ollama_response=$(jq -n --arg too_long "$(printf 'x%.0s' {1..257})" '{
  models: [
    {name: "glm-5.2", size: 123},
    {name: "gpt-oss:20b"},
    {name: ""},
    {name: null},
    {name: 42},
    {name: $too_long}
  ],
  api_key: "fixture-secret-must-not-escape",
  private_detail: {token: "also-secret"}
}')

ollama_discovered=$(
    parse_remote_model_response ollama-cloud "$ollama_response" |
        aggregate_discovered_models
)
jq -e '
  length == 2
  and all(.[];
    length == 3
    and .[0] == "ollama-cloud"
    and (.[1] == "glm-5.2" or .[1] == "gpt-oss:20b")
    and .[2] == "provider_api"
  )
' <<<"$ollama_discovered" >/dev/null
if grep -Fq 'fixture-secret' <<<"$ollama_discovered"; then
    echo "Ollama discovery retained a secret-bearing structured field" >&2
    exit 1
fi

openai_discovered=$(
    parse_remote_model_response openai-api \
        '{"data":[{"id":"gpt-fixture"},{"id":""},{"id":false}]}' |
        aggregate_discovered_models
)
jq -e '
  . == [["openai-api", "gpt-fixture", "provider_api"]]
' <<<"$openai_discovered" >/dev/null

empty_discovered=$(
    parse_remote_model_response ollama-cloud '{"models":[]}' |
        aggregate_discovered_models
)
jq -e '. == []' <<<"$empty_discovered" >/dev/null

merged=$(merge_model_policy '{"version":1,"models":[]}' "$ollama_discovered")
jq -e '
  .version == 1
  and (.models | length == 2)
  and all(.models[];
    .provider == "ollama-cloud"
    and .enabled == false
    and .source == "provider_api"
  )
' <<<"$merged" >/dev/null

echo "Model discovery pipeline tests passed"
