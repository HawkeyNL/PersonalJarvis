//! Read-only crypto wallets of the owner (schema 0023): the list with EUR
//! values, add, remove and refresh. Owner session like every `/v1` route;
//! write bodies are capped by the router at [`BODY_LIMIT`]. Every error text
//! is fixed and never carries the request: a refused body may hold a private
//! key or recovery phrase. All routes answer 503 while the feature is off.
//! Until a wallet's first good refresh it is `pending` and counts for
//! nothing. A balance counts only with a price younger than an hour; stale
//! balances still count. Optional fields are absent, never null.

use std::time::Duration;

use axum::{
    extract::{
        rejection::{JsonRejection, PathRejection},
        Path, State,
    },
    http::StatusCode,
    Json,
};
use jarvis_client_core::realtime::Event;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_market_data::balances::PRICE_MAX_AGE;
use jarvis_market_data::wallet::{
    evm_checksum, is_key_material, AddressError, Asset, Chain, WalletKind, ASSETS,
};

use crate::crypto::store::{self, CryptoStoreError, NewWallet, StoredBalance, StoredPrice, Wallet};
use crate::{AppState, Authed};

/// Request bodies of the wallet routes.
pub(crate) const BODY_LIMIT: usize = 4 * 1024;
const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// A wallet whose last good refresh is older than this is `stale`.
const STALE_AFTER: time::Duration = time::Duration::hours(1);
/// EUR values have at most 20 integer digits, like the stored amounts.
const MAX_EUR: Decimal = Decimal::from_parts(0x6310_0000, 0x6BC7_5E2D, 0x5, false, 0);
/// Display order of chains.
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

/// Every route answers this while the feature is off.
fn require_enabled(state: &AppState) -> Result<(), Failure> {
    if state.crypto.enabled() {
        Ok(())
    } else {
        Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "crypto wallets are not enabled",
        ))
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

/// A sum or product beyond [`MAX_EUR`].
#[derive(Debug)]
pub(crate) struct OutOfRange;

fn add(a: Decimal, b: Decimal) -> Result<Decimal, OutOfRange> {
    a.checked_add(b)
        .filter(|sum| *sum < MAX_EUR)
        .ok_or(OutOfRange)
}

fn eur(value: Decimal) -> String {
    value.round_dp(2).normalize().to_string()
}

/// One stored balance with its EUR price and value, when priced.
struct Line {
    chain: Chain,
    asset: &'static Asset,
    amount: Decimal,
    price: Option<Decimal>,
    value: Option<Decimal>,
}

/// The price of `asset` if it is younger than [`PRICE_MAX_AGE`].
fn price_of(prices: &[StoredPrice], asset: &str, now: OffsetDateTime) -> Option<Decimal> {
    prices
        .iter()
        .find(|price| price.asset == asset && now - price.as_of <= PRICE_MAX_AGE)
        .map(|price| price.eur)
}

/// The balances of `wallet_id` on the allowlist, in display order.
fn lines(
    wallet_id: &str,
    balances: &[StoredBalance],
    prices: &[StoredPrice],
    now: OffsetDateTime,
) -> Result<Vec<Line>, OutOfRange> {
    let mut lines = Vec::new();
    for balance in balances.iter().filter(|b| b.wallet_id == wallet_id) {
        let Some(asset) = ASSETS.iter().find(|a| a.key == balance.asset) else {
            continue;
        };
        let price = price_of(prices, asset.key, now);
        let value = price
            .map(|price| {
                balance
                    .amount
                    .checked_mul(price)
                    .filter(|value| *value < MAX_EUR)
                    .ok_or(OutOfRange)
            })
            .transpose()?;
        lines.push(Line {
            chain: balance.chain,
            asset,
            amount: balance.amount,
            price,
            value,
        });
    }
    lines.sort_by_key(|line| {
        (
            CHAINS.iter().position(|chain| *chain == line.chain),
            ASSETS.iter().position(|a| a.key == line.asset.key),
        )
    });
    Ok(lines)
}

fn priced(body: &mut Map<String, Value>, price: Option<Decimal>, value: Option<Decimal>) {
    if let Some(price) = price {
        body.insert("price_eur".into(), price.normalize().to_string().into());
    }
    if let Some(value) = value {
        body.insert("value_eur".into(), eur(value).into());
    }
}

/// A wallet of the app contract; EVM addresses in their EIP-55 form.
fn wallet_body(wallet: &Wallet, lines: &[Line], now: OffsetDateTime) -> Result<Value, OutOfRange> {
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
    let mut value = Decimal::ZERO;
    let mut balances = Vec::new();
    for line in lines {
        value = add(value, line.value.unwrap_or_default())?;
        let mut row = Map::new();
        row.insert("chain".into(), json!(line.chain));
        row.insert("asset".into(), line.asset.key.into());
        row.insert("symbol".into(), line.asset.symbol.into());
        row.insert("amount".into(), line.amount.normalize().to_string().into());
        priced(&mut row, line.price, line.value);
        balances.push(Value::Object(row));
    }
    body.insert("value_eur".into(), eur(value).into());
    body.insert("balances".into(), balances.into());
    Ok(Value::Object(body))
}

/// `GET /v1/trading/crypto`. `as_of` is the oldest good refresh, or now
/// while no wallet has one.
pub(crate) fn overview_body(
    wallets: &[Wallet],
    balances: &[StoredBalance],
    prices: &[StoredPrice],
    chains_available: &[Chain],
    now: OffsetDateTime,
) -> Result<Value, OutOfRange> {
    let as_of = wallets
        .iter()
        .filter_map(|wallet| wallet.last_ok_at)
        .min()
        .unwrap_or(now);
    let mut total = Decimal::ZERO;
    let mut bodies = Vec::new();
    // Per asset: amount and value over every wallet.
    let mut assets: Vec<(&'static Asset, Decimal, Option<Decimal>)> = Vec::new();
    for wallet in wallets {
        let lines = lines(&wallet.id, balances, prices, now)?;
        for line in &lines {
            total = add(total, line.value.unwrap_or_default())?;
            match assets
                .iter_mut()
                .find(|(asset, ..)| asset.key == line.asset.key)
            {
                Some((_, amount, value)) => {
                    *amount = add(*amount, line.amount)?;
                    *value = match (*value, line.value) {
                        (Some(a), Some(b)) => Some(add(a, b)?),
                        (a, b) => a.or(b),
                    };
                }
                None => assets.push((line.asset, line.amount, line.value)),
            }
        }
        bodies.push(wallet_body(wallet, &lines, now)?);
    }
    // Largest value first, unpriced last, then allowlist order.
    assets.sort_by_key(|(asset, _, value)| {
        (
            std::cmp::Reverse(*value),
            ASSETS.iter().position(|a| a.key == asset.key),
        )
    });
    let assets: Vec<Value> = assets
        .into_iter()
        .map(|(asset, amount, value)| {
            let mut row = Map::new();
            row.insert("asset".into(), asset.key.into());
            row.insert("symbol".into(), asset.symbol.into());
            row.insert("amount".into(), amount.normalize().to_string().into());
            priced(&mut row, price_of(prices, asset.key, now), value);
            if let Some(weight) = value
                .and_then(|value| value.checked_mul(Decimal::ONE_HUNDRED))
                .and_then(|value| value.checked_div(total))
            {
                row.insert("weight_pct".into(), eur(weight).into());
            }
            Value::Object(row)
        })
        .collect();
    let chains_available: Vec<Chain> = CHAINS
        .into_iter()
        .filter(|chain| chains_available.contains(chain))
        .collect();
    Ok(json!({
        "source": "crypto",
        "as_of": ts(as_of),
        "base_currency": "EUR",
        "total_eur": eur(total),
        "chains_available": chains_available,
        "wallets": bodies,
        "assets": assets,
    }))
}

fn out_of_range(_: OutOfRange) -> Failure {
    tracing::warn!("crypto wallet value out of range");
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "crypto value out of range",
    )
}

/// Tell the apps the wallets changed.
fn changed(state: &AppState) {
    state.realtime.publish_all(Event::CryptoUpdated {
        as_of: OffsetDateTime::now_utc(),
    });
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
    require_enabled(&state)?;
    let wallets = bounded(store::list(&state.db)).await?;
    let balances = bounded(store::balances(&state.db)).await?;
    let prices = bounded(store::prices(&state.db)).await?;
    overview_body(
        &wallets,
        &balances,
        &prices,
        state.crypto.chains(),
        OffsetDateTime::now_utc(),
    )
    .map(Json)
    .map_err(out_of_range)
}

/// `POST /v1/trading/crypto/refresh`: 202, or 429 within the cooldown.
pub(crate) async fn refresh(
    _authed: Authed,
    State(state): State<AppState>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    require_enabled(&state)?;
    match state.crypto.request_refresh() {
        Ok(()) => Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true })))),
        Err(retry_after_secs) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "a refresh was asked for less than a minute ago",
                "retry_after_secs": retry_after_secs,
            })),
        )),
    }
}

pub(crate) async fn create(
    authed: Authed,
    State(state): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    require_enabled(&state)?;
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
    if !wallet
        .chains
        .iter()
        .all(|chain| state.crypto.chains().contains(chain))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "a chosen chain is not available on this Core",
        ));
    }
    let created = bounded(store::create(&state.db, &wallet)).await?;
    audit(&state, &authed, "crypto.wallet_added", &created).await;
    // Read it right away, or as soon as the cooldown allows.
    state.crypto.refresh_soon();
    changed(&state);
    let body = wallet_body(&created, &[], OffsetDateTime::now_utc()).map_err(out_of_range)?;
    Ok((StatusCode::CREATED, Json(body)))
}

/// Remove the wallet and its balances.
pub(crate) async fn delete(
    authed: Authed,
    State(state): State<AppState>,
    id: Result<Path<Uuid>, PathRejection>,
) -> ApiResult {
    require_enabled(&state)?;
    let Path(id) = id.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid wallet id"))?;
    let gone = bounded(store::delete(&state.db, &id.to_string())).await?;
    audit(&state, &authed, "crypto.wallet_removed", &gone).await;
    changed(&state);
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

    fn balance(wallet_id: &str, chain: Chain, asset: &str, amount: &str) -> StoredBalance {
        StoredBalance {
            wallet_id: wallet_id.into(),
            chain,
            asset: asset.into(),
            amount: amount.parse().unwrap(),
        }
    }

    fn price(asset: &str, eur: &str, as_of: OffsetDateTime) -> StoredPrice {
        StoredPrice {
            asset: asset.into(),
            eur: eur.parse().unwrap(),
            as_of,
        }
    }

    #[test]
    fn bodies_match_the_app_contract() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let evm = wallet(
            WalletKind::Evm,
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        );
        let body = wallet_body(&evm, &[], now).unwrap();
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
        refreshed.id = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c".into();
        refreshed.label = Some("Phantom".into());
        refreshed.last_ok_at = Some(now - time::Duration::minutes(5));
        refreshed.other_tokens = Some(2);
        let body = wallet_body(&refreshed, &[], now).unwrap();
        assert_eq!(
            (&body["status"], &body["label"], &body["other_tokens"]),
            (&json!("ok"), &json!("Phantom"), &json!(2))
        );
        assert_eq!(body["last_refresh_at"], "1970-01-01T23:55:00Z");
        refreshed.last_ok_at = Some(now - time::Duration::hours(2));
        assert_eq!(
            wallet_body(&refreshed, &[], now).unwrap()["status"],
            "stale"
        );
        refreshed.error = Some("rate_limited".into());
        let failed = wallet_body(&refreshed, &[], now).unwrap();
        assert_eq!(
            (&failed["status"], &failed["error"]),
            (&json!("error"), &json!("rate_limited"))
        );

        let all = CHAINS.to_vec();
        let overview = overview_body(&[evm, refreshed], &[], &[], &all, now).unwrap();
        assert_eq!(overview["as_of"], "1970-01-01T22:00:00Z");
        assert_eq!(overview["source"], "crypto");
        assert_eq!(
            overview["chains_available"].as_array().map(Vec::len),
            Some(7)
        );
        let empty = overview_body(&[], &[], &[], &[Chain::Solana, Chain::Bitcoin], now).unwrap();
        assert_eq!(empty["as_of"], "1970-01-02T00:00:00Z");
        assert_eq!(empty["chains_available"], json!(["bitcoin", "solana"]));
    }

    #[test]
    fn values_count_only_fresh_prices_and_stale_balances() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let mut evm = wallet(
            WalletKind::Evm,
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        );
        // Stale balances still count.
        evm.last_ok_at = Some(now - time::Duration::hours(3));
        let id = evm.id.clone();
        let balances = [
            balance(&id, Chain::Base, "usdc", "100"),
            balance(&id, Chain::Ethereum, "eth", "0.5"),
            balance(&id, Chain::Base, "eth", "1.5"),
            balance(&id, Chain::Base, "dai", "7"),
            balance(&id, Chain::Base, "scam", "1000"),
            balance("other-wallet", Chain::Base, "eth", "9"),
        ];
        let prices = [
            price("eth", "2000", now - time::Duration::minutes(10)),
            price("usdc", "0.9", now - time::Duration::minutes(59)),
            // Too old: shown without a price, not counted.
            price("dai", "0.9", now - time::Duration::minutes(61)),
        ];
        let body = overview_body(&[evm], &balances, &prices, &CHAINS, now).unwrap();
        assert_eq!(body["total_eur"], "4090");
        let wallet = &body["wallets"][0];
        assert_eq!(wallet["value_eur"], "4090");
        assert_eq!(
            wallet["balances"],
            json!([
                {"chain": "ethereum", "asset": "eth", "symbol": "ETH", "amount": "0.5", "price_eur": "2000", "value_eur": "1000"},
                {"chain": "base", "asset": "eth", "symbol": "ETH", "amount": "1.5", "price_eur": "2000", "value_eur": "3000"},
                {"chain": "base", "asset": "usdc", "symbol": "USDC", "amount": "100", "price_eur": "0.9", "value_eur": "90"},
                {"chain": "base", "asset": "dai", "symbol": "DAI", "amount": "7"},
            ])
        );
        assert_eq!(
            body["assets"],
            json!([
                {"asset": "eth", "symbol": "ETH", "amount": "2", "price_eur": "2000", "value_eur": "4000", "weight_pct": "97.8"},
                {"asset": "usdc", "symbol": "USDC", "amount": "100", "price_eur": "0.9", "value_eur": "90", "weight_pct": "2.2"},
                {"asset": "dai", "symbol": "DAI", "amount": "7"},
            ])
        );
        let text = body.to_string();
        assert!(!text.contains("scam") && !text.contains("other-wallet"));
    }

    #[test]
    fn values_beyond_twenty_digits_are_refused() {
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let evm = wallet(
            WalletKind::Evm,
            "0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed",
        );
        let huge = "99999999999999999999";
        let balances = [
            balance(&evm.id, Chain::Base, "eth", huge),
            balance(&evm.id, Chain::Ethereum, "eth", huge),
        ];
        let prices = [price("eth", "1", now)];
        assert!(overview_body(&[evm], &balances, &prices, &CHAINS, now).is_err());
    }
}
