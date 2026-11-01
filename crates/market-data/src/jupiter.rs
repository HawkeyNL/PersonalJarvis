//! Jupiter quotes, read-only.
//!
//! Swap API v2 replaced the old `/quote` endpoint with `GET /swap/v2/order`:
//! called **without a `taker`** it returns only a quote and no transaction.
//! [`QuoteRequest`] has no taker field, so this client cannot ask for a
//! transaction, and a reply that carries one anyway is refused. No execute,
//! build or submit endpoint is called.

use std::time::Duration;

use reqwest::{header::HeaderValue, Url};
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{checked_url, Http};
use crate::{opt_decimal, u64_string, validate_address, ApiKey, MarketDataError, Provider};

const ORDER_URL: &str = "https://api.jup.ag/swap/v2/order";
/// Keyless requests are limited to 0.5/s; with a key we stay at 1/s.
// ponytail: 1/s fits the smallest paid tier; raise it from the owner's plan.
const GAP_WITH_KEY: Duration = Duration::from_secs(1);
const GAP_KEYLESS: Duration = Duration::from_millis(2_100);

/// What to quote: sell `amount` raw units of `input_mint` for `output_mint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteRequest {
    pub input_mint: String,
    pub output_mint: String,
    pub amount: u64,
    /// `None` lets Jupiter pick; otherwise 0..=10 000.
    pub slippage_bps: Option<u16>,
}

/// A quote. Amounts in raw token units.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub input_mint: String,
    pub output_mint: String,
    pub in_amount: u64,
    pub out_amount: u64,
    /// Price impact in percentage points (`1.5` = 1.5%), if reported.
    pub price_impact_pct: Option<Decimal>,
    /// Jupiter's USD valuation of each side, if reported.
    pub in_usd_value: Option<Decimal>,
    pub out_usd_value: Option<Decimal>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderReply {
    input_mint: String,
    output_mint: String,
    #[serde(deserialize_with = "u64_string")]
    in_amount: u64,
    #[serde(deserialize_with = "u64_string")]
    out_amount: u64,
    #[serde(default, deserialize_with = "opt_decimal")]
    price_impact: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    in_usd_value: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    out_usd_value: Option<Decimal>,
    #[serde(default)]
    transaction: Option<String>,
    #[serde(default)]
    error_code: Option<i64>,
}

pub struct JupiterClient {
    http: Http,
    url: String,
    api_key: Option<ApiKey>,
}

impl std::fmt::Debug for JupiterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JupiterClient")
            .field("api_key", &self.api_key)
            .finish_non_exhaustive()
    }
}

impl JupiterClient {
    /// The key, if any, goes in the `x-api-key` header, never in the URL.
    pub fn new(api_key: Option<ApiKey>) -> Result<Self, MarketDataError> {
        checked_url(ORDER_URL)?;
        let gap = if api_key.is_some() {
            GAP_WITH_KEY
        } else {
            GAP_KEYLESS
        };
        Ok(Self {
            http: Http::new(Provider::Jupiter, gap)?,
            url: ORDER_URL.to_owned(),
            api_key,
        })
    }

    #[cfg(test)]
    pub(crate) fn plain(base: &str, api_key: Option<ApiKey>) -> Self {
        Self {
            http: Http::plain(Provider::Jupiter, Duration::ZERO),
            url: format!("{base}/swap/v2/order"),
            api_key,
        }
    }

    pub async fn quote(&self, request: &QuoteRequest) -> Result<Quote, MarketDataError> {
        validate_address(&request.input_mint)?;
        validate_address(&request.output_mint)?;
        if request.amount == 0 {
            return Err(MarketDataError::InvalidInput("amount must be positive"));
        }
        if request.slippage_bps.is_some_and(|bps| bps > 10_000) {
            return Err(MarketDataError::InvalidInput("slippage above 100%"));
        }
        let mut url = Url::parse(&self.url).map_err(|_| MarketDataError::InvalidUrl)?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("inputMint", &request.input_mint)
                .append_pair("outputMint", &request.output_mint)
                .append_pair("amount", &request.amount.to_string());
            if let Some(bps) = request.slippage_bps {
                query.append_pair("slippageBps", &bps.to_string());
            }
        }
        let mut builder = self.http.get(url);
        if let Some(key) = &self.api_key {
            let mut value = HeaderValue::from_str(key.expose())
                .map_err(|_| MarketDataError::InvalidInput("API key"))?;
            value.set_sensitive(true);
            builder = builder.header("x-api-key", value);
        }
        let reply: OrderReply = self.http.json(builder).await?;
        let invalid = |reason| MarketDataError::InvalidData {
            provider: Provider::Jupiter,
            reason,
        };
        if reply
            .transaction
            .as_deref()
            .is_some_and(|tx| !tx.is_empty())
        {
            return Err(invalid("quote reply carries a transaction"));
        }
        if reply.error_code.is_some() {
            return Err(invalid("quote reply carries an error code"));
        }
        if reply.input_mint != request.input_mint
            || reply.output_mint != request.output_mint
            || reply.in_amount != request.amount
        {
            return Err(invalid("quote does not match the request"));
        }
        if reply.out_amount == 0 {
            return Err(MarketDataError::NoRoute);
        }
        Ok(Quote {
            input_mint: reply.input_mint,
            output_mint: reply.output_mint,
            in_amount: reply.in_amount,
            out_amount: reply.out_amount,
            price_impact_pct: reply.price_impact,
            in_usd_value: reply.in_usd_value,
            out_usd_value: reply.out_usd_value,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, response, serve};

    const MEME: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";
    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

    fn sell() -> QuoteRequest {
        QuoteRequest {
            input_mint: MEME.to_owned(),
            output_mint: USDC.to_owned(),
            amount: 2_500_000_000,
            slippage_bps: Some(150),
        }
    }

    fn fixture() -> String {
        include_str!("../fixtures/jupiter_order_quote.json").to_owned()
    }

    #[tokio::test]
    async fn parses_quote_and_sends_key_in_header_only() {
        let (base, seen) = serve(ok_json(&fixture())).await;
        let key = ApiKey::new("jup-secret-key".to_owned()).unwrap();
        let client = JupiterClient::plain(&base, Some(key));
        let quote = client.quote(&sell()).await.unwrap();
        assert_eq!(quote.out_amount, 1_043_211);
        assert_eq!(quote.price_impact_pct, Some("1.37".parse().unwrap()));
        assert_eq!(quote.out_usd_value, Some("1.0432".parse().unwrap()));

        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        let request_line = request.lines().next().unwrap();
        assert!(request_line.starts_with("GET /swap/v2/order?inputMint="));
        assert!(request_line.contains("slippageBps=150"));
        assert!(!request_line.contains("taker"));
        assert!(!request_line.contains("jup-secret-key"));
        assert!(request.contains("x-api-key: jup-secret-key"));
        assert!(!format!("{client:?}").contains("jup-secret-key"));
    }

    #[tokio::test]
    async fn refuses_transactions_mismatches_and_empty_routes() {
        let cases = [
            fixture().replace(r#""transaction": null"#, r#""transaction": "AQAB""#),
            fixture().replace(r#""inAmount": "2500000000""#, r#""inAmount": "1""#),
            fixture().replace(USDC, "So11111111111111111111111111111111111111112"),
            fixture().replace(
                r#""router": "metis""#,
                r#""router": "metis", "errorCode": 3"#,
            ),
        ];
        for body in cases {
            let (base, _) = serve(ok_json(&body)).await;
            let error = JupiterClient::plain(&base, None)
                .quote(&sell())
                .await
                .unwrap_err();
            assert!(
                matches!(error, MarketDataError::InvalidData { .. }),
                "{error}"
            );
        }
        let empty = fixture().replace(r#""outAmount": "1043211""#, r#""outAmount": "0""#);
        let (base, _) = serve(ok_json(&empty)).await;
        assert!(matches!(
            JupiterClient::plain(&base, None).quote(&sell()).await,
            Err(MarketDataError::NoRoute)
        ));
        let (base, _) = serve(response("400 Bad Request", "", r#"{"error":"no route"}"#)).await;
        assert!(matches!(
            JupiterClient::plain(&base, None).quote(&sell()).await,
            Err(MarketDataError::Status { status: 400, .. })
        ));
    }

    #[tokio::test]
    async fn rejects_bad_requests_before_sending() {
        let client = JupiterClient::plain("http://127.0.0.1:1", None);
        let mut zero = sell();
        zero.amount = 0;
        let mut slippage = sell();
        slippage.slippage_bps = Some(10_001);
        let mut mint = sell();
        mint.input_mint = "x&taker=evil".to_owned();
        for request in [zero, slippage, mint] {
            assert!(matches!(
                client.quote(&request).await,
                Err(MarketDataError::InvalidInput(_))
            ));
        }
    }
}
