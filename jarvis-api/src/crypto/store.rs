//! Typed repository for read-only crypto wallets (schema 0023).
//!
//! Only public addresses: [`NewWallet::parse`] refuses key material first,
//! with a fixed message. Database errors are dropped here, never logged or
//! returned: a refused write may quote the address it was given. All values
//! are bound parameters.

use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_market_data::balances::{Balance, Price};
use jarvis_market_data::wallet::{is_key_material, parse_address, AddressError, Chain, WalletKind};
use jarvis_store::Database;

/// The most wallets the owner may add.
pub const MAX_WALLETS: usize = 20;
/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 40;
/// The chains an EVM wallet may list, in display order.
pub const EVM_CHAINS: [Chain; 5] = [
    Chain::Ethereum,
    Chain::Base,
    Chain::Arbitrum,
    Chain::Optimism,
    Chain::Polygon,
];

const WALLET_FIELDS: &str = "record::id(id) AS id, kind, address, chains, label, created_at, \
     last_ok_at, error, other_tokens";

/// Every message is fixed: none carries the input or a database detail.
#[derive(Debug)]
pub enum CryptoStoreError {
    Address(AddressError),
    Invalid(&'static str),
    Duplicate,
    Full,
    NotFound,
    Database,
    InvalidData,
}

impl std::fmt::Display for CryptoStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Address(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Duplicate => f.write_str("this wallet was already added"),
            Self::Full => f.write_str("wallet limit reached"),
            Self::NotFound => f.write_str("wallet not found"),
            Self::Database => f.write_str("crypto wallet database error"),
            Self::InvalidData => f.write_str("invalid crypto wallet row"),
        }
    }
}

impl std::error::Error for CryptoStoreError {}

impl From<AddressError> for CryptoStoreError {
    fn from(error: AddressError) -> Self {
        Self::Address(error)
    }
}

impl From<surrealdb::Error> for CryptoStoreError {
    fn from(_: surrealdb::Error) -> Self {
        Self::Database
    }
}

type Result<T> = std::result::Result<T, CryptoStoreError>;

/// A stored wallet. `address` is in its stored form (EVM lower case).
#[derive(Debug, Clone, Deserialize)]
pub struct Wallet {
    pub id: String,
    pub kind: WalletKind,
    pub address: String,
    pub chains: Vec<Chain>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// The last refresh that read every chain; `None` before the first.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_ok_at: Option<OffsetDateTime>,
    /// Code of the last failed refresh.
    #[serde(default)]
    pub error: Option<String>,
    /// Solana tokens off the allowlist, counted only.
    #[serde(default)]
    pub other_tokens: Option<u32>,
}

/// A validated wallet to add.
#[derive(Debug, PartialEq, Eq)]
pub struct NewWallet {
    pub kind: WalletKind,
    pub address: String,
    pub chains: Vec<Chain>,
    pub label: Option<String>,
}

impl NewWallet {
    /// Check the owner's input. Key material in the address or the label is
    /// refused before anything else. EVM wallets list their chains (stored
    /// in [`EVM_CHAINS`] order); Bitcoin and Solana wallets name none.
    pub fn parse(
        kind: WalletKind,
        address: &str,
        chains: Option<Vec<Chain>>,
        label: Option<&str>,
    ) -> Result<Self> {
        if label.is_some_and(is_key_material) {
            return Err(AddressError::KeyMaterial.into());
        }
        let address = parse_address(kind, address)?;
        let chains = match (kind, chains) {
            (WalletKind::Bitcoin, None) => vec![Chain::Bitcoin],
            (WalletKind::Solana, None) => vec![Chain::Solana],
            (WalletKind::Evm, Some(chains)) => {
                let fits = chains.iter().all(|chain| EVM_CHAINS.contains(chain))
                    && chains
                        .iter()
                        .enumerate()
                        .all(|(at, chain)| !chains[..at].contains(chain));
                if !fits || chains.is_empty() {
                    return Err(CryptoStoreError::Invalid(
                        "choose one or more EVM chains, each once",
                    ));
                }
                EVM_CHAINS
                    .into_iter()
                    .filter(|chain| chains.contains(chain))
                    .collect()
            }
            (WalletKind::Evm, None) => {
                return Err(CryptoStoreError::Invalid(
                    "choose one or more EVM chains, each once",
                ))
            }
            (_, Some(_)) => {
                return Err(CryptoStoreError::Invalid(
                    "chains are chosen only for EVM wallets",
                ))
            }
        };
        let label = match label.map(str::trim) {
            None => None,
            Some(label)
                if !label.is_empty()
                    && label.chars().count() <= MAX_LABEL_CHARS
                    && crate::validation::no_hidden_chars(label) =>
            {
                Some(label.to_owned())
            }
            Some(_) => {
                return Err(CryptoStoreError::Invalid(
                    "label must be 1 to 40 visible characters",
                ))
            }
        };
        Ok(Self {
            kind,
            address,
            chains,
            label,
        })
    }
}

/// Serialises wallet creation, so the duplicate and limit checks and the
/// write cannot interleave. The transaction repeats the checks behind the
/// guard row (0023) for a caller whose timeout released the lock while its
/// write was still in flight: of two such creates only one commits.
// ponytail: an in-process lock; Core is the only writer of wallets.
static CREATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Every wallet, oldest first.
pub async fn list(db: &Database) -> Result<Vec<Wallet>> {
    Ok(db
        .query(format!(
            "SELECT {WALLET_FIELDS} FROM crypto_wallets ORDER BY created_at, id LIMIT {MAX_WALLETS}"
        ))
        .await?
        .check()?
        .take(0)?)
}

async fn get(db: &Database, id: &str) -> Result<Option<Wallet>> {
    let found: Vec<Wallet> = db
        .query(format!(
            "SELECT {WALLET_FIELDS} FROM crypto_wallets WHERE record::id(id) = $id LIMIT 1"
        ))
        .bind(json!({ "id": id }))
        .await?
        .check()?
        .take(0)?;
    Ok(found.into_iter().next())
}

/// Add a wallet: [`CryptoStoreError::Duplicate`] when its kind and address
/// are stored already, [`CryptoStoreError::Full`] at [`MAX_WALLETS`].
pub async fn create(db: &Database, wallet: &NewWallet) -> Result<Wallet> {
    let _creating = CREATE.lock().await;
    let id = uuid::Uuid::now_v7().to_string();
    let bindings = json!({
        "id": id,
        "kind": wallet.kind,
        "address": wallet.address,
        "chains": wallet.chains,
        "label": wallet.label,
        "max": MAX_WALLETS,
    });
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             UPSERT crypto_wallet_guards:create SET touched_at = time::now() RETURN NONE; \
             IF array::len((SELECT VALUE id FROM crypto_wallets \
               WHERE kind = $kind AND address = $address)) > 0 \
               { THROW 'wallet exists'; }; \
             IF array::len((SELECT VALUE id FROM crypto_wallets)) >= $max \
               { THROW 'no room'; }; \
             CREATE crypto_wallets SET id = $id, kind = $kind, address = $address, \
               chains = $chains, label = $label, created_at = time::now() RETURN NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(bindings.clone())
        .await?;
    if response.check().is_err() {
        // Explain the refusal from the state after it.
        let mut state = db
            .query(
                "SELECT VALUE record::id(id) FROM crypto_wallets \
                   WHERE kind = $kind AND address = $address; \
                 SELECT VALUE record::id(id) FROM crypto_wallets;",
            )
            .bind(bindings)
            .await?
            .check()?;
        let same: Vec<String> = state.take(0)?;
        let all: Vec<String> = state.take(1)?;
        return Err(if !same.is_empty() {
            CryptoStoreError::Duplicate
        } else if all.len() >= MAX_WALLETS {
            CryptoStoreError::Full
        } else {
            CryptoStoreError::Database
        });
    }
    get(db, &id).await?.ok_or(CryptoStoreError::InvalidData)
}

/// Most balance rows: 20 wallets on at most five chains of seven assets.
const MAX_BALANCE_ROWS: usize = 1000;

/// One stored balance of an allowlisted asset.
#[derive(Debug, Clone, Deserialize)]
pub struct StoredBalance {
    pub wallet_id: String,
    pub chain: Chain,
    pub asset: String,
    pub amount: Decimal,
}

/// The last good EUR price of an asset.
#[derive(Debug, Clone, Deserialize)]
pub struct StoredPrice {
    pub asset: String,
    pub eur: Decimal,
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
}

/// The outcome of one wallet's refresh.
#[derive(Debug)]
pub struct Refresh<'a> {
    /// The chains read in full; only their rows are replaced, so a failed
    /// chain keeps its last good balances.
    pub chains: &'a [Chain],
    pub balances: &'a [Balance],
    /// Solana only: mints off the allowlist, counted.
    pub other_tokens: Option<u32>,
    /// The first failure code; `None` when every chain was read.
    pub error: Option<&'static str>,
}

pub async fn balances(db: &Database) -> Result<Vec<StoredBalance>> {
    Ok(db
        .query(format!(
            "SELECT wallet_id, chain, asset, amount FROM crypto_balances \
             ORDER BY wallet_id, chain, asset LIMIT {MAX_BALANCE_ROWS}"
        ))
        .await?
        .check()?
        .take(0)?)
}

pub async fn prices(db: &Database) -> Result<Vec<StoredPrice>> {
    Ok(db
        .query("SELECT record::id(id) AS asset, eur, as_of FROM crypto_prices LIMIT 100")
        .await?
        .check()?
        .take(0)?)
}

fn rfc3339(at: OffsetDateTime) -> Result<String> {
    at.format(&Rfc3339)
        .map_err(|_| CryptoStoreError::InvalidData)
}

/// Replace the prices of the assets in `prices`; others keep their last
/// price, which ages out by its own time.
pub async fn write_prices(db: &Database, prices: &[Price]) -> Result<()> {
    let rows = prices
        .iter()
        .map(|price| {
            Ok(json!({
                "asset": price.asset,
                "eur": price.eur.to_string(),
                "as_of": rfc3339(price.as_of)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let keys: Vec<&str> = prices.iter().map(|price| price.asset).collect();
    db.query(
        "BEGIN TRANSACTION; \
         DELETE crypto_prices WHERE record::id(id) IN $keys RETURN NONE; \
         FOR $p IN $rows { CREATE crypto_prices SET id = $p.asset, eur = $p.eur, \
           as_of = <datetime>$p.as_of RETURN NONE; }; \
         COMMIT TRANSACTION;",
    )
    .bind(json!({ "keys": keys, "rows": rows }))
    .await?
    .check()?;
    Ok(())
}

/// Store one wallet's refresh in one transaction: the rows of the chains
/// read are replaced, then the wallet's status. [`CryptoStoreError::NotFound`]
/// when the wallet was removed meanwhile.
pub async fn write_refresh(db: &Database, wallet_id: &str, refresh: &Refresh<'_>) -> Result<()> {
    let rows: Vec<_> = refresh
        .balances
        .iter()
        .filter(|balance| refresh.chains.contains(&balance.chain))
        .map(|balance| {
            json!({
                "chain": balance.chain,
                "asset": balance.asset,
                "amount": balance.amount.to_string(),
            })
        })
        .collect();
    // Fixed fragments only; every value is bound.
    let status = if refresh.error.is_some() {
        "error = $error"
    } else {
        "error = NONE, last_ok_at = time::now()"
    };
    let other = if refresh.other_tokens.is_some() {
        ", other_tokens = $other_tokens"
    } else {
        ""
    };
    let response = db
        .query(format!(
            "BEGIN TRANSACTION; \
             IF array::len((SELECT VALUE id FROM crypto_wallets WHERE record::id(id) = $id)) != 1 \
               {{ THROW 'wallet unavailable'; }}; \
             DELETE crypto_balances WHERE wallet_id = $id AND chain IN $chains RETURN NONE; \
             FOR $b IN $rows {{ CREATE crypto_balances SET \
               id = string::concat($id, ':', $b.chain, ':', $b.asset), wallet_id = $id, \
               chain = $b.chain, asset = $b.asset, amount = $b.amount, as_of = time::now() \
               RETURN NONE; }}; \
             UPDATE crypto_wallets SET {status}{other} WHERE record::id(id) = $id RETURN NONE; \
             COMMIT TRANSACTION;"
        ))
        .bind(json!({
            "id": wallet_id,
            "chains": refresh.chains,
            "rows": rows,
            "error": refresh.error,
            "other_tokens": refresh.other_tokens,
        }))
        .await?;
    if response.check().is_err() {
        return Err(match get(db, wallet_id).await? {
            None => CryptoStoreError::NotFound,
            Some(_) => CryptoStoreError::Database,
        });
    }
    Ok(())
}

/// Remove a wallet and its balances in one transaction.
pub async fn delete(db: &Database, id: &str) -> Result<Wallet> {
    let wallet = get(db, id).await?.ok_or(CryptoStoreError::NotFound)?;
    let response = db
        .query(
            "BEGIN TRANSACTION; \
             LET $gone = (DELETE crypto_wallets WHERE record::id(id) = $id RETURN BEFORE); \
             IF array::len($gone) != 1 { THROW 'wallet unavailable'; }; \
             DELETE crypto_balances WHERE wallet_id = $id RETURN NONE; \
             COMMIT TRANSACTION;",
        )
        .bind(json!({ "id": id }))
        .await?;
    if response.check().is_err() {
        return Err(match get(db, id).await? {
            None => CryptoStoreError::NotFound,
            Some(_) => CryptoStoreError::Database,
        });
    }
    Ok(wallet)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVM: &str = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
    const BTC: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

    fn invalid(result: Result<NewWallet>) -> String {
        result.unwrap_err().to_string()
    }

    #[test]
    fn evm_wallets_list_their_chains_in_display_order() {
        let wallet = NewWallet::parse(
            WalletKind::Evm,
            EVM,
            Some(vec![Chain::Polygon, Chain::Ethereum]),
            Some("  Cold  "),
        )
        .unwrap();
        assert_eq!(wallet.address, EVM.to_ascii_lowercase());
        assert_eq!(wallet.chains, [Chain::Ethereum, Chain::Polygon]);
        assert_eq!(wallet.label.as_deref(), Some("Cold"));
        for chains in [
            None,
            Some(vec![]),
            Some(vec![Chain::Base, Chain::Base]),
            Some(vec![Chain::Base, Chain::Solana]),
            Some(vec![Chain::Bitcoin]),
        ] {
            assert!(matches!(
                NewWallet::parse(WalletKind::Evm, EVM, chains, None),
                Err(CryptoStoreError::Invalid(_))
            ));
        }
    }

    #[test]
    fn bitcoin_and_solana_wallets_name_no_chains() {
        let wallet = NewWallet::parse(WalletKind::Bitcoin, BTC, None, None).unwrap();
        assert_eq!(wallet.chains, [Chain::Bitcoin]);
        let solana = "So11111111111111111111111111111111111111112";
        let wallet = NewWallet::parse(WalletKind::Solana, solana, None, None).unwrap();
        assert_eq!(wallet.chains, [Chain::Solana]);
        assert!(matches!(
            NewWallet::parse(WalletKind::Bitcoin, BTC, Some(vec![Chain::Bitcoin]), None),
            Err(CryptoStoreError::Invalid(_))
        ));
    }

    #[test]
    fn labels_are_short_and_visible() {
        for label in ["", "   ", "a\u{202E}b", "tab\there"] {
            assert!(matches!(
                NewWallet::parse(WalletKind::Bitcoin, BTC, None, Some(label)),
                Err(CryptoStoreError::Invalid(_))
            ));
        }
        let long = "l".repeat(MAX_LABEL_CHARS + 1);
        assert!(NewWallet::parse(WalletKind::Bitcoin, BTC, None, Some(&long)).is_err());
        let longest = "é".repeat(MAX_LABEL_CHARS);
        assert!(NewWallet::parse(WalletKind::Bitcoin, BTC, None, Some(&longest)).is_ok());
    }

    #[test]
    fn key_material_is_refused_first_and_never_echoed() {
        let phrase =
            "abandon ability able about above absent absorb abstract absurd abuse access accident";
        let hex = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        for (address, label) in [(phrase, None), (EVM, Some(phrase)), (hex, None)] {
            // Even with a wrong chain choice, key material is what is reported.
            let message = invalid(NewWallet::parse(
                WalletKind::Evm,
                address,
                Some(vec![Chain::Bitcoin]),
                label,
            ));
            assert_eq!(message, AddressError::KeyMaterial.to_string());
            assert!(!message.contains("abandon") && !message.contains(&hex[..16]));
        }
    }
}
