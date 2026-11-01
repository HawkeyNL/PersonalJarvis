#!/usr/bin/env bash
# All keys, values and calls are disposable fixtures: never touches host
# credentials, users or services.
set -euo pipefail
trap 'echo "IBKR OAuth credential assertion failed at line $LINENO" >&2' ERR
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
source "$repo/deploy/systemd/jarvis-credentials.sh"
ibkr_oauth_dir=$fixture/ibkr-oauth
ibkr_root_uid=$(id -u)
require_tty() { :; }
chown() { :; }
getent() { [[ -e $fixture/identity ]]; }
useradd() { printf '%s\n' "$*" >> "$fixture/identity"; }
systemctl() { printf '%s\n' "$*" >> "$fixture/service-calls"; }
# Answers the hidden prompts in order from the fixture's answer list.
ibkr_read_hidden() {
    local answer
    answer=$(sed -n '1p' "$fixture/answers")
    sed -i '1d' "$fixture/answers"
    printf '%s' "$answer"
}

readonly fx_consumer_key=FIXTURECONS1
readonly fx_access_token=fixtureAccessToken42
readonly fx_secret=fixture-access-token-secret
fx_secret_hex=$(printf '%s' "$fx_secret" | od -An -v -tx1 | tr -d ' \n')
(
    umask 077
    mkdir "$ibkr_oauth_dir"
    for key in encryption signature; do
        openssl genrsa -out "$ibkr_oauth_dir/private_$key.pem" 2048 2>/dev/null
    done
    openssl genrsa -out "$fixture/wrong.pem" 2048 2>/dev/null
    openssl rsa -in "$ibkr_oauth_dir/private_encryption.pem" -pubout -out "$fixture/public.pem" 2>/dev/null
    printf -- '-----BEGIN DH PARAMETERS-----\nfixture\n-----END DH PARAMETERS-----\n' > "$ibkr_oauth_dir/dhparam.pem"
)
fx_encrypted=$(printf '%s' "$fx_secret" |
    openssl pkeyutl -encrypt -pubin -inkey "$fixture/public.pem" -pkeyopt rsa_padding_mode:pkcs1 | base64 -w0)
answers() { printf '%s\n' "$@" > "$fixture/answers"; }

# Nothing is set yet.
ibkr_oauth_status > "$fixture/status"
grep -Eq '^consumer_key +empty$' "$fixture/status"
grep -Eq '^private_signature.pem +set$' "$fixture/status"

# Malformed input and a ciphertext for another key change nothing.
for case in "lowercase-key $fx_access_token $fx_encrypted" "$fx_consumer_key bad/token $fx_encrypted" \
    "$fx_consumer_key $fx_access_token not*base64"; do
    read -r -a values <<< "$case"
    answers "${values[@]}"
    if set_ibkr_oauth >> "$fixture/output" 2>&1; then
        echo "malformed IBKR input was accepted: ${values[0]}" >&2; exit 1
    fi
done
fx_wrong=$(printf '%s' "$fx_secret" | openssl pkeyutl -encrypt -pubin \
    -inkey <(openssl rsa -in "$fixture/wrong.pem" -pubout 2>/dev/null) -pkeyopt rsa_padding_mode:pkcs1 | base64 -w0)
answers "$fx_consumer_key" "$fx_access_token" "$fx_wrong"
if set_ibkr_oauth >> "$fixture/output" 2>&1; then
    echo 'a secret encrypted for another key was accepted' >&2; exit 1
fi
grep -Fq 'does not decrypt with private_encryption.pem' "$fixture/output"
[[ ! -e $ibkr_oauth_dir/consumer_key && ! -e $ibkr_oauth_dir/access_token_secret ]]
[[ ! -e $fixture/identity && ! -e $fixture/service-calls ]]

# Unsafe directory or key permissions are refused before any prompt.
chmod 0750 "$ibkr_oauth_dir"
answers "$fx_consumer_key" "$fx_access_token" "$fx_encrypted"
if set_ibkr_oauth >> "$fixture/output" 2>&1; then
    echo 'a group-readable credential directory was accepted' >&2; exit 1
fi
chmod 0700 "$ibkr_oauth_dir"
chmod 0640 "$ibkr_oauth_dir/private_encryption.pem"
if set_ibkr_oauth >> "$fixture/output" 2>&1; then
    echo 'a group-readable encryption key was accepted' >&2; exit 1
fi
chmod 0600 "$ibkr_oauth_dir/private_encryption.pem"

# Success: the secret is decrypted once and stored as lowercase hex, every
# value root-only 0400; the identity is created and a running broker reloads.
answers "$fx_consumer_key" "$fx_access_token" "$fx_encrypted"
set_ibkr_oauth >> "$fixture/output" 2>&1
[[ $(<"$ibkr_oauth_dir/consumer_key") == "$fx_consumer_key" ]]
[[ $(<"$ibkr_oauth_dir/access_token") == "$fx_access_token" ]]
[[ $(<"$ibkr_oauth_dir/access_token_secret") == "$fx_secret_hex" ]]
for item in consumer_key access_token access_token_secret; do
    [[ $(command stat -c '%a' "$ibkr_oauth_dir/$item") == 400 ]]
done
[[ -z $(find "$ibkr_oauth_dir" -name '.*' -print -quit) ]]
grep -Fq -- '--system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin jarvis-ibkr' "$fixture/identity"
grep -Fxq 'try-restart jarvis-ibkr-broker.service' "$fixture/service-calls"
ibkr_oauth_status > "$fixture/status"
[[ $(grep -c ' set$' "$fixture/status") == 5 ]]

# Status reports only set/empty; an unsafe file counts as empty.
chmod 0440 "$ibkr_oauth_dir/access_token"
ibkr_oauth_status > "$fixture/status"
grep -Eq '^access_token +empty$' "$fixture/status"
[[ -z $(grep -Ev '^[a-z_.]+ +(set|empty)$' "$fixture/status") ]]

# Links are never followed: a linked item counts as empty, a linked directory
# is refused.
mv "$ibkr_oauth_dir/consumer_key" "$fixture/linked-key"
ln -s "$fixture/linked-key" "$ibkr_oauth_dir/consumer_key"
ibkr_oauth_status > "$fixture/status"
grep -Eq '^consumer_key +empty$' "$fixture/status"
mv "$ibkr_oauth_dir" "$fixture/real-dir"
ln -s "$fixture/real-dir" "$ibkr_oauth_dir"
answers "$fx_consumer_key" "$fx_access_token" "$fx_encrypted"
if set_ibkr_oauth >> "$fixture/output" 2>&1; then
    echo 'a linked credential directory was accepted' >&2; exit 1
fi

# No value, ciphertext or decrypted secret reaches output or service calls.
for leaked in "$fx_consumer_key" "$fx_access_token" "$fx_encrypted" "$fx_secret" "$fx_secret_hex"; do
    if grep -Fq -- "$leaked" "$fixture/output" "$fixture/status" "$fixture/service-calls" "$fixture/identity"; then
        echo 'IBKR credential material leaked into output' >&2; exit 1
    fi
done
echo 'IBKR OAuth credential helper tests passed'
