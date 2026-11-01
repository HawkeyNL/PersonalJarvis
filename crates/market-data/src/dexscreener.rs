//! DexScreener public API (no key): Solana pairs by token or by pair address.
//!
//! "Boosts" are paid promotion. They are kept on [`Pair`] so they can be
//! shown and audited, and [`crate::snapshot`] copies them into the snapshot,
//! where no rule may treat them as a positive signal.

use std::time::Duration;

use reqwest::Url;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{checked_url, Http};
use crate::{opt_decimal, validate_address, MarketDataError, Provider};

const BASE: &str = "https://api.dexscreener.com";
/// The documented limit for the pair and token endpoints is 300 requests/min;
/// 250 ms keeps us at 240.
const GAP: Duration = Duration::from_millis(250);
/// The tokens endpoint takes at most 30 addresses per call.
pub const MAX_TOKENS_PER_CALL: usize = 30;

/// One pool as DexScreener reports it. Unknown or missing fields stay `None`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pair {
    pub chain_id: String,
    pub dex_id: String,
    pub pair_address: String,
    pub base_token: PairToken,
    pub quote_token: PairToken,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub price_usd: Option<Decimal>,
    #[serde(default)]
    pub liquidity: Option<Liquidity>,
    #[serde(default)]
    pub txns: Option<Txns>,
    #[serde(default)]
    pub price_change: Option<PriceChange>,
    /// Pool creation time in Unix milliseconds.
    #[serde(default)]
    pub pair_created_at: Option<i64>,
    #[serde(default)]
    pub boosts: Option<Boosts>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PairToken {
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Liquidity {
    #[serde(default, deserialize_with = "opt_decimal")]
    pub usd: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Txns {
    #[serde(default)]
    pub m5: Option<BuysSells>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct BuysSells {
    pub buys: u32,
    pub sells: u32,
}

/// Price changes in percent (`12.5` = +12.5%).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PriceChange {
    #[serde(default, deserialize_with = "opt_decimal")]
    pub m5: Option<Decimal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Boosts {
    #[serde(default)]
    pub active: u32,
}

#[derive(Deserialize)]
struct PairsReply {
    #[serde(default)]
    pairs: Option<Vec<Pair>>,
}

pub struct DexScreenerClient {
    http: Http,
    base: String,
}

impl DexScreenerClient {
    pub fn new() -> Result<Self, MarketDataError> {
        checked_url(BASE)?;
        Ok(Self {
            http: Http::new(Provider::DexScreener, GAP)?,
            base: BASE.to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn plain(base: &str) -> Self {
        Self {
            http: Http::plain(Provider::DexScreener, Duration::ZERO),
            base: base.to_owned(),
        }
    }

    fn url(&self, path: &str) -> Result<Url, MarketDataError> {
        Url::parse(&format!("{}{path}", self.base)).map_err(|_| MarketDataError::InvalidUrl)
    }

    /// Pairs for up to [`MAX_TOKENS_PER_CALL`] Solana token mints
    /// (`GET /tokens/v1/solana/{addresses}`).
    pub async fn tokens(&self, mints: &[&str]) -> Result<Vec<Pair>, MarketDataError> {
        if mints.is_empty() || mints.len() > MAX_TOKENS_PER_CALL {
            return Err(MarketDataError::InvalidInput("1 to 30 token addresses"));
        }
        for mint in mints {
            validate_address(mint)?;
        }
        let url = self.url(&format!("/tokens/v1/solana/{}", mints.join(",")))?;
        let pairs: Vec<Pair> = self.http.json(self.http.get(url)).await?;
        Ok(solana_only(pairs))
    }

    /// One Solana pair by pool address (`GET /latest/dex/pairs/solana/{pair}`).
    pub async fn pair(&self, pair_address: &str) -> Result<Option<Pair>, MarketDataError> {
        validate_address(pair_address)?;
        let url = self.url(&format!("/latest/dex/pairs/solana/{pair_address}"))?;
        let reply: PairsReply = self.http.json(self.http.get(url)).await?;
        Ok(solana_only(reply.pairs.unwrap_or_default())
            .into_iter()
            .find(|p| p.pair_address == pair_address))
    }
}

/// Drop anything the API returns for another chain.
fn solana_only(pairs: Vec<Pair>) -> Vec<Pair> {
    pairs
        .into_iter()
        .filter(|p| p.chain_id == "solana")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, serve};

    const MINT: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
    const PAIR: &str = "5zpyutJu9ee6jFymDGoK7F6S5Kczqtc9FomP3ueKuyA9";

    #[tokio::test]
    async fn parses_tokens_fixture_and_drops_other_chains() {
        let (base, seen) =
            serve(ok_json(include_str!("../fixtures/dexscreener_tokens.json"))).await;
        let pairs = DexScreenerClient::plain(&base)
            .tokens(&[MINT])
            .await
            .unwrap();
        assert_eq!(pairs.len(), 2);
        let main = &pairs[0];
        assert_eq!(main.base_token.address, MINT);
        assert_eq!(main.price_usd, Some("0.0004213".parse().unwrap()));
        assert_eq!(
            main.liquidity.as_ref().unwrap().usd,
            Some("184532.17".parse().unwrap())
        );
        assert_eq!(
            main.txns.as_ref().unwrap().m5,
            Some(BuysSells {
                buys: 41,
                sells: 17
            })
        );
        assert_eq!(
            main.price_change.as_ref().unwrap().m5,
            Some("3.25".parse().unwrap())
        );
        assert_eq!(main.pair_created_at, Some(1_761_900_000_000));
        assert_eq!(main.boosts, Some(Boosts { active: 500 }));
        // The second pool lacks most fields: they stay unknown.
        assert_eq!(pairs[1].price_usd, None);
        assert_eq!(pairs[1].liquidity, None);
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(request.starts_with(&format!("GET /tokens/v1/solana/{MINT} ")));
    }

    #[tokio::test]
    async fn parses_pair_fixture() {
        let (base, _) = serve(ok_json(include_str!("../fixtures/dexscreener_pair.json"))).await;
        let pair = DexScreenerClient::plain(&base)
            .pair(PAIR)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pair.pair_address, PAIR);

        let (base, _) = serve(ok_json(r#"{"schemaVersion":"1.0.0","pairs":null}"#)).await;
        assert_eq!(
            DexScreenerClient::plain(&base).pair(PAIR).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn rejects_bad_input_and_malformed_bodies() {
        let client = DexScreenerClient::plain("http://127.0.0.1:1");
        assert!(client.tokens(&[]).await.is_err());
        assert!(client.tokens(&[MINT; 31]).await.is_err());
        assert!(client.tokens(&["a,b"]).await.is_err());
        assert!(client.pair("../../x").await.is_err());

        for body in [
            r#"{"pairs":[]}"#,
            r#"[{"chainId":"solana"}]"#,
            r#"[{"chainId":"solana","dexId":"raydium","pairAddress":"p","baseToken":{"address":"a"},"quoteToken":{"address":"b"},"priceUsd":"NaN"}]"#,
        ] {
            let (base, _) = serve(ok_json(body)).await;
            let error = DexScreenerClient::plain(&base)
                .tokens(&[MINT])
                .await
                .unwrap_err();
            assert!(matches!(error, MarketDataError::Decode { .. }), "{body}");
        }
    }

    #[test]
    fn real_client_targets_the_allowlisted_host() {
        assert!(DexScreenerClient::new().is_ok());
    }
}
