//! RugCheck token report summary (`GET /v1/tokens/{mint}/report/summary`).
//!
//! RugCheck is a second opinion only: authorities, extensions and holders come
//! from our own cross-checked RPC reads. Risk names and descriptions are
//! untrusted text and must be fenced before they reach any prompt.

use std::time::Duration;

use reqwest::Url;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::http::{checked_url, Http};
use crate::{opt_decimal, validate_address, MarketDataError, Provider};

const BASE: &str = "https://api.rugcheck.xyz";
// ponytail: RugCheck publishes no limit for keyless reads; 1 request/s is a
// guess, tune it from observed 429s.
const GAP: Duration = Duration::from_secs(1);

/// `dto.TokenCheckSummary` from RugCheck's published OpenAPI spec.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RugCheckSummary {
    #[serde(rename = "tokenProgram", default)]
    pub token_program: Option<String>,
    #[serde(rename = "tokenType", default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub risks: Vec<Risk>,
    #[serde(default)]
    pub score: Option<i64>,
    #[serde(default)]
    pub score_normalised: Option<i64>,
    /// Share of LP RugCheck considers locked, in percent. It does not say
    /// whether the LP is burned or time-locked, nor until when.
    #[serde(rename = "lpLockedPct", default, deserialize_with = "opt_decimal")]
    pub lp_locked_pct: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Risk {
    pub name: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub score: Option<i64>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub struct RugCheckClient {
    http: Http,
    base: String,
}

impl RugCheckClient {
    pub fn new() -> Result<Self, MarketDataError> {
        checked_url(BASE)?;
        Ok(Self {
            http: Http::new(Provider::RugCheck, GAP)?,
            base: BASE.to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn plain(base: &str) -> Self {
        Self {
            http: Http::plain(Provider::RugCheck, Duration::ZERO),
            base: base.to_owned(),
        }
    }

    pub async fn summary(&self, mint: &str) -> Result<RugCheckSummary, MarketDataError> {
        validate_address(mint)?;
        let url = Url::parse(&format!("{}/v1/tokens/{mint}/report/summary", self.base))
            .map_err(|_| MarketDataError::InvalidUrl)?;
        self.http.json(self.http.get(url)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, response, serve};

    const MINT: &str = "7GCihgDB8fe6KNjn2MYtkzZcRjQy3t9GHdC8uHYmW2hr";

    #[tokio::test]
    async fn parses_summary_fixture() {
        let (base, seen) = serve(ok_json(include_str!("../fixtures/rugcheck_summary.json"))).await;
        let summary = RugCheckClient::plain(&base).summary(MINT).await.unwrap();
        assert_eq!(summary.score_normalised, Some(41));
        assert_eq!(summary.lp_locked_pct, Some("99.87".parse().unwrap()));
        assert_eq!(summary.risks.len(), 2);
        assert_eq!(summary.risks[0].level.as_deref(), Some("danger"));
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(request.starts_with(&format!("GET /v1/tokens/{MINT}/report/summary ")));
    }

    #[tokio::test]
    async fn not_found_and_malformed_are_errors() {
        let (base, _) = serve(response("404 Not Found", "", r#"{"error":"not found"}"#)).await;
        assert!(matches!(
            RugCheckClient::plain(&base).summary(MINT).await,
            Err(MarketDataError::Status { status: 404, .. })
        ));
        let (base, _) = serve(ok_json(r#"{"risks":"none"}"#)).await;
        assert!(matches!(
            RugCheckClient::plain(&base).summary(MINT).await,
            Err(MarketDataError::Decode { .. })
        ));
        assert!(RugCheckClient::plain(&base)
            .summary("x/../y")
            .await
            .is_err());
    }
}
