#!/usr/bin/env bash
# Static security regression coverage for PR #26.  No real provider or secret
# is required in CI.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
credentials="$repo_dir/deploy/systemd/jarvis-credentials.sh"
models="$repo_dir/deploy/systemd/jarvis-models.sh"
unit="$repo_dir/deploy/systemd/jarvis-core.service"
prepare="$repo_dir/deploy/systemd/prepare-home-node.sh"

for file in "$credentials" "$models" "$unit" "$prepare"; do
    [[ -f $file ]] || { echo "missing model security asset: $file" >&2; exit 1; }
done
bash -n "$credentials" "$models" "$prepare"

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

# Provider access policy is exact, starts remote models disabled, and uses an
# atomic replacement.  Local Ollama is distinguished from ollama-cloud.
grep -Fq 'new remote models remain disabled' "$models"
grep -Fq 'atomic_write' "$models"
grep -Fq 'ollama-cloud' "$models"
grep -Fq "provider == \$item[0] and .model == \$item[1]" "$models"
grep -Fq '/etc/jarvis/secrets' "$prepare"

for provider in anthropic openai deepseek xai zai ollama-cloud; do
    grep -Fq "EnvironmentFile=-/etc/jarvis/secrets/$provider.env" "$unit"
done

echo "Model/credential boundary checks passed"
