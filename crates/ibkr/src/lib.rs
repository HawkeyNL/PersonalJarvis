//! Read-only IBKR adapter over the Client Portal Web API (see ADR-013).
//!
//! The interactive login (browser SSO + 2FA) happens in the IBKR Client Portal
//! Gateway; this client only performs read-only GETs against the local gateway.
//! Broker-provided market values are surfaced as `f64` (display only) — our own
//! money math uses Decimal elsewhere.

use serde::{Deserialize, Serialize};

/// Errors from the IBKR adapter.
#[derive(Debug, thiserror::Error)]
pub enum IbkrError {
    #[error("gateway request failed")]
    Http(#[from] reqwest::Error),
    #[error("gateway is not authenticated")]
    NotAuthenticated,
    #[error("no account available")]
    NoAccount,
}

/// Session/authentication status of the gateway.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AuthStatus {
    #[serde(default)]
    pub authenticated: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub competing: bool,
}

/// An IBKR account visible in the current session.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "accountTitle", default)]
    pub title: Option<String>,
}

/// A single position (read-only; broker-provided values).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Position {
    #[serde(default)]
    pub conid: i64,
    #[serde(rename = "contractDesc", default)]
    pub symbol: String,
    #[serde(default)]
    pub position: f64,
    #[serde(rename = "avgCost", default)]
    pub avg_cost: f64,
    #[serde(rename = "mktPrice", default)]
    pub mkt_price: f64,
    #[serde(rename = "mktValue", default)]
    pub mkt_value: f64,
    #[serde(default)]
    pub currency: String,
}

/// Read-only client for the IBKR Client Portal Web API.
pub struct IbkrClient {
    base: String,
    http: reqwest::Client,
}

impl IbkrClient {
    /// Create a client for the given gateway base URL (e.g.
    /// `https://localhost:5000/v1/api`). The local gateway uses a self-signed
    /// certificate, so certificate verification is disabled for it.
    pub fn new(base_url: &str) -> Result<Self, IbkrError> {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Session/auth status. Fails if the gateway is unreachable.
    pub async fn auth_status(&self) -> Result<AuthStatus, IbkrError> {
        let url = format!("{}/iserver/auth/status", self.base);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Accounts visible in the current session.
    pub async fn accounts(&self) -> Result<Vec<Account>, IbkrError> {
        let url = format!("{}/portfolio/accounts", self.base);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Positions for an account (first page).
    pub async fn positions(&self, account_id: &str) -> Result<Vec<Position>, IbkrError> {
        let url = format!("{}/portfolio/{}/positions/0", self.base, account_id);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_auth_status() {
        let json = r#"{"authenticated":true,"competing":false,"connected":true,"MAC":"..."}"#;
        let status: AuthStatus = serde_json::from_str(json).unwrap();
        assert!(status.authenticated);
        assert!(status.connected);
    }

    #[test]
    fn parses_accounts() {
        let json = r#"[{"id":"DU1234567","accountId":"DU1234567","accountTitle":"Paper"}]"#;
        let accounts: Vec<Account> = serde_json::from_str(json).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].account_id, "DU1234567");
        assert_eq!(accounts[0].title.as_deref(), Some("Paper"));
    }

    #[test]
    fn parses_positions() {
        let json = r#"[{"conid":265598,"contractDesc":"AAPL","position":12.0,"avgCost":178.5,"mktPrice":190.1,"mktValue":2281.2,"currency":"USD"}]"#;
        let positions: Vec<Position> = serde_json::from_str(json).unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "AAPL");
        assert_eq!(positions[0].conid, 265598);
        assert_eq!(positions[0].currency, "USD");
    }
}
