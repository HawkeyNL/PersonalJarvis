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
# Answers the hidden prompts in order from the fixture's answer list and
# records each prompt label and its keep flag.
market_read_hidden() {
    local answer
    printf '%s|%s\n' "$1" "${2:-}" >> "$fixture/prompts"
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
# With nothing saved, Enter is an empty URL and a plain prompt.
expect_refused 'an empty RPC A with nothing saved' '' "$fx_rpc_b" ''
[[ $(tail -n1 "$fixture/prompts") == 'Solana RPC A URL|' ]]
grep -Fq 'must be on different hosts' "$fixture/output"
[[ ! -e $fixture/curl.argv ]]

# A provider that does not answer changes nothing.
expect_refused 'a failed probe' "$fx_rpc_a" 'https://api.mainnet-beta.solana.com/down' ''
grep -Fxq 'RPC A: ok' "$fixture/output"
grep -Fxq 'RPC B: failed' "$fixture/output"

# With nothing saved, "-" also means no Jupiter key.
: > "$fixture/prompts"
answers "$fx_rpc_a" "$fx_rpc_b" '-'
set_market_data >> "$fixture/output" 2>&1
[[ $(tail -n1 "$fixture/prompts") == 'Jupiter API key (optional, empty or - for none)|' ]]
[[ $(<"$market_data_file") == "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_a
JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_b" ]]

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
market_data_status > "$fixture/status"
[[ $(grep -c ' set$' "$fixture/status") == 3 ]]

# Saved values: every prompt offers to keep its value.
fx_rpc_c='https://api.mainnet-beta.solana.com/'
: > "$fixture/prompts"
: > "$fixture/curl.config"
answers '' "$fx_rpc_c" ''
set_market_data >> "$fixture/output" 2>&1
[[ $(<"$fixture/prompts") == 'Solana RPC A URL|keep
Solana RPC B URL|keep
Jupiter API key (optional, - removes the saved key)|keep' ]]
# Keep A and type B: the kept A is probed again, the key is kept.
[[ $(<"$market_data_file") == "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_a
JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_c
JARVIS_MARKET_JUPITER_API_KEY=$fx_jupiter" ]]
grep -Fq "url = \"$fx_rpc_a\"" "$fixture/curl.config"
[[ $(command stat -c '%a' "$market_data_file") == 640 ]]
# A kept A still needs a B on another host.
cp "$market_data_file" "$fixture/before"
answers '' 'https://mainnet.helius-rpc.com/?api-key=other' ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a kept RPC A on the same host as B was accepted' >&2; exit 1
fi
cmp -s "$market_data_file" "$fixture/before"
# Keep both, and a kept RPC that no longer answers changes nothing.
answers "$fx_rpc_b" '' ''
set_market_data >> "$fixture/output" 2>&1
[[ $(<"$market_data_file") == "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_b
JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_c
JARVIS_MARKET_JUPITER_API_KEY=$fx_jupiter" ]]
cp "$market_data_file" "$fixture/before"
: > "$fixture/curl.config"
answers '' '' ''
set_market_data >> "$fixture/output" 2>&1
cmp -s "$market_data_file" "$fixture/before"
grep -Fq "url = \"$fx_rpc_b\"" "$fixture/curl.config"
grep -Fq "url = \"$fx_rpc_c\"" "$fixture/curl.config"
sed -i "s|^JARVIS_MARKET_SOLANA_RPC_B_URL=.*|JARVIS_MARKET_SOLANA_RPC_B_URL=${fx_rpc_c}down|" "$market_data_file"
cp "$market_data_file" "$fixture/before"
answers '' '' ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a kept RPC that failed its probe was saved' >&2; exit 1
fi
cmp -s "$market_data_file" "$fixture/before"

# The Jupiter key is optional; a new set replaces the whole file, and "-"
# removes a saved key.
answers "$fx_rpc_b" "$fx_rpc_a" '-'
set_market_data >> "$fixture/output" 2>&1
[[ $(wc -l < "$market_data_file") == 2 ]]
: > "$fixture/prompts"
answers '' '' ''
set_market_data >> "$fixture/output" 2>&1
[[ $(wc -l < "$market_data_file") == 2 ]]
[[ $(tail -n1 "$fixture/prompts") == 'Jupiter API key (optional, empty or - for none)|' ]]
market_data_status > "$fixture/status"
grep -Eq '^JARVIS_MARKET_JUPITER_API_KEY +empty$' "$fixture/status"
grep -Eq '^JARVIS_MARKET_SOLANA_RPC_A_URL +set$' "$fixture/status"
[[ -z $(grep -Ev '^JARVIS_[A-Z_]+ +(set|empty)$' "$fixture/status") ]]
[[ -z $(find "$fixture/secrets" -name '.*' -print -quit) ]]
if compgen -G "$fixture/credential-*" >/dev/null; then
    echo 'a probe left a temporary file behind' >&2; exit 1
fi

# Hand-edited file: the last duplicate wins, a missing final newline still
# reads, and malformed, empty or leading-space lines are ignored.
printf '%s\n' "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_c" "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_a" \
    "JARVIS_MARKET_SOLANA_RPC_B_URL=" " JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_c" 'not a line' '#x' > "$market_data_file"
printf '%s' "JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_b" >> "$market_data_file"
chmod 0640 "$market_data_file"
saved_a='' saved_b='' saved_jupiter=''
market_data_load_saved
[[ $saved_a == "$fx_rpc_a" && $saved_b == "$fx_rpc_b" && -z $saved_jupiter ]]
# A CR line keeps a value that fails its check, so set fails closed.
printf '%s\r\n%s\n' "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_a" "JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_b" > "$market_data_file"
cp "$market_data_file" "$fixture/before"
answers '' '' ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a kept value with a CR was accepted' >&2; exit 1
fi
cmp -s "$market_data_file" "$fixture/before"
# A wrong owner or group counts as nothing saved.
for market_data_owner in "wrong-user:$(id -gn)" "$(id -un):wrong-group"; do
    saved_a='' saved_b='' saved_jupiter=''
    market_data_load_saved
    [[ -z $saved_a$saved_b$saved_jupiter ]]
done
market_data_owner="$(id -un):$(id -gn)"
unset saved_a saved_b saved_jupiter
printf '%s\n' "JARVIS_MARKET_SOLANA_RPC_A_URL=$fx_rpc_b" "JARVIS_MARKET_SOLANA_RPC_B_URL=$fx_rpc_a" > "$market_data_file"
# The open file is checked itself, not only the path: with the path check
# stubbed out, a 0644 file or a link still loads nothing.
(
    market_data_safe() { return 0; }
    market_data_load_saved
    [[ $saved_a == "$fx_rpc_b" ]]
    unset saved_a saved_b saved_jupiter
    chmod 0644 "$market_data_file"
    market_data_load_saved
    [[ -z ${saved_a:-}${saved_b:-}${saved_jupiter:-} ]]
    chmod 0640 "$market_data_file"
    mv "$market_data_file" "$fixture/linked.env"
    ln -s "$fixture/linked.env" "$market_data_file"
    market_data_load_saved
    [[ -z ${saved_a:-}${saved_b:-}${saved_jupiter:-} ]]
    mv -fT -- "$fixture/linked.env" "$market_data_file"
)
[[ -f $market_data_file && ! -L $market_data_file ]]

# An unsafe or linked file counts as empty, nothing in it is kept, and a
# link is never replaced.
chmod 0644 "$market_data_file"
market_data_status > "$fixture/status"
[[ $(grep -c ' empty$' "$fixture/status") == 3 ]]
cp "$market_data_file" "$fixture/before"
: > "$fixture/prompts"
answers '' '' ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a value from an unsafe market data file was kept' >&2; exit 1
fi
[[ $(<"$fixture/prompts") == 'Solana RPC A URL|' ]]
cmp -s "$market_data_file" "$fixture/before"
chmod 0640 "$market_data_file"
mv "$market_data_file" "$fixture/real.env"
ln -s "$fixture/real.env" "$market_data_file"
market_data_status > "$fixture/status"
[[ $(grep -c ' empty$' "$fixture/status") == 3 ]]
: > "$fixture/prompts"
answers '' '' ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a value from a linked market data file was kept' >&2; exit 1
fi
[[ $(<"$fixture/prompts") == 'Solana RPC A URL|' ]]
answers "$fx_rpc_a" "$fx_rpc_b" ''
if set_market_data >> "$fixture/output" 2>&1; then
    echo 'a linked market data file was replaced' >&2; exit 1
fi
[[ -L $market_data_file ]]

# No URL or key, typed or kept, reaches curl's argv, the output or the
# status.
if grep -Fq -e "$fx_rpc_a" -e "$fx_rpc_b" -e "$fx_rpc_c" -e "$fx_jupiter" "$fixture/curl.argv"; then
    echo 'market data leaked into curl argv' >&2; exit 1
fi
if grep -Fq -e "$fx_rpc_a" -e "$fx_rpc_b" -e "$fx_jupiter" -e fixture-helius-key -e fixture-alchemy-key \
    "$fixture/output" "$fixture/status"; then
    echo 'market data leaked into output' >&2; exit 1
fi
echo 'market data credential helper tests passed'
