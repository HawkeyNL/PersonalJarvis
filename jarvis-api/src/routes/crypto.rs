//! Read-only crypto wallets of the owner (schema 0023): the list, add and
//! remove. Owner session like every `/v1` route; write bodies are capped by
//! the router at [`BODY_LIMIT`]. Every error text is fixed and never carries
//! the request: a refused body may hold a private key or recovery phrase.
//! Balances come with the refresh (a later change); until a wallet's first
//! good refresh it is `pending` and counts for nothing. Optional fields are
//! absent, never null.

use std::time::Duration;

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        Path, State,
    },
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_market_data::wallet::{evm_checksum, is_key_material, AddressError, Chain, WalletKind};

use crate::crypto::store::{self, CryptoStoreError, NewWallet, Wallet};
use crate::{AppState, Authed};

/// Request bodies of the wallet routes.
pub(crate) const BODY_LIMIT: usize = 4 * 1024;
const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// A wallet whose last good refresh is older than this is `stale`.
const STALE_AFTER: time::Duration = time::Duration::hours(1);
const CHAINS: [Chain; 7] = [
    Chain::Bitcoin,
    Chain::Ethereum,
    Chain::Base,
    Chain::Arbitrum,
    Chain::Optimism,
    Chain::Polygon,
    Chain::Solana,
];

type Failure = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, Failure>;

fn error(status: StatusCode, message: &str) -> Failure {
    (status, Json(json!({ "error": message })))
}

fn failure(refused: CryptoStoreError) -> Failure {
    match refused {
        CryptoStoreError::Address(_) | CryptoStoreError::Invalid(_) => {
            error(StatusCode::BAD_REQUEST, &refused.to_string())
        }
        CryptoStoreError::Duplicate | CryptoStoreError::Full => {
            error(StatusCode::CONFLICT, &refused.to_string())
        }
        CryptoStoreError::NotFound => error(StatusCode::NOT_FOUND, "not found"),
        CryptoStoreError::Database | CryptoStoreError::InvalidData => {
            // Fixed text: the store keeps no database detail.
            tracing::warn!(error = %refused, "crypto wallet operation failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, CryptoStoreError>>,
) -> Result<T, Failure> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(result) => result.map_err(failure),
        Err(_) => {
            tracing::warn!("crypto wallet operation timed out");
            Err(error(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
        }
    }
}

fn ts(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

fn kind_name(kind: WalletKind) -> &'static str {
    match kind {
        WalletKind::Bitcoin => "bitcoin",
        WalletKind::Evm => "evm",
        WalletKind::Solana => "solana",
    }
}

/// One audit row per change: the device, the wallet id and kind, never the
/// address or label.
async fn audit(state: &AppState, authed: &Authed, event: &str, wallet: &Wallet) {
    let detail = format!("{} {}", wallet.id, kind_name(wallet.kind));
    crate::audit::record_security_event(state, Some(authed.device.id), event, "ok", Some(&detail))
        .await;
}

fn status(wallet: &Wallet, now: OffsetDateTime) -> &'static str {
    match wallet.last_ok_at {
        _ if wallet.error.is_some() => "error",
        None => "pending",
        Some(at) if now - at > STALE_AFTER => "stale",
        Some(_) => "ok",
    }
}

/// A wallet of the app contract; EVM addresses in their EIP-55 form.
pub(crate) fn wallet_body(wallet: &Wallet, now: OffsetDateTime) -> Value {
    let address = match wallet.kind {
        WalletKind::Evm => evm_checksum(&wallet.address),
        WalletKind::Bitcoin | WalletKind::Solana => wallet.address.clone(),
    };
    let mut body = Map::new();
    body.insert("id".into(), wallet.id.clone().into());
    body.insert("kind".into(), kind_name(wallet.kind).into());
    body.insert("chains".into(), json!(wallet.chains));
    body.insert("address".into(), address.into());
    if let Some(label) = &wallet.label {
        body.insert("label".into(), label.clone().into());
    }
    body.insert("status".into(), status(wallet, now).into());
    if let Some(code) = &wallet.error {
        body.insert("error".into(), code.clone().into());
    }
    if let Some(at) = wallet.last_ok_at {
        body.insert("last_refresh_at".into(), ts(at).into());
    }
    if let Some(count) = wallet.other_tokens {
        body.insert("other_tokens".into(), count.into());
    }
    body.insert("value_eur".into(), "0".into());
    body.insert("balances".into(), json!([]));
    Value::Object(body)
}

/// `GET /v1/trading/crypto`. `as_of` is the oldest good refresh, or now
/// while no wallet has one.
pub(crate) fn overview_body(wallets: &[Wallet], now: OffsetDateTime) -> Value {
    let as_of = wallets
        .iter()
        .filter_map(|wallet| wallet.last_ok_at)
        .min()
        .unwrap_or(now);
    json!({
        "source": "crypto",
        "as_of": ts(as_of),
        "base_currency": "EUR",
        "total_eur": "0",
        "chains_available": CHAINS,
        "wallets": wallets.iter().map(|wallet| wallet_body(wallet, now)).collect::<Vec<_>>(),
        "assets": [],
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateWallet {
    kind: WalletKind,
    address: String,
    #[serde(default)]
    chains: Option<Vec<Chain>>,
    #[serde(default)]
    label: Option<String>,
}

/// A body axum could not read, as a fixed text: its own message may quote
/// the body.
fn unreadable(rejection: JsonRejection) -> Failure {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
    } else {
        error(StatusCode::BAD_REQUEST, "invalid request body")
    }
}

/// Whether any key or string in the body looks like key material, or an
/// array of 32+ numbers does (a Solana keypair file's bytes).
fn holds_key_material(value: &Value) -> bool {
    match value {
        Value::String(text) => is_key_material(text),
        Value::Array(items) => {
            (items.len() >= 32 && items.iter().all(Value::is_u64))
                || items.iter().any(holds_key_material)
        }
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| is_key_material(key) || holds_key_material(value)),
        _ => false,
    }
}

pub(crate) async fn overview(_authed: Authed, State(state): State<AppState>) -> ApiResult {
    let wallets = bounded(store::list(&state.db)).await?;
    Ok(Json(overview_body(&wallets, OffsetDateTime::now_utc())))
}

pub(crate) async fn create(
    authed: Authed,
    State(state): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let Json(body) = body.map_err(unreadable)?;
    // Key material first, wherever it is in the body, before any other check.
    if holds_key_material(&body) {
        return Err(failure(AddressError::KeyMaterial.into()));
    }
    let input: CreateWallet = serde_json::from_value(body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "invalid request body"))?;
    let wallet = NewWallet::parse(
        input.kind,
        &input.address,
        input.chains,
        input.label.as_deref(),
    )
    .map_err(failure)?;
    let created = bounded(store::create(&state.db, &wallet)).await?;
    audit(&state, &authed, "crypto.wallet_added", &created).await;
    Ok((
        StatusCode::CREATED,
        Json(wallet_body(&created, OffsetDateTime::now_utc())),
    ))
}

/// Remove the wallet and its balances.
pub(crate) async fn delete(
    authed: Authed,
    State(state): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> ApiResult {
    let Path(id) = id.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid wallet id"))?;
    let gone = bounded(store::delete(&state.db, &id.to_string())).await?;
    audit(&state, &authed, "crypto.wallet_removed", &gone).await;
    Ok(Json(json!({ "deleted": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(kind: WalletKind, address: &str) -> Wallet {
        Wallet {
            id: "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".into(),
            kind,
            address: address.into(),
            chains: vec![Chain::Ethereum, Chain::Base],
            label: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            last_ok_at: None,
            error: None,
            other_tokens: None,
        }
    }

    #[test]
    fn key_material_is_found_anywhere_in_a_body() {
        let hex = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        for body in [
            json!({"label": {"nested": [hex]}}),
            json!({ hex: "key name" }),
            json!({"address": vec![7; 64]}),
        ] {
            assert!(holds_key_material(&body), "{body}");
        }
        let fine = json!({"kind": "evm", "address": "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed", "chains": ["base"], "label": "Cold"});
        assert!(!holds_key_material(&fine));
    }

    #[test]
    fn bodies_match_the_app_contract() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let evm = wallet(
            WalletKind::Evm,
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        );
        let body = wallet_body(&evm, now);
        assert_eq!(
            body,
            json!({
                "id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
                "kind": "evm",
                "chains": ["ethereum", "base"],
                "address": "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
                "status": "pending",
                "value_eur": "0",
                "balances": [],
            })
        );
        let mut refreshed = wallet(
            WalletKind::Solana,
            "So11111111111111111111111111111111111111112",
        );
        refreshed.label = Some("Phantom".into());
        refreshed.last_ok_at = Some(now - time::Duration::minutes(5));
        refreshed.other_tokens = Some(2);
        let body = wallet_body(&refreshed, now);
        assert_eq!(
            (&body["status"], &body["label"], &body["other_tokens"]),
            (&json!("ok"), &json!("Phantom"), &json!(2))
        );
        assert_eq!(body["last_refresh_at"], "1970-01-01T23:55:00Z");
        refreshed.last_ok_at = Some(now - time::Duration::hours(2));
        assert_eq!(wallet_body(&refreshed, now)["status"], "stale");
        refreshed.error = Some("rate_limited".into());
        let failed = wallet_body(&refreshed, now);
        assert_eq!(
            (&failed["status"], &failed["error"]),
            (&json!("error"), &json!("rate_limited"))
        );

        let overview = overview_body(&[evm, refreshed], now);
        assert_eq!(overview["as_of"], "1970-01-01T22:00:00Z");
        assert_eq!(overview["source"], "crypto");
        assert_eq!(
            overview["chains_available"].as_array().map(Vec::len),
            Some(7)
        );
        assert_eq!(overview_body(&[], now)["as_of"], "1970-01-02T00:00:00Z");
    }
}
