//! Read-only Solana JSON-RPC reads against two independent providers.
//!
//! Only `getAccountInfo` (mint) and `getTokenLargestAccounts` are called, at
//! `confirmed` commitment. [`CrossCheckedRpc`] asks both providers and fails
//! with [`MarketDataError::ProviderDisagreement`] unless they agree exactly;
//! only it can produce the [`CrossChecked`] values the snapshot mapping takes.
// ponytail: response slots are not compared, so two providers that lag the
// same way agree on stale state; pass `minContextSlot` if that shows up.

use std::fmt;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::http::{checked_url, Http, DEFAULT_BACKOFF};
use crate::token2022::{parse_mint, MintAccount, TokenProgram};
use crate::{u64_string, validate_address, MarketDataError, Provider};

/// Spacing per provider: 4 requests/s, under the public mainnet per-method
/// limit (40 per 10 s).
const GAP: Duration = Duration::from_millis(250);

/// An RPC provider URL. Providers often put the API key in the path or query,
/// so the whole URL is treated as a secret: zeroized on drop, never shown by
/// `Debug`, and never put into an error.
pub struct RpcEndpoint {
    url: Zeroizing<String>,
    host: String,
}

impl RpcEndpoint {
    /// Accept a credential-free (no user/password) https URL on an
    /// allowlisted host.
    pub fn new(url: String) -> Result<Self, MarketDataError> {
        let url = Zeroizing::new(url);
        let host = checked_url(&url)?
            .host_str()
            .ok_or(MarketDataError::InvalidUrl)?
            .to_owned();
        Ok(Self { url, host })
    }
}

impl fmt::Debug for RpcEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcEndpoint")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// JSON-RPC error codes providers use for rate limits inside an HTTP 200.
const RATE_LIMIT_CODES: &[i64] = &[-32005, -32429];

/// One RPC provider. Crate-private: callers read through [`CrossCheckedRpc`].
pub(crate) struct RpcClient {
    http: Http,
    endpoint: RpcEndpoint,
}

impl fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcClient")
            .field("provider", &self.http.provider())
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[derive(Deserialize)]
struct Reply<T> {
    jsonrpc: String,
    id: Value,
    result: Option<T>,
    error: Option<ReplyError>,
}

#[derive(Deserialize)]
struct ReplyError {
    code: i64,
}

#[derive(Deserialize)]
struct WithContext<T> {
    value: T,
}

#[derive(Deserialize)]
struct AccountValue {
    /// `[data, encoding]`.
    data: (String, String),
    owner: String,
}

/// One token account from `getTokenLargestAccounts`, amount in raw units.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TokenAccountBalance {
    pub address: String,
    #[serde(deserialize_with = "u64_string")]
    pub amount: u64,
}

impl RpcClient {
    /// `index` names the provider in errors (`0` primary, `1` secondary).
    pub fn new(index: u8, endpoint: RpcEndpoint) -> Result<Self, MarketDataError> {
        Ok(Self {
            http: Http::new(Provider::SolanaRpc(index), GAP)?,
            endpoint,
        })
    }

    #[cfg(test)]
    pub(crate) fn plain(index: u8, base: &str) -> Self {
        Self {
            http: Http::plain(Provider::SolanaRpc(index), Duration::ZERO),
            endpoint: RpcEndpoint {
                url: Zeroizing::new(base.to_owned()),
                host: "localhost".to_owned(),
            },
        }
    }

    fn invalid(&self, reason: &'static str) -> MarketDataError {
        MarketDataError::InvalidData {
            provider: self.http.provider(),
            reason,
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<T, MarketDataError> {
        // reqwest keeps its own (not zeroized) copy of the URL for the request.
        let url = Url::parse(&self.endpoint.url).map_err(|_| MarketDataError::InvalidUrl)?;
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let reply: Reply<T> = self.http.json(self.http.post(url).json(&body)).await?;
        if reply.jsonrpc != "2.0" || reply.id != json!(1) {
            return Err(self.invalid("not a JSON-RPC 2.0 reply to this request"));
        }
        match (reply.result, reply.error) {
            (_, Some(error)) if RATE_LIMIT_CODES.contains(&error.code) => {
                Err(self.http.rate_limited(DEFAULT_BACKOFF))
            }
            (_, Some(error)) => Err(MarketDataError::Rpc {
                provider: self.http.provider(),
                code: error.code,
            }),
            (Some(result), None) => Ok(result),
            (None, None) => Err(self.invalid("reply has neither result nor error")),
        }
    }

    /// Read and parse a mint account (SPL Token or Token-2022).
    pub async fn mint(&self, mint: &str) -> Result<MintAccount, MarketDataError> {
        validate_address(mint)?;
        let reply: WithContext<Option<AccountValue>> = self
            .call(
                "getAccountInfo",
                json!([mint, { "encoding": "base64", "commitment": "confirmed" }]),
            )
            .await?;
        let account = reply
            .value
            .ok_or(MarketDataError::NotFound("mint account"))?;
        let program = TokenProgram::from_owner(&account.owner)
            .ok_or_else(|| self.invalid("account is not owned by a token program"))?;
        let (data, encoding) = account.data;
        if encoding != "base64" {
            return Err(self.invalid("account data is not base64"));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| self.invalid("account data is not valid base64"))?;
        parse_mint(program, &bytes).map_err(|e| self.invalid(e.0))
    }

    /// The largest token accounts of `mint` (at most 20, per the RPC spec).
    pub async fn largest_accounts(
        &self,
        mint: &str,
    ) -> Result<Vec<TokenAccountBalance>, MarketDataError> {
        validate_address(mint)?;
        let reply: WithContext<Vec<TokenAccountBalance>> = self
            .call(
                "getTokenLargestAccounts",
                json!([mint, { "commitment": "confirmed" }]),
            )
            .await?;
        Ok(reply.value)
    }
}

/// Two independent RPC providers that must agree on every read.
#[derive(Debug)]
pub struct CrossCheckedRpc {
    primary: RpcClient,
    secondary: RpcClient,
}

impl CrossCheckedRpc {
    /// Refuses two endpoints on the same host: that is one provider, not two.
    pub fn new(primary: RpcEndpoint, secondary: RpcEndpoint) -> Result<Self, MarketDataError> {
        if primary.host == secondary.host {
            return Err(MarketDataError::InvalidInput(
                "RPC providers must be independent",
            ));
        }
        Ok(Self {
            primary: RpcClient::new(0, primary)?,
            secondary: RpcClient::new(1, secondary)?,
        })
    }

    #[cfg(test)]
    fn from_clients(primary: RpcClient, secondary: RpcClient) -> Self {
        Self { primary, secondary }
    }

    pub async fn mint(&self, mint: &str) -> Result<CrossChecked<MintAccount>, MarketDataError> {
        let (a, b) = tokio::join!(self.primary.mint(mint), self.secondary.mint(mint));
        agree(a?, b?, "mint account")
    }

    /// Both lists sorted by amount (largest first) then address, so ties in
    /// a different order are not a disagreement.
    // ponytail: exact match; balances that move between the two reads fail
    // the check, add a tolerance only if live churn makes it flap.
    pub async fn largest_accounts(
        &self,
        mint: &str,
    ) -> Result<CrossChecked<Vec<TokenAccountBalance>>, MarketDataError> {
        let (a, b) = tokio::join!(
            self.primary.largest_accounts(mint),
            self.secondary.largest_accounts(mint)
        );
        let sort = |mut v: Vec<TokenAccountBalance>| {
            v.sort_by(|x, y| {
                y.amount
                    .cmp(&x.amount)
                    .then_with(|| x.address.cmp(&y.address))
            });
            v
        };
        agree(sort(a?), sort(b?), "largest token accounts")
    }
}

/// A value two independent RPC providers returned identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossChecked<T>(pub(crate) T);

impl<T> CrossChecked<T> {
    pub fn get(&self) -> &T {
        &self.0
    }
}

/// The value both providers returned, or an error naming what differs.
fn agree<T: PartialEq>(a: T, b: T, what: &'static str) -> Result<CrossChecked<T>, MarketDataError> {
    if a == b {
        Ok(CrossChecked(a))
    } else {
        Err(MarketDataError::ProviderDisagreement(what))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{ok_json, serve};
    use crate::token2022::tests::{base_mint, mint_2022};
    use jarvis_onchain::types::ExtensionKind;

    fn account_info(owner: &str, data: &[u8]) -> String {
        include_str!("../fixtures/rpc_get_account_info.json")
            .replace("{OWNER}", owner)
            .replace("{DATA}", &STANDARD.encode(data))
    }

    async fn client(
        index: u8,
        body: &str,
    ) -> (RpcClient, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let (base, seen) = serve(ok_json(body)).await;
        (RpcClient::plain(index, &base), seen)
    }

    #[tokio::test]
    async fn reads_a_token_2022_mint() {
        let data = mint_2022(&[(18, vec![0; 64])]);
        let (rpc, seen) = client(
            0,
            &account_info(crate::token2022::TOKEN_2022_PROGRAM, &data),
        )
        .await;
        let mint = rpc
            .mint("So11111111111111111111111111111111111111112")
            .await
            .unwrap();
        assert_eq!(mint.program, TokenProgram::Token2022);
        assert_eq!(mint.extensions[0].kind, ExtensionKind::MetadataPointer);
        let request = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(request.starts_with("POST "));
        assert!(request.contains(r#""method":"getAccountInfo""#));
        assert!(request.contains(r#""encoding":"base64""#));
    }

    #[tokio::test]
    async fn refuses_non_token_owner_null_account_and_rpc_errors() {
        let mint = "So11111111111111111111111111111111111111112";
        let (rpc, _) = client(
            0,
            &account_info("11111111111111111111111111111111", &base_mint(None, None)),
        )
        .await;
        assert!(matches!(
            rpc.mint(mint).await,
            Err(MarketDataError::InvalidData { .. })
        ));

        let (rpc, _) = client(
            0,
            r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":null}}"#,
        )
        .await;
        assert!(matches!(
            rpc.mint(mint).await,
            Err(MarketDataError::NotFound(_))
        ));

        let (rpc, _) = client(
            0,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Invalid param"}}"#,
        )
        .await;
        assert!(matches!(
            rpc.mint(mint).await,
            Err(MarketDataError::Rpc { code: -32602, .. })
        ));

        let (rpc, _) = client(
            0,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32429,"message":"rate limited"}}"#,
        )
        .await;
        for _ in 0..2 {
            assert!(matches!(
                rpc.mint(mint).await,
                Err(MarketDataError::RateLimited { .. })
            ));
        }

        let (rpc, _) = client(
            0,
            r#"{"jsonrpc":"2.0","id":7,"result":{"context":{"slot":1},"value":null}}"#,
        )
        .await;
        assert!(matches!(
            rpc.mint(mint).await,
            Err(MarketDataError::InvalidData { .. })
        ));

        let (rpc, _) = client(
            0,
            &account_info(crate::token2022::SPL_TOKEN_PROGRAM, &[1, 2, 3]),
        )
        .await;
        assert!(matches!(
            rpc.mint(mint).await,
            Err(MarketDataError::InvalidData { .. })
        ));

        assert!(matches!(
            rpc.mint("../etc").await,
            Err(MarketDataError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn reads_largest_accounts() {
        let (rpc, _) = client(
            0,
            include_str!("../fixtures/rpc_get_token_largest_accounts.json"),
        )
        .await;
        let accounts = rpc
            .largest_accounts("So11111111111111111111111111111111111111112")
            .await
            .unwrap();
        assert_eq!(accounts.len(), 3);
        assert_eq!(accounts[0].amount, 400_000_000_000_000);

        let bad = r#"{"jsonrpc":"2.0","id":1,"result":{"context":{"slot":1},"value":[{"address":"x","amount":"-5"}]}}"#;
        let (rpc, _) = client(0, bad).await;
        assert!(matches!(
            rpc.largest_accounts("So11111111111111111111111111111111111111112")
                .await,
            Err(MarketDataError::Decode { .. })
        ));
    }

    #[tokio::test]
    async fn cross_check_fails_on_disagreement() {
        let mint = "So11111111111111111111111111111111111111112";
        let spl = crate::token2022::SPL_TOKEN_PROGRAM;
        let revoked = account_info(spl, &base_mint(None, None));
        let active = account_info(spl, &base_mint(Some(1), None));

        let (a, _) = client(0, &revoked).await;
        let (b, _) = client(1, &revoked).await;
        let agreed = CrossCheckedRpc::from_clients(a, b)
            .mint(mint)
            .await
            .unwrap();
        assert_eq!(agreed.get().mint_authority, None);

        let (a, _) = client(0, &revoked).await;
        let (b, _) = client(1, &active).await;
        assert!(matches!(
            CrossCheckedRpc::from_clients(a, b).mint(mint).await,
            Err(MarketDataError::ProviderDisagreement("mint account"))
        ));

        // One provider failing fails the read.
        let (a, _) = client(0, &revoked).await;
        let (b, _) = client(1, "{}").await;
        assert!(CrossCheckedRpc::from_clients(a, b)
            .mint(mint)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cross_check_ignores_tie_order_only() {
        let mint = "So11111111111111111111111111111111111111112";
        let list = |entries: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"result":{{"context":{{"slot":1}},"value":[{entries}]}}}}"#
            )
        };
        let ab = list(r#"{"address":"A","amount":"5"},{"address":"B","amount":"5"}"#);
        let ba = list(r#"{"address":"B","amount":"5"},{"address":"A","amount":"5"}"#);
        let moved = list(r#"{"address":"A","amount":"6"},{"address":"B","amount":"5"}"#);
        let (a, _) = client(0, &ab).await;
        let (b, _) = client(1, &ba).await;
        assert!(CrossCheckedRpc::from_clients(a, b)
            .largest_accounts(mint)
            .await
            .is_ok());
        let (a, _) = client(0, &ab).await;
        let (b, _) = client(1, &moved).await;
        assert!(matches!(
            CrossCheckedRpc::from_clients(a, b)
                .largest_accounts(mint)
                .await,
            Err(MarketDataError::ProviderDisagreement(_))
        ));
    }

    #[test]
    fn endpoints_are_allowlisted_redacted_and_independent() {
        let key = "https://mainnet.helius-rpc.com/?api-key=secret-123";
        let endpoint = RpcEndpoint::new(key.to_owned()).unwrap();
        let shown = format!("{endpoint:?}");
        assert!(!shown.contains("secret-123"), "{shown}");
        assert!(shown.contains("mainnet.helius-rpc.com"));
        let client = RpcClient::new(0, endpoint).unwrap();
        assert!(!format!("{client:?}").contains("secret-123"));

        assert!(matches!(
            RpcEndpoint::new("https://rpc.evil.example/?api-key=x".to_owned()),
            Err(MarketDataError::HostNotAllowed)
        ));
        assert!(matches!(
            RpcEndpoint::new("http://api.mainnet-beta.solana.com".to_owned()),
            Err(MarketDataError::InvalidUrl)
        ));

        let same = |q: &str| {
            RpcEndpoint::new(format!("https://mainnet.helius-rpc.com/?api-key={q}")).unwrap()
        };
        assert!(CrossCheckedRpc::new(same("a"), same("b")).is_err());
        let public = RpcEndpoint::new("https://api.mainnet-beta.solana.com".to_owned()).unwrap();
        assert!(CrossCheckedRpc::new(same("a"), public).is_ok());
    }
}
