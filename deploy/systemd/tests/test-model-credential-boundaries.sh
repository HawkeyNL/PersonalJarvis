#!/usr/bin/env bash
# Static security regression coverage for PR #26.  No real provider or secret
# is required in CI.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
credentials="$repo_dir/deploy/systemd/jarvis-credentials.sh"
models="$repo_dir/deploy/systemd/jarvis-models.sh"
unit="$repo_dir/deploy/systemd/jarvis-core.service"
prepare="$repo_dir/deploy/systemd/prepare-home-node.sh"
generator="$repo_dir/deploy/systemd/generate-core-env.sh"
pricing="$repo_dir/deploy/systemd/pricing-registry.json"

for file in "$credentials" "$models" "$unit" "$prepare" "$generator"; do
    [[ -f $file ]] || { echo "missing model security asset: $file" >&2; exit 1; }
done
broker="$repo_dir/deploy/systemd/jarvis-config-broker.service"
[[ -f $broker ]] || { echo "missing privileged config broker unit" >&2; exit 1; }
grep -Fq 'User=root' "$broker"
grep -Fq 'EnvironmentFile=/etc/jarvis/core.env' "$broker"
grep -Fq 'ReadWritePaths=/etc/jarvis/model-policy.json' "$broker"
if grep -Eq 'ExecStart=.*(sh|bash)|/bin/(sh|bash)' "$broker"; then
    echo "privileged broker must not expose a shell" >&2
    exit 1
fi
[[ -f $pricing ]] || { echo "missing pricing registry fixture" >&2; exit 1; }
bash -n "$credentials" "$models" "$prepare" "$generator"
jq -e '
  .version == 1
  and (.source | type == "string" and length > 0)
  and (.updated_at | type == "string" and length > 0)
  and (.models | type == "array" and length > 0)
  and all(.models[]; (.provider | type == "string" and length > 0) and (.model | type == "string" and length > 0) and (.input_per_million_usd >= 0) and (.output_per_million_usd >= 0))
' "$pricing" >/dev/null
jq -e '
  ([.models[] | select(.provider == "ollama-cloud")] | length >= 15)
  and any(.models[]; .provider == "ollama-cloud" and .model == "gpt-oss:20b"
      and .input_per_million_usd == 0.07 and .output_per_million_usd == 0.3)
' "$pricing" >/dev/null

# Credentials require a TTY, use hidden input, and are never accepted through
# argv/stdin.  The secret variable must not be printed or passed to curl.
grep -Fq "requires a controlling TTY" "$credentials"
grep -Fq "read -r -s secret </dev/tty" "$credentials"
if grep -Eq "echo.*\\\$secret" "$credentials"; then
    echo "credential manager prints a secret variable" >&2
    exit 1
fi
if grep -Eq 'curl[[:space:]].*-H' "$credentials"; then
    echo "credential manager sends credentials to curl" >&2
    exit 1
fi
grep -Fq "mktemp \"\$secret_dir/." "$credentials"
grep -Fq 'chown root:jarvis' "$credentials"
grep -Fq 'chmod 0640' "$credentials"
grep -Fq 'restoring prior credential state' "$credentials"
grep -Fq 'probe_provider' "$credentials"
grep -Fq 'mktemp /run/jarvis-credential-test' "$credentials"
grep -Fq 'curl --config "$config"' "$credentials"
grep -Fq 'no generation request was made' "$credentials"

# Ollama Cloud is a first-class remote provider. A fresh Home Node and an
# existing one configured through `credentials set/test ollama-cloud` both get
# the canonical OpenAI-compatible chat endpoint without exposing the credential.
grep -Fq 'ollama_cloud_default_base_url=https://ollama.com/v1' "$credentials"
grep -Fq 'ensure_provider_defaults "$provider"' "$credentials"
grep -Fq 'JARVIS_LLM_OLLAMA_CLOUD_BASE_URL "$ollama_cloud_default_base_url"' "$credentials"
grep -Fq 'JARVIS_LLM_OLLAMA_CLOUD_BASE_URL=https://ollama.com/v1' "$generator"
# Ollama Cloud account/model metadata is native `/api/tags`, not `/v1/models`.
grep -Fq 'ollama_cloud_tags_url=https://ollama.com/api/tags' "$credentials"
grep -Fq 'url=$ollama_cloud_tags_url' "$credentials"
# Normal restart races must stay quiet instead of printing transient connection
# refused diagnostics while systemd is still bringing Core back up.
grep -Fq 'systemctl is-active --quiet jarvis-core.service' "$credentials"
grep -Fq 'curl --fail --silent --output /dev/null' "$credentials"

# Provider access policy is exact, starts remote models disabled, and uses an
# atomic replacement. Local Ollama is distinguished from ollama-cloud.
grep -Fq 'new remote models remain disabled' "$models"
grep -Fq 'atomic_write' "$models"
grep -Fq 'ollama-cloud' "$models"
grep -Fq "provider == \$item[0] and .model == \$item[1]" "$models"
grep -Fq "curl --config \"\$config\"" "$models"
grep -Fq 'mktemp /run/jarvis-model-discovery' "$models"
grep -Fq 'provider_api' "$models"
grep -Fq 'ollama_cloud_default_base_url=https://ollama.com/v1' "$models"
grep -Fq 'ollama_cloud_tags_url=https://ollama.com/api/tags' "$models"
grep -Fq "ollama-cloud) jq_filter='.models[]?.name?'" "$models"
grep -Fq 'aggregate_discovered_models' "$models"
if grep -Fq 'map(fromjson?)' "$models"; then
    echo "model discovery reparses JSON values as strings" >&2
    exit 1
fi
grep -Fq 'model discovery returned no models' "$models"
# Only the known restrictive legacy ownership/mode may be normalized; unsafe
# writable/symlink/non-root states still fail closed.
grep -Fq 'normalize_model_policy_boundary' "$models"
grep -Fq 'root:root:600|root:root:640|root:jarvis:600' "$models"
grep -Fq 'policy directory permissions are unsafe' "$models"
grep -Fq 'policy permissions are unsafe' "$models"
if grep -Eq 'curl[[:space:]].*-H[[:space:]]+.*Authorization' "$models"; then
    echo "model discovery exposes an authorization header in argv" >&2
    exit 1
fi
grep -Fq '/etc/jarvis/secrets' "$prepare"
grep -Fq '/etc/jarvis/pricing-registry.json' "$prepare"
grep -Fq '[[ ! -e /etc/jarvis/pricing-registry.json ]]' "$prepare"

for provider in anthropic openai deepseek xai zai ollama-cloud; do
    grep -Fq "EnvironmentFile=-/etc/jarvis/secrets/$provider.env" "$unit"
done

echo "Model/credential boundary checks passed"
