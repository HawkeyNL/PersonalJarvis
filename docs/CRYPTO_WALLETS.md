# Read-only crypto wallets

Core can show what the owner's self-custody crypto wallets hold, in EUR. It
is **read-only**: the owner adds **public addresses** only, and Core reads
public balances and prices. There is no key, seed, signing or transaction
code, and nothing here touches the on-chain trading signer or its mandate.
Anything that looks like key material (a recovery phrase, a private key, an
extended private key, a Solana keypair) is refused before any other check,
with one fixed message; it is never stored, logged or echoed.

It is **off by default** (`JARVIS_CRYPTO_WALLETS_ENABLED=false`). While it is
off, every crypto route answers `503` ("crypto wallets are not enabled") and
Core makes no balance or price request. Wallets already stored stay in the
database but are not served.

Supported: Bitcoin (mainnet, single addresses, no xpub); one EVM address
covering Ethereum, Base, Arbitrum One, OP Mainnet and Polygon PoS (the owner
picks which); Solana. At most 20 wallets.

## Enabling it (owner)

1. Allow the provider hosts in the Home Node's egress rules (see
   [Egress hosts](#egress-hosts)). Core already refuses every other host.
2. Set `JARVIS_CRYPTO_WALLETS_ENABLED=true` in Core's environment and restart
   Core. The EVM RPC URLs default to PublicNode; Solana uses the primary
   market RPC (`JARVIS_MARKET_SOLANA_RPC_A_URL`).
3. Add wallets in the app's Trading > Crypto page. The first refresh runs at
   start and right after a wallet is added (or when the one-minute refresh
   cooldown ends).

To turn it off, set the flag back to `false` and restart Core.

## Configuration (names only)

| Key | Default | Meaning |
| --- | --- | --- |
| `JARVIS_CRYPTO_WALLETS_ENABLED` | `false` | Turns the feature and its refresh task on. |
| `JARVIS_CRYPTO_EVM_RPC_ETHEREUM_URL` | `https://ethereum-rpc.publicnode.com` | Ethereum JSON-RPC. |
| `JARVIS_CRYPTO_EVM_RPC_BASE_URL` | `https://base-rpc.publicnode.com` | Base JSON-RPC. |
| `JARVIS_CRYPTO_EVM_RPC_ARBITRUM_URL` | `https://arbitrum-one-rpc.publicnode.com` | Arbitrum One JSON-RPC. |
| `JARVIS_CRYPTO_EVM_RPC_OPTIMISM_URL` | `https://optimism-rpc.publicnode.com` | OP Mainnet JSON-RPC. |
| `JARVIS_CRYPTO_EVM_RPC_POLYGON_URL` | `https://polygon-bor-rpc.publicnode.com` | Polygon PoS JSON-RPC. |
| `JARVIS_MARKET_SOLANA_RPC_A_URL` | empty | The primary Solana RPC, shared with the memecoin paper trader. |

- An empty EVM URL turns that chain off; an empty Solana URL turns Solana
  off. A chain that is off is left out of `chains_available`, the app does
  not offer it and Core refuses a new wallet on it (`400`).
- Every RPC URL must be `https` on a host in Core's allowlist
  (`crates/market-data/src/lib.rs`, `ALLOWED_HOSTS`); a refused URL turns
  only that chain off and the log names the chain, never the URL.
- RPC URLs are treated as secrets: they may embed a provider key, are
  redacted in Core's config dump and never appear in logs or errors. A keyed
  URL belongs in the market-data secrets file, not in the plain Core
  environment. Switching to Alchemy or Infura also needs their host added to
  `ALLOWED_HOSTS` (a code change) and to the egress rules.
- mempool.space and CoinGecko are fixed; they have no setting.

## Providers and what each learns

Every request carries only a validated public address (or nothing, for
prices) and comes from the Home Node's IP address.

| Provider | Used for | What it learns |
| --- | --- | --- |
| mempool.space | Bitcoin confirmed balance (`GET /api/address/{address}`) | Each Bitcoin address and the IP. It can link the addresses to each other by IP and timing. No account. |
| PublicNode | EVM balances: `eth_getBalance` and ERC-20 `balanceOf` for the fixed allowlisted contracts, one batch per chain | The EVM address, the chains asked, the (fixed) token contracts and the IP; it can link addresses by IP. No account. |
| Alchemy / Infura (only if configured) | Same as PublicNode | The same, plus a link to the owner's provider account through the key in the URL. |
| Solana primary RPC (`JARVIS_MARKET_SOLANA_RPC_A_URL`) | `getBalance`, `getTokenAccountsByOwner` (Token and Token-2022 programs, `jsonParsed`) | The Solana address and the IP; a keyed URL ties it to the owner's provider account, the same account the memecoin paper trader uses (each has its own pace, so together they can send up to twice the rate). No cross-check with the second RPC and no failover. |
| CoinGecko (keyless) | EUR prices: one `GET /api/v3/simple/price` for the whole allowlist | Only the IP and the fixed list of coins. The request is the same whatever is held, so it reveals no holdings. |

CoinGecko's keyless API is shared per IP (about 5 calls a minute) and asks
for attribution where its data is shown, so the apps' crypto page must say
"Prices by CoinGecko". Core asks once per pass.

## What Core does

- **Refresh pass**: right after start, then every 15 minutes, or when the
  owner asks (`POST /v1/trading/crypto/refresh`, at most once a minute,
  else `429` with `retry_after_secs`; adding a wallet counts as asking and,
  within the cooldown, schedules one pass for when it ends).
  A pass reads prices, then every wallet; the chains of an EVM wallet are read
  in parallel. A pass has 60 s; what it does not reach keeps its last state.
  A pass with a failure backs off: 30 minutes, then 1 hour, until a good
  pass. Every pass ends with the realtime hint `trading.crypto.updated`
  (also after a wallet is added or removed).
- **Bitcoin**: confirmed balance = funded − spent outputs of confirmed
  transactions. Unconfirmed (mempool) amounts are ignored.
- **EVM**: native coin plus every allowlisted token of the chain, in one
  JSON-RPC batch. Each reply is matched by id; a missing, duplicate or
  unknown id, an error reply or a value of 2^96 or more fails that chain.
- **Solana**: SOL plus the allowlisted SPL tokens. An allowlisted mint
  whose decimals differ from the allowlist fails the read. Other mints with a
  non-zero balance, and token accounts the provider cannot parse, are only
  counted (`other_tokens`); their names, symbols and amounts are never read
  or stored.
- **Prices**: a price that is missing, not above zero, unparseable, or older
  than 1 hour by CoinGecko's own `last_updated_at` is not used; the asset is
  shown without a price and not counted.
- Only allowlisted assets are counted, under Core's own key, symbol and
  decimals (`crates/market-data/src/wallet.rs`). A token name or symbol from
  a chain is never read, stored or shown. Amounts and values have at most 20
  integer digits; anything larger fails the read (or, for a total, the
  request with `500`).
- **Logs** name a wallet by id and kind and a failure by its code, never an
  address, label, URL or provider text.

## Failure states

Each wallet shows one status:

| Status | Meaning |
| --- | --- |
| `pending` | Added, not read yet. Counts for nothing. |
| `ok` | The last refresh read every chain. |
| `stale` | The last full refresh is more than 1 hour old. Its balances still count, with their time. |
| `error` | The last refresh failed on at least one chain. The last good balances are kept and still count; chains that were read are updated. |

Error codes (`error`): `unavailable` (provider down, HTTP or JSON-RPC error,
or the chain is not configured), `rate_limited` (HTTP 429 or a JSON-RPC rate
limit; Core backs off that provider), `invalid_response` (a reply Core does
not accept: wrong shape, out-of-range value, wrong decimals, foreign data),
`too_many_tokens` (the Solana token-account list is over the 2 MiB response
cap).

Every request is https only with verified TLS, no redirects and no proxy
from the environment, 5 s to connect and 15 s per request, a 2 MiB response
cap and a per-provider pace.

## Egress hosts

Core talks only to these hosts for this feature; the owner must allow them
in the Home Node's egress rules:

- `mempool.space`
- `api.coingecko.com`
- `ethereum-rpc.publicnode.com`
- `base-rpc.publicnode.com`
- `arbitrum-one-rpc.publicnode.com`
- `optimism-rpc.publicnode.com`
- `polygon-bor-rpc.publicnode.com`
- the host of `JARVIS_MARKET_SOLANA_RPC_A_URL` (already allowed when the
  memecoin paper trader runs)

## What is not counted

- Coins and tokens off the allowlist (on Solana only their count is shown).
- Unconfirmed Bitcoin transactions.
- Solana stake accounts, LP and liquidity positions, staked or lent tokens,
  and anything held through a contract (vaults, multisigs) rather than at the
  address itself.
- Extended public keys (xpub): add single addresses.
- Testnets.
- Assets without a usable EUR price (shown, not counted).
- Manual holdings (`/v1/portfolio`) stay cost basis only and are never
  added to the crypto total. Add self-custody addresses only, not exchange
  deposit addresses, and do not also enter these coins as manual holdings.
  WBTC and BTC, and IBKR crypto positions, are counted separately.

## API

Owner session on all; `503` while off. Optional fields are absent, never
null.

- `GET /v1/trading/crypto`: `source`, `as_of` (oldest good refresh),
  `base_currency` `EUR`, `total_eur`, `chains_available`, `wallets[]`
  (`id`, `kind`, `chains`, `address` (EVM in EIP-55 form), `label?`,
  `status`, `error?`, `last_refresh_at?`, `other_tokens?`, `value_eur`,
  `balances[]` with `chain`, `asset`, `symbol`, `amount`, `price_eur?`,
  `value_eur?`), `assets[]` (`asset`, `symbol`, `amount`, `price_eur?`,
  `value_eur?`, `weight_pct?`).
- `POST /v1/trading/crypto/wallets` `{kind, address, label?, chains?}`
  (chains only and required for `evm`): `201`, `400`, `409` duplicate or
  "wallet limit reached". Body at most 4 KiB, unknown fields refused.
- `DELETE /v1/trading/crypto/wallets/{id}`: the wallet and its balances.
- `POST /v1/trading/crypto/refresh`: `202` or `429 {retry_after_secs}`.
- Realtime `trading.crypto.updated {as_of}`: a hint to reload; see
  `REALTIME_PROTOCOL.md`.
