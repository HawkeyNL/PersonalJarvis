//! Public balances and EUR prices for the read-only crypto wallets.
//!
//! - Bitcoin: mempool.space (Esplora), confirmed balance only.
//! - EVM chains: one JSON-RPC batch per chain and address, `eth_getBalance`
//!   plus ERC-20 `balanceOf` for each allowlisted token of that chain.
//! - Solana: the primary configured RPC, `getBalance` plus
//!   `getTokenAccountsByOwner` for the Token and Token-2022 programs.
//! - Prices: one CoinGecko `simple/price` call for the whole allowlist.
//!
//! Only assets in [`crate::wallet::TOKENS`] are counted, under the
//! allowlist's own key and decimals; a token name or symbol from a chain is
//! never read. Every amount is bounded (at most 20 integer digits) and a value
//! outside the bounds fails the read. Requests are built only from validated
//! addresses and fixed contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use reqwest::Url;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use time::OffsetDateTime;

use crate::http::{checked_url, Http, DEFAULT_BACKOFF};
use crate::rpc::{ReplyError, RpcClient, RpcEndpoint, BACKOFF_CODES};
use crate::token2022::{SPL_TOKEN_PROGRAM, TOKEN_2022_PROGRAM};
use crate::wallet::{parse_address, Chain, WalletKind, ASSETS, TOKENS};
use crate::{MarketDataError, Provider};

const MEMPOOL_BASE: &str = "https://mempool.space/api";
const COINGECKO_BASE: &str = "https://api.coingecko.com/api/v3";
/// mempool.space asks clients to stay well under its (unpublished) limit.
const MEMPOOL_GAP: Duration = Duration::from_millis(500);
/// The keyless CoinGecko pool is about 5 calls a minute per IP; one call
/// per pass and the 60 s refresh cooldown stay far below that. The gap is
/// under the pacer's queue limit so two close passes wait instead of failing.
const COINGECKO_GAP: Duration = Duration::from_secs(4);
const EVM_GAP: Duration = Duration::from_millis(250);
const SOLANA_GAP_INDEX: u8 = 0;
/// A price older than this (by its own `last_updated_at`) is not used.
pub const PRICE_MAX_AGE: time::Duration = time::Duration::hours(1);
/// A price time further ahead than this is refused as invalid.
const PRICE_MAX_SKEW: time::Duration = time::Duration::minutes(5);
/// The most bitcoin there will ever be, in satoshi.
const MAX_SATS: u64 = 21_000_000 * 100_000_000;
/// Stored amounts and prices have at most 20 integer digits (schema 0023).
const MAX_VALUE: Decimal = Decimal::from_parts(0x63100000, 0x6BC75E2D, 0x5, false, 0);
/// Solana tokens off the allowlist are counted up to this.
const MAX_OTHER_TOKENS: u32 = 100_000;
/// ERC-20 `balanceOf(address)`.
const BALANCE_OF: &str = "0x70a08231";

/// One allowlisted asset held on one chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balance {
    pub chain: Chain,
    /// [`crate::wallet::Asset::key`].
    pub asset: &'static str,
    pub amount: Decimal,
}

/// The EUR price of one allowlisted asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    pub asset: &'static str,
    pub eur: Decimal,
    /// CoinGecko's `last_updated_at`.
    pub as_of: OffsetDateTime,
}

/// What a Solana address holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolanaHoldings {
    pub balances: Vec<Balance>,
    /// Token mints off the allowlist with a non-zero balance, counted only.
    pub other_tokens: u32,
}

/// The fixed failure code stored for a wallet (schema 0023).
pub fn failure_code(error: &MarketDataError) -> &'static str {
    match error {
        MarketDataError::RateLimited { .. } | MarketDataError::Busy { .. } => "rate_limited",
        // Only the token-account list can be that large for a Solana read.
        MarketDataError::TooLarge {
            provider: Provider::SolanaRpc(_),
        } => "too_many_tokens",
        MarketDataError::TooLarge { .. }
        | MarketDataError::Decode { .. }
        | MarketDataError::InvalidData { .. }
        | MarketDataError::NotFound(_)
        | MarketDataError::ProviderDisagreement(_) => "invalid_response",
        MarketDataError::InvalidUrl
        | MarketDataError::HostNotAllowed
        | MarketDataError::InvalidInput(_)
        | MarketDataError::Http { .. }
        | MarketDataError::Status { .. }
        | MarketDataError::Rpc { .. }
        | MarketDataError::NoRoute => "unavailable",
    }
}

fn invalid(provider: Provider, reason: &'static str) -> MarketDataError {
    MarketDataError::InvalidData { provider, reason }
}

/// `raw` base units with `decimals`, trailing zeros dropped; `None` above the
/// stored bound.
fn amount(raw: u128, decimals: u32) -> Option<Decimal> {
    let raw = i128::try_from(raw).ok()?;
    let value = Decimal::try_from_i128_with_scale(raw, decimals).ok()?;
    (value < MAX_VALUE).then(|| value.normalize())
}

/// The stored form of an address of `kind`, or an input error: only an
/// already validated address is ever put into a request.
fn checked(kind: WalletKind, address: &str) -> Result<(), MarketDataError> {
    match parse_address(kind, address) {
        Ok(stored) if stored == address => Ok(()),
        _ => Err(MarketDataError::InvalidInput("address")),
    }
}

/// Bitcoin balances from mempool.space (Esplora REST).
pub struct Esplora {
    http: Http,
    base: Url,
}

#[derive(Deserialize)]
struct EsploraAddress {
    address: String,
    chain_stats: EsploraStats,
}

#[derive(Deserialize)]
struct EsploraStats {
    funded_txo_sum: u64,
    spent_txo_sum: u64,
}

impl Esplora {
    pub fn new() -> Result<Self, MarketDataError> {
        Ok(Self {
            http: Http::new(Provider::Mempool, MEMPOOL_GAP)?,
            base: checked_url(MEMPOOL_BASE)?,
        })
    }

    #[cfg(test)]
    fn plain(base: &str) -> Self {
        Self {
            http: Http::plain(Provider::Mempool, Duration::ZERO),
            base: Url::parse(base).unwrap(),
        }
    }

    /// The confirmed balance; mempool (unconfirmed) amounts are ignored.
    pub async fn balance(&self, address: &str) -> Result<Balance, MarketDataError> {
        checked(WalletKind::Bitcoin, address)?;
        let url = Url::parse(&format!(
            "{}/address/{address}",
            self.base.as_str().trim_end_matches('/')
        ))
        .map_err(|_| MarketDataError::InvalidUrl)?;
        let reply: EsploraAddress = self.http.json(self.http.get(url)).await?;
        let provider = self.http.provider();
        if reply.address != address {
            return Err(invalid(provider, "reply is for another address"));
        }
        let sats = reply
            .chain_stats
            .funded_txo_sum
            .checked_sub(reply.chain_stats.spent_txo_sum)
            .filter(|sats| *sats <= MAX_SATS)
            .ok_or_else(|| invalid(provider, "balance out of range"))?;
        Ok(Balance {
            chain: Chain::Bitcoin,
            asset: "btc",
            amount: amount(u128::from(sats), 8).ok_or_else(|| invalid(provider, "balance"))?,
        })
    }
}

/// One EVM chain's JSON-RPC provider.
pub struct EvmRpc {
    chain: Chain,
    http: Http,
    endpoint: RpcEndpoint,
}

impl std::fmt::Debug for EvmRpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvmRpc")
            .field("chain", &self.chain)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[derive(Deserialize)]
struct EvmReply {
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    error: Option<ReplyError>,
}

/// A JSON-RPC quantity or 32-byte word, at most 96 significant bits.
fn uint96(hex: &str) -> Option<u128> {
    let digits = hex.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let significant = digits.trim_start_matches('0');
    match significant.len() {
        0 => Some(0),
        1..=24 => u128::from_str_radix(significant, 16).ok(),
        _ => None,
    }
}

impl EvmRpc {
    /// `chain` must be one of the EVM chains.
    pub fn new(chain: Chain, endpoint: RpcEndpoint) -> Result<Self, MarketDataError> {
        if matches!(chain, Chain::Bitcoin | Chain::Solana) {
            return Err(MarketDataError::InvalidInput("EVM chain"));
        }
        Ok(Self {
            chain,
            http: Http::new(Provider::EvmRpc(chain), EVM_GAP)?,
            endpoint,
        })
    }

    #[cfg(test)]
    fn plain(chain: Chain, base: &str) -> Self {
        Self {
            chain,
            http: Http::plain(Provider::EvmRpc(chain), Duration::ZERO),
            endpoint: RpcEndpoint::local(base),
        }
    }

    pub fn chain(&self) -> Chain {
        self.chain
    }

    /// The native coin and every allowlisted token of this chain held by
    /// `address` (stored form), non-zero ones only, in one batch.
    pub async fn balances(&self, address: &str) -> Result<Vec<Balance>, MarketDataError> {
        checked(WalletKind::Evm, address)?;
        let provider = self.http.provider();
        let tokens: Vec<_> = TOKENS.iter().filter(|t| t.chain == self.chain).collect();
        let padded = format!("{BALANCE_OF}{:0>64}", &address[2..]);
        let calls: Vec<Value> = tokens
            .iter()
            .enumerate()
            .map(|(id, token)| {
                let (method, params) = match token.contract {
                    None => ("eth_getBalance", json!([address, "latest"])),
                    Some(contract) => (
                        "eth_call",
                        json!([{ "to": contract.to_ascii_lowercase(), "data": padded }, "latest"]),
                    ),
                };
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            })
            .collect();
        let url = self.endpoint.url()?;
        let replies: Vec<EvmReply> = self.http.json(self.http.post(url).json(&calls)).await?;
        // Every request id exactly once; an error reply fails the chain.
        let mut results: Vec<Option<String>> = vec![None; tokens.len()];
        for reply in replies {
            if reply.jsonrpc != "2.0" {
                return Err(invalid(provider, "not a JSON-RPC 2.0 reply"));
            }
            if let Some(error) = reply.error {
                return Err(if BACKOFF_CODES.contains(&error.code) {
                    self.http.rate_limited(DEFAULT_BACKOFF)
                } else {
                    MarketDataError::Rpc {
                        provider,
                        code: error.code,
                    }
                });
            }
            let slot = usize::try_from(reply.id)
                .ok()
                .and_then(|id| results.get_mut(id))
                .ok_or_else(|| invalid(provider, "reply to an unknown request"))?;
            if slot.is_some() {
                return Err(invalid(provider, "duplicate reply"));
            }
            *slot = Some(
                reply
                    .result
                    .ok_or_else(|| invalid(provider, "reply without result"))?,
            );
        }
        let mut balances = Vec::new();
        for (token, result) in tokens.iter().zip(results) {
            let result = result.ok_or_else(|| invalid(provider, "missing reply"))?;
            let raw = uint96(&result).ok_or_else(|| invalid(provider, "balance out of range"))?;
            let amount = amount(raw, token.decimals).ok_or_else(|| invalid(provider, "balance"))?;
            if !amount.is_zero() {
                balances.push(Balance {
                    chain: self.chain,
                    asset: token.asset,
                    amount,
                });
            }
        }
        Ok(balances)
    }
}

/// Solana balances from the primary configured RPC provider.
#[derive(Debug)]
pub struct SolanaBalances {
    rpc: RpcClient,
}

impl SolanaBalances {
    pub fn new(endpoint: RpcEndpoint) -> Result<Self, MarketDataError> {
        Ok(Self {
            rpc: RpcClient::new(SOLANA_GAP_INDEX, endpoint)?,
        })
    }

    #[cfg(test)]
    fn plain(base: &str) -> Self {
        Self {
            rpc: RpcClient::plain(SOLANA_GAP_INDEX, base),
        }
    }

    /// SOL and the allowlisted SPL tokens `owner` holds, non-zero ones only.
    /// Accounts of other mints are only counted. An allowlisted mint with
    /// other decimals than the allowlist's fails the read.
    pub async fn read(&self, owner: &str) -> Result<SolanaHoldings, MarketDataError> {
        checked(WalletKind::Solana, owner)?;
        let provider = Provider::SolanaRpc(SOLANA_GAP_INDEX);
        let lamports = self.rpc.balance(owner).await?;
        let mut accounts = self
            .rpc
            .token_accounts_by_owner(owner, SPL_TOKEN_PROGRAM)
            .await?;
        accounts.extend(
            self.rpc
                .token_accounts_by_owner(owner, TOKEN_2022_PROGRAM)
                .await?,
        );
        let mut raw: BTreeMap<&'static str, (u128, u32)> = BTreeMap::new();
        let mut others = BTreeSet::new();
        let mut unparsed: usize = 0;
        for account in &accounts {
            // Anyone can open a token account for any owner; one the provider
            // could not parse is counted, not a reason to fail the read.
            let Some(account) = account else {
                unparsed += 1;
                continue;
            };
            if account.owner != owner {
                return Err(invalid(provider, "token account of another owner"));
            }
            let amount = account.token_amount.amount;
            let known = TOKENS
                .iter()
                .find(|t| t.chain == Chain::Solana && t.contract == Some(account.mint.as_str()));
            match known {
                Some(token) => {
                    if u32::from(account.token_amount.decimals) != token.decimals {
                        return Err(invalid(
                            provider,
                            "token decimals differ from the allowlist",
                        ));
                    }
                    let entry = raw.entry(token.asset).or_insert((0, token.decimals));
                    entry.0 += u128::from(amount);
                }
                None if amount > 0 => {
                    others.insert(account.mint.as_str());
                }
                None => {}
            }
        }
        let mut balances = Vec::new();
        let sol = amount(u128::from(lamports), 9).ok_or_else(|| invalid(provider, "balance"))?;
        if !sol.is_zero() {
            balances.push(Balance {
                chain: Chain::Solana,
                asset: "sol",
                amount: sol,
            });
        }
        for (asset, (units, decimals)) in raw {
            let amount = amount(units, decimals).ok_or_else(|| invalid(provider, "balance"))?;
            if !amount.is_zero() {
                balances.push(Balance {
                    chain: Chain::Solana,
                    asset,
                    amount,
                });
            }
        }
        Ok(SolanaHoldings {
            balances,
            other_tokens: u32::try_from(others.len().saturating_add(unparsed))
                .unwrap_or(u32::MAX)
                .min(MAX_OTHER_TOKENS),
        })
    }
}

/// EUR prices from CoinGecko's keyless API.
pub struct CoinGecko {
    http: Http,
    base: Url,
}

impl CoinGecko {
    pub fn new() -> Result<Self, MarketDataError> {
        Ok(Self {
            http: Http::new(Provider::CoinGecko, COINGECKO_GAP)?,
            base: checked_url(COINGECKO_BASE)?,
        })
    }

    #[cfg(test)]
    fn plain(base: &str) -> Self {
        Self {
            http: Http::plain(Provider::CoinGecko, Duration::ZERO),
            base: Url::parse(base).unwrap(),
        }
    }

    /// One request for the whole allowlist, whatever is held. Assets without
    /// a usable price (missing, not above zero, unparseable, older than
    /// [`PRICE_MAX_AGE`]) are left out: they stay unpriced.
    pub async fn prices(&self) -> Result<Vec<Price>, MarketDataError> {
        let ids: Vec<&str> = ASSETS.iter().map(|asset| asset.coingecko_id).collect();
        let url = Url::parse(&format!(
            "{}/simple/price?ids={}&vs_currencies=eur&include_last_updated_at=true",
            self.base.as_str().trim_end_matches('/'),
            ids.join(",")
        ))
        .map_err(|_| MarketDataError::InvalidUrl)?;
        let reply: Map<String, Value> = self.http.json(self.http.get(url)).await?;
        Ok(parse_prices(&reply, OffsetDateTime::now_utc()))
    }
}

fn parse_prices(reply: &Map<String, Value>, now: OffsetDateTime) -> Vec<Price> {
    ASSETS
        .iter()
        .filter_map(|asset| {
            let entry = reply.get(asset.coingecko_id)?;
            let eur = match entry.get("eur")? {
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            let eur = eur
                .parse::<Decimal>()
                .or_else(|_| Decimal::from_scientific(&eur))
                .ok()
                .filter(|eur| *eur > Decimal::ZERO && *eur < MAX_VALUE)?
                .normalize();
            let as_of = entry
                .get("last_updated_at")?
                .as_i64()
                .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
                .filter(|at| now - *at <= PRICE_MAX_AGE && *at - now <= PRICE_MAX_SKEW)?;
            Some(Price {
                asset: asset.key,
                eur,
                as_of,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, response, serve, serve_with};

    const BTC: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const EVM: &str = "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed";
    const SOL: &str = "So11111111111111111111111111111111111111112";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    fn esplora(funded: &str, spent: &str) -> String {
        format!(
            r#"{{"address":"{BTC}","chain_stats":{{"funded_txo_count":2,"funded_txo_sum":{funded},"spent_txo_count":1,"spent_txo_sum":{spent},"tx_count":3}},"mempool_stats":{{"funded_txo_sum":999999,"spent_txo_sum":0}}}}"#
        )
    }

    #[test]
    fn new_hosts_pass_the_allowlist() {
        for url in [
            "https://mempool.space/api/address/x",
            "https://api.coingecko.com/api/v3/simple/price",
            "https://ethereum-rpc.publicnode.com",
            "https://base-rpc.publicnode.com",
            "https://arbitrum-one-rpc.publicnode.com",
            "https://optimism-rpc.publicnode.com",
            "https://polygon-bor-rpc.publicnode.com",
        ] {
            assert!(checked_url(url).is_ok(), "{url}");
        }
        assert!(Esplora::new().is_ok());
        assert!(CoinGecko::new().is_ok());
        assert!(matches!(
            RpcEndpoint::new("https://eth.evil.example".to_owned()),
            Err(MarketDataError::HostNotAllowed)
        ));
        let endpoint = RpcEndpoint::new("https://base-rpc.publicnode.com".to_owned()).unwrap();
        assert!(EvmRpc::new(Chain::Solana, endpoint).is_err());
    }

    #[tokio::test]
    async fn esplora_reads_the_confirmed_balance() {
        let (base, seen) = serve(ok_json(&esplora("150000000", "50000000"))).await;
        let balance = Esplora::plain(&base).balance(BTC).await.unwrap();
        assert_eq!(
            (balance.chain, balance.asset, balance.amount),
            (Chain::Bitcoin, "btc", d("1"))
        );
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(
            request.starts_with(&format!("GET /address/{BTC} ")),
            "{request}"
        );
    }

    #[tokio::test]
    async fn esplora_refuses_negative_overflowing_and_foreign_replies() {
        for body in [
            esplora("1", "2"),
            esplora("18446744073709551616", "0"),
            esplora("-5", "0"),
            esplora(&(MAX_SATS + 1).to_string(), "0"),
            esplora("1", "0").replace(BTC, "bc1qother"),
        ] {
            let (base, _) = serve(ok_json(&body)).await;
            let error = Esplora::plain(&base).balance(BTC).await.unwrap_err();
            assert_eq!(failure_code(&error), "invalid_response", "{body}");
        }
        let (base, _) = serve(ok_json("{}")).await;
        for bad in [
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
            "../x",
            "BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4",
        ] {
            assert!(matches!(
                Esplora::plain(&base).balance(bad).await,
                Err(MarketDataError::InvalidInput(_))
            ));
        }
    }

    #[tokio::test]
    async fn rate_limits_are_typed() {
        let (base, _) = serve(response(
            "429 Too Many Requests",
            "retry-after: 9\r\n",
            "{}",
        ))
        .await;
        let error = Esplora::plain(&base).balance(BTC).await.unwrap_err();
        assert!(matches!(
            error,
            MarketDataError::RateLimited {
                retry_after_secs: 9,
                ..
            }
        ));
        assert_eq!(failure_code(&error), "rate_limited");
    }

    /// Replies for Base: ETH, then USDC, EURC and WETH in allowlist order.
    fn evm_reply(order: &[usize], values: &[&str]) -> String {
        let replies: Vec<String> = order
            .iter()
            .map(|&id| format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"{}"}}"#, values[id]))
            .collect();
        format!("[{}]", replies.join(","))
    }

    const WORD_ZERO: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

    #[tokio::test]
    async fn evm_batch_matches_replies_by_id() {
        // 1.5 ETH; 2.5 USDC (6 decimals), out of order.
        let usdc = "0x00000000000000000000000000000000000000000000000000000000002625a0";
        let values = ["0x14d1120d7b160000", usdc, WORD_ZERO, WORD_ZERO];
        let (base, seen) = serve(ok_json(&evm_reply(&[3, 1, 0, 2], &values))).await;
        let rpc = EvmRpc::plain(Chain::Base, &base);
        let balances = rpc.balances(EVM).await.unwrap();
        assert_eq!(
            balances,
            [
                Balance {
                    chain: Chain::Base,
                    asset: "eth",
                    amount: d("1.5")
                },
                Balance {
                    chain: Chain::Base,
                    asset: "usdc",
                    amount: d("2.5")
                },
            ]
        );
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body.as_array().map(Vec::len), Some(4));
        assert_eq!(body[0]["method"], "eth_getBalance");
        assert_eq!(body[1]["method"], "eth_call");
        assert_eq!(
            body[1]["params"][0]["to"],
            "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
        );
        assert_eq!(
            body[1]["params"][0]["data"],
            "0x70a082310000000000000000000000005aaeb6053f3e94c9b9a09f33669435e7ef1beaed"
        );
    }

    #[tokio::test]
    async fn evm_batch_fails_closed() {
        let zero = [WORD_ZERO; 4];
        let too_big = "0x0000000000000000000000000000000000000001000000000000000000000000";
        let cases = [
            // A reply missing.
            (evm_reply(&[0, 1, 2], &zero), "invalid_response"),
            // A reply twice.
            (evm_reply(&[0, 1, 2, 2], &zero), "invalid_response"),
            // An id that was not asked.
            (evm_reply(&[0, 1, 2, 7], &[WORD_ZERO; 8]), "invalid_response"),
            // At or above 2^96.
            (evm_reply(&[0, 1, 2, 3], &[too_big, WORD_ZERO, WORD_ZERO, WORD_ZERO]), "invalid_response"),
            // Within 2^96 but more than 20 integer digits of USDC.
            (evm_reply(&[0, 1, 2, 3], &[WORD_ZERO, "0xffffffffffffffffffffffff", WORD_ZERO, WORD_ZERO]), "invalid_response"),
            // An empty result (no contract there).
            (evm_reply(&[0, 1, 2, 3], &["0x", WORD_ZERO, WORD_ZERO, WORD_ZERO]), "invalid_response"),
            (
                r#"[{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"header not found"}}]"#.to_owned(),
                "unavailable",
            ),
            (
                r#"[{"jsonrpc":"2.0","id":0,"error":{"code":-32005,"message":"limit"}}]"#.to_owned(),
                "rate_limited",
            ),
            (r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32600}}"#.to_owned(), "invalid_response"),
        ];
        for (body, code) in cases {
            let (base, _) = serve(ok_json(&body)).await;
            let error = EvmRpc::plain(Chain::Base, &base)
                .balances(EVM)
                .await
                .unwrap_err();
            assert_eq!(failure_code(&error), code, "{body}");
        }
        let (base, _) = serve(ok_json("[]")).await;
        let mixed = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        assert!(matches!(
            EvmRpc::plain(Chain::Base, &base).balances(mixed).await,
            Err(MarketDataError::InvalidInput(_))
        ));
    }

    #[test]
    fn uint96_bounds() {
        assert_eq!(uint96("0x0"), Some(0));
        assert_eq!(uint96("0xff"), Some(255));
        assert_eq!(
            uint96(&format!("0x{}", "f".repeat(24))),
            Some((1 << 96) - 1)
        );
        for bad in [
            "",
            "0x",
            "ff",
            "0xg1",
            &format!("0x1{}", "0".repeat(24)),
            &format!("0x{}", "0".repeat(65)),
        ] {
            assert_eq!(uint96(bad), None, "{bad}");
        }
    }

    fn token_account(mint: &str, owner: &str, amount: &str, decimals: u8) -> String {
        format!(
            r#"{{"pubkey":"x","account":{{"data":{{"program":"spl-token","parsed":{{"info":{{"isNative":false,"mint":"{mint}","owner":"{owner}","state":"initialized","tokenAmount":{{"amount":"{amount}","decimals":{decimals},"uiAmount":1.0,"uiAmountString":"1"}}}},"type":"account"}},"space":165}},"executable":false,"lamports":2039280,"owner":"{SPL_TOKEN_PROGRAM}","rentEpoch":0,"space":165}}}}"#
        )
    }

    fn rpc_value(value: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":1}},"value":{value}}}}}"#)
    }

    /// Answers getBalance with 2.5 SOL, the Token program with `spl` and
    /// Token-2022 with `token_2022`.
    async fn solana(spl: Vec<String>, token_2022: Vec<String>) -> SolanaBalances {
        let (base, _) = serve_with(move |request| {
            let text = String::from_utf8_lossy(request);
            let body = if text.contains("getBalance") {
                rpc_value("2500000000")
            } else if text.contains(TOKEN_2022_PROGRAM) {
                rpc_value(&format!("[{}]", token_2022.join(",")))
            } else {
                rpc_value(&format!("[{}]", spl.join(",")))
            };
            ok_json(&body)
        })
        .await;
        SolanaBalances::plain(&base)
    }

    #[tokio::test]
    async fn solana_counts_allowlisted_tokens_and_counts_the_rest() {
        let other = "DezXAZ8z7PnrnRJjz3wXBoRgixCa6xjnB7YaB1pPB263";
        let reader = solana(
            vec![
                token_account(USDC, SOL, "1500000", 6),
                token_account(USDC, SOL, "500000", 6),
                token_account(other, SOL, "7", 5),
                token_account("Fake1111111111111111111111111111111111111111", SOL, "0", 2),
            ],
            vec![
                // A Token-2022 account the provider could not parse.
                r#"{"pubkey":"y","account":{"data":["AAAA","base64"],"executable":false,"lamports":1,"owner":"TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb","rentEpoch":0,"space":3}}"#.to_owned(),
                token_account(
                "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
                SOL,
                "1",
                6,
            )],
        )
        .await;
        let holdings = reader.read(SOL).await.unwrap();
        assert_eq!(
            holdings,
            SolanaHoldings {
                balances: vec![
                    Balance {
                        chain: Chain::Solana,
                        asset: "sol",
                        amount: d("2.5")
                    },
                    Balance {
                        chain: Chain::Solana,
                        asset: "usdc",
                        amount: d("2")
                    },
                ],
                other_tokens: 3,
            }
        );
    }

    #[tokio::test]
    async fn solana_refuses_wrong_decimals_foreign_owner_and_oversize() {
        let reader = solana(vec![token_account(USDC, SOL, "1", 9)], vec![]).await;
        let error = reader.read(SOL).await.unwrap_err();
        assert_eq!(failure_code(&error), "invalid_response");

        let stranger = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";
        let reader = solana(vec![token_account(USDC, stranger, "1", 6)], vec![]).await;
        assert_eq!(
            failure_code(&reader.read(SOL).await.unwrap_err()),
            "invalid_response"
        );

        let many = vec![token_account(USDC, SOL, "1", 6); 2 * 1024 * 1024 / 400];
        let reader = solana(many, vec![]).await;
        let error = reader.read(SOL).await.unwrap_err();
        assert!(matches!(error, MarketDataError::TooLarge { .. }));
        assert_eq!(failure_code(&error), "too_many_tokens");
    }

    #[test]
    fn prices_keep_only_fresh_positive_known_values() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let at = |ago: i64| now.unix_timestamp() - ago;
        let reply: Map<String, Value> = serde_json::from_value(json!({
            "bitcoin": {"eur": 61234.5, "last_updated_at": at(60)},
            "ethereum": {"eur": 0, "last_updated_at": at(60)},
            "solana": {"eur": -3, "last_updated_at": at(60)},
            "usd-coin": {"eur": 0.86, "last_updated_at": at(2 * 3600)},
            "tether": {"eur": "0.86", "last_updated_at": at(60)},
            "euro-coin": {"eur": 1.0},
            "weth": {"eur": 1e-7, "last_updated_at": at(-3600)},
            "dai": {"eur": 0.000001, "last_updated_at": at(0)},
            "not-on-the-list": {"eur": 5, "last_updated_at": at(0)},
        }))
        .unwrap();
        let prices = parse_prices(&reply, now);
        let got: Vec<(&str, Decimal)> = prices.iter().map(|p| (p.asset, p.eur)).collect();
        assert_eq!(got, [("btc", d("61234.5")), ("dai", d("0.000001"))]);
        assert_eq!(prices[0].as_of, now - time::Duration::seconds(60));
    }

    #[tokio::test]
    async fn coingecko_asks_for_the_whole_allowlist_in_eur() {
        let (base, seen) = serve(ok_json(r#"{"bitcoin":{"eur":1}}"#)).await;
        // Too old to use, so nothing is priced.
        assert!(CoinGecko::plain(&base).prices().await.unwrap().is_empty());
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        let line = request.lines().next().unwrap();
        assert!(
            line.starts_with("GET /simple/price?ids=bitcoin,ethereum,"),
            "{line}"
        );
        assert!(line.contains("&vs_currencies=eur&include_last_updated_at=true"));
        for asset in ASSETS {
            assert!(line.contains(asset.coingecko_id));
        }
    }
}
