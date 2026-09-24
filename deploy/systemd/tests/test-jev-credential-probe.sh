#!/usr/bin/env bash
# Jev credential probe is metadata-only and keeps the opaque secret out of argv.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
# shellcheck source=../jarvis-credentials.sh
source "$repo_dir/deploy/systemd/jarvis-credentials.sh"

fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
printf '%s\n' '{"models":[{"name":"jev-latest","description":"fixture","release_date":"2026-09-15"}]}' > "$fixture/valid.json"
printf '%s\n' '{"models":[]}' > "$fixture/empty.json"
printf '%s\n' '{"models":[{"name":"bad\nname"}]}' > "$fixture/bad.json"
valid_jev_model_response "$fixture/valid.json"
for invalid in empty bad; do
    if valid_jev_model_response "$fixture/$invalid.json"; then
        echo "invalid Jev catalog accepted: $invalid" >&2
        exit 1
    fi
done

readonly probe_secret='opaque-jev-test-secret=value'
printf 'JARVIS_LLM_JEV_API_KEY=%s\n' "$probe_secret" > "$fixture/probe.env"
captured_argv="$fixture/curl.argv"
captured_config="$fixture/curl.config"

mktemp() {
    case ${1:-} in
        /run/jarvis-credential-test.*) command mktemp "$fixture/credential-test.XXXXXX" ;;
        /run/jarvis-credential-response.*) command mktemp "$fixture/credential-response.XXXXXX" ;;
        *) command mktemp "$@" ;;
    esac
}

curl() {
    local config='' output=/dev/null
    printf '%s\n' "$@" > "$captured_argv"
    while (($#)); do
        case $1 in
            --config) config=$2; shift 2 ;;
            --output) output=$2; shift 2 ;;
            *) shift ;;
        esac
    done
    cp -- "$config" "$captured_config"
    cp -- "$fixture/valid.json" "$output"
    printf '200'
}

[[ -z $(probe_provider jev "$fixture/probe.env") ]]
grep -Fq 'url = "https://api.typesafe.ai/v1/models"' "$captured_config"
grep -Fq "Authorization: Bearer $probe_secret" "$captured_config"
if grep -Fq "$probe_secret" "$captured_argv"; then
    echo "Jev credential leaked into curl argv" >&2
    exit 1
fi
if compgen -G "$fixture/credential-test.*" >/dev/null \
    || compgen -G "$fixture/credential-response.*" >/dev/null; then
    echo "Jev credential probe left a temporary file behind" >&2
    exit 1
fi
echo "Jev credential probe tests passed"
