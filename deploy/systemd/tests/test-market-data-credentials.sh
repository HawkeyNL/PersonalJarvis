#!/usr/bin/env bash
# All URLs, keys and calls are disposable fixtures: never touches host
# credentials, services or the network.
set -euo pipefail
trap 'echo "market data credential assertion failed at line $LINENO" >&2' ERR
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
# shellcheck source=../jarvis-credentials.sh
source "$repo/deploy/systemd/jarvis-credentials.sh"
market_data_file=$fixture/secrets/market-data.env
market_data_owner="$(id -un):$(id -gn)"
require_tty() { :; }
install() { command mkdir -p -- "${@: -1}"; }
mktemp() {
    case ${1:-} in
        /run/jarvis-credential-test.*) command mktemp "$fixture/credential-test.XXXXXX" ;;
        /run/jarvis-credential-response.*) command mktemp "$fixture/credential-response.XXXXXX" ;;
        *) command mktemp "$@" ;;
    esac
}
# Answers the hidden prompts in order from the fixture's answer list.
market_read_hidden() {
    local answer
    answer=$(sed -n '1p' "$fixture/answers")
    sed -i '1d' "$fixture/answers"
    printf '%s' "$answer"
}
answers() { printf '%s\n' "$@" > "$fixture/answers"; }
# A provider answers getSlot unless its URL contains "down".
curl() {
    local config='' output=/dev/null
    printf '%s\n' "$@" >> "$fixture/curl.argv"
    while (($#)); do
        case $1 in
            --config) config=$2; shift 2 ;;
            --output) output=$2; shift 2 ;;
            *) shift ;;
        esac
    done
    cat -- "$config" >> "$fixture/curl.config"
    if grep -Fq down "$config"; then
        printf '%s' '{"jsonrpc":"2.0","error":{"code":-32005},"id":1}' > "$output"
    else
        printf '%s' '{"jsonrpc":"2.0","result":372001234,"id":1}' > "$output"
    fi
    printf '200'
}

readonly fx_rpc_a='https://mainnet.helius-rpc.com/?api-key=fixture-helius-key-1'
readonly fx_rpc_b='https://solana-mainnet.g.alchemy.com/v2/fixture-alchemy-key-2'
readonly fx_jupiter=fixture-jupiter-key-3
expect_refused() {
    local reason=$1
    shift
    answers "$@"
    if set_market_data >> "$fixture/output" 2>&1; then
        echo "accepted: $reason" >&2; exit 1
    fi
    [[ ! -e $market_data_file ]]
}

# Nothing is set yet.
market_data_status > "$fixture/status"
[[ $(grep -c ' empty$' "$fixture/status") == 3 ]]

# Only https URLs on the allowlisted RPC hosts, without user, port or
# fragment, and two different hosts; nothing is probed before that holds.
expect_refused 'another host' 'https://rpc.example.com/' "$fx_rpc_b" ''
expect_refused 'plain http' 'http://api.mainnet-beta.solana.com' "$fx_rpc_b" ''
expect_refused 'a port' 'https://api.mainnet-beta.solana.com:8899' "$fx_rpc_b" ''
expect_refused 'user info' 'https://u:p@mainnet.helius-rpc.com/' "$fx_rpc_b" ''
expect_refused 'a fragment' 'https://mainnet.helius-rpc.com/#x' "$fx_rpc_b" ''
expect_refused 'a quote' 'https://mainnet.helius-rpc.com/?k="x' "$fx_rpc_b" ''
expect_refused 'a lookalike host' 'https://mainnet.helius-rpc.com.evil.example/' "$fx_rpc_b" ''
expect_refused 'one host twice' "$fx_rpc_a" 'https://mainnet.helius-rpc.com/?api-key=other' ''
expect_refused 'a malformed Jupiter key' "$fx_rpc_a" "$fx_rpc_b" 'bad key'
grep -Fq 'must be on different hosts' "$fixture/output"
[[ ! -e $fixture/curl.argv ]]

# A provider that does not answer changes nothing.
expect_refused 'a failed probe' "$fx_rpc_a" 'https://api.mainnet-beta.solana.com/down' ''
grep -Fxq 'RPC A: ok' "$fixture/output"
grep -Fxq 'RPC B: failed' "$fixture/output"

# Success: both probed with a read-only getSlot, the file staged and moved
# into place with mode 0640, and the values never on curl's argv.
answers "$fx_rpc_a" "$fx_rpc_b" "$fx_jupiter"
set_market_data > "$fixture/saved" 2>&1
grep -Fxq 'RPC A: ok' "$fixture/saved"
grep -Fxq 'RPC B: ok' "$fixture/saved"
cat "$fixture/saved" >> "$fixture/output"
[[ $(<"$market_data_file") == "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_a
JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_b
JARVIS_MARKET_JUPITER_API_KEY=$fx_jupiter" ]]
[[ $(command stat -c '%a' "$market_data_file") == 640 ]]
grep -Fq "url = \"$fx_rpc_a\"" "$fixture/curl.config"
grep -Fq '"method\":\"getSlot\"' "$fixture/curl.config"
if grep -Fq -e "$fx_rpc_a" -e "$fx_rpc_b" -e "$fx_jupiter" "$fixture/curl.argv"; then
    echo 'market data leaked into curl argv' >&2; exit 1
fi
market_data_status > "$fixture/status"
[[ $(grep -c ' set$' "$fixture/status") == 3 ]]

# The Jupiter key is optional; a new set replaces the whole file.
answers "$fx_rpc_b" "$fx_rpc_a" ''
set_market_data >> "$fixture/output" 2>&1
[[ $(wc -l < "$market_data_file") == 2 ]]
market_data_status > "$fixture/status"
grep -Eq '^JARVIS_MARKET_JUPITER_API_KEY +empty$' "$fixture/status"
grep -Eq '^JARVIS_MARKET_SOLANA_RPC_A_URL +set$' "$fixture/status"
[[ -z $(grep -Ev '^JARVIS_[A-Z_]+ +(set|empty)$' "$fixture/status") ]]
[[ -z $(find "$fixture/secrets" -name '.*' -print -quit) ]]
if compgen -G "$fixture/credential-*" >/dev/null; then
    echo 'a probe left a temporary file behind' >&2; exit 1
fi

# An unsafe or linked file counts as empty, and a link is never replaced.
chmod 0644 "$market_data_file"
market_data_status > "$fixture/status"
[[ $(grep -c ' empty$' "$fixture/status") == 3 ]]
chmod 0640 "$market_data_file"
mv "$market_data_file" "$fixture/real.env"
ln -s "$fixture/real.env" "$market_data_file"
market_data_status > "$fixture/status"
[[ $(grep -c ' empty$' "$fixture/status") == 3 ]]
answers "$fx_rpc_a" "$fx_rpc_b" ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a linked market data file was replaced' >&2; exit 1
fi
[[ -L $market_data_file ]]

# No URL or key reaches the output or the status.
if grep -Fq -e "$fx_rpc_a" -e "$fx_rpc_b" -e "$fx_jupiter" -e fixture-helius-key -e fixture-alchemy-key \
    "$fixture/output" "$fixture/status"; then
    echo 'market data leaked into output' >&2; exit 1
fi
echo 'market data credential helper tests passed'
