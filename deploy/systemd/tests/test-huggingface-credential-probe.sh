#!/usr/bin/env bash
# Metadata-only credential probe coverage. A shell-local curl mock is used; no
# token leaves the process and no network request is made.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
# shellcheck source=../jarvis-credentials.sh
source "$repo_dir/deploy/systemd/jarvis-credentials.sh"

fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
cp "$repo_dir/deploy/systemd/tests/fixtures/huggingface-models.json" "$fixture/valid.json"
printf '%s\n' '{"data":[]}' > "$fixture/empty.json"
printf '%s\n' '{"data":[{"id":"bad\nmodel"}]}' > "$fixture/control.json"
printf '%s\n' '{"models":[{"id":"wrong-shape"}]}' > "$fixture/wrong.json"

valid_huggingface_model_response "$fixture/valid.json"
printf '%s\n' 'JARVIS_LLM_HUGGINGFACE_API_KEY=opaque=value"with\slashes' > "$fixture/credential.env"
credential=$(read_credential_value "$fixture/credential.env" JARVIS_LLM_HUGGINGFACE_API_KEY)
[[ $credential == 'opaque=value"with\slashes' ]]
escaped=$(curl_config_escape "$credential")
[[ $escaped == 'opaque=value\"with\\slashes' ]]
unset credential escaped
for invalid in empty control wrong; do
    if valid_huggingface_model_response "$fixture/$invalid.json"; then
        echo "invalid Hugging Face credential-probe response was accepted: $invalid" >&2
        exit 1
    fi
done

# Exercise the same probe function used in production. Redirect its /run
# temporaries into the fixture and capture curl's argv/config separately so the
# test proves that the credential is confined to the mode-0600 curl config.
readonly probe_secret='opaque-hf-test-secret=value'
printf 'JARVIS_LLM_HUGGINGFACE_API_KEY=%s\n' "$probe_secret" > "$fixture/probe.env"
captured_argv="$fixture/curl.argv"
captured_config="$fixture/curl.config"
mock_response="$fixture/valid.json"

openai_compatible_base_url() {
    [[ $1 == huggingface ]]
    printf '%s\n' "$huggingface_default_base_url"
}

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
    cp -- "$mock_response" "$output"
    printf '200'
}

probe_output=$(probe_provider huggingface "$fixture/probe.env")
[[ -z $probe_output ]]
grep -Fq 'url = "https://router.huggingface.co/v1/models"' "$captured_config"
grep -Fq "Authorization: Bearer $probe_secret" "$captured_config"
if grep -Fq "$probe_secret" "$captured_argv"; then
    echo "Hugging Face credential leaked into curl argv" >&2
    exit 1
fi
if compgen -G "$fixture/credential-test.*" >/dev/null \
    || compgen -G "$fixture/credential-response.*" >/dev/null; then
    echo "Hugging Face credential probe left a temporary file behind" >&2
    exit 1
fi

echo "Hugging Face credential probe tests passed"
