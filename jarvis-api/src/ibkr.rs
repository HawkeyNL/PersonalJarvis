//! Read-only IBKR portfolio sync.
//!
//! Core holds no IBKR credentials: it asks the local `jarvis-ibkr-broker`
//! over its peer-uid checked socket for the account, its positions and its
//! ledger, validates every value here, and stores the result as the read
//! model of schema 0015. There is no order, mandate or signer path. The sync
//! is off unless `JARVIS_IBKR_PORTFOLIO_SYNC_ENABLED=true`.

pub mod store;

use std::ffi::CStr;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jarvis_ibkr::broker::{BrokerClient, BrokerError, UPSTREAM_DEADLINE};
use jarvis_ibkr::oauth::{Account, Ledger, Position};
use rust_decimal::Decimal;
use time::OffsetDateTime;

use jarvis_store::Database;
use store::{Holding, Portfolio, Skipped, BASE_CURRENCY, MAX_POSITIONS, MAX_SKIPPED};

/// Between good syncs; below the app's ten-minute staleness mark.
const SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Failed syncs back off by doubling, up to this.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// One broker call (sync and bars). Above the broker's own upstream deadline,
/// which covers IBKR pacing and a session mint, so the broker answers first.
pub(crate) const BROKER_TIMEOUT: Duration = Duration::from_secs(UPSTREAM_DEADLINE.as_secs() + 5);
const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// The broker's service account (`User=` of `jarvis-ibkr-broker.service`).
const BROKER_USER: &CStr = c"jarvis-ibkr";
/// Decimal strings in schema 0015 have at most 20 integer digits.
const MAX_ABS: Decimal = Decimal::from_parts(0x6310_0000, 0x6BC7_5E2D, 0x5, false, 0);

/// Why a sync did not store anything. Fixed texts: no broker or database
/// detail (that is logged where it happens).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    /// The broker socket does not exist: the broker is not enabled.
    BrokerNotRunning,
    Broker(BrokerError),
    Timeout,
    /// The broker answered, but not with something Core accepts.
    Invalid(&'static str),
    Store,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrokerNotRunning => f.write_str("IBKR broker is not running"),
            Self::Broker(error) => write!(f, "{error}"),
            Self::Timeout => f.write_str("IBKR broker call timed out"),
            Self::Invalid(reason) => write!(f, "IBKR data refused: {reason}"),
            Self::Store => f.write_str("IBKR portfolio could not be stored"),
        }
    }
}

impl std::error::Error for SyncError {}

/// The broker client for `socket`, bound to the broker's service uid. Fails
/// when the path is not absolute or the broker account is missing or is
/// Core's own user (then any Core-owned socket would pass the uid check).
pub fn client(socket: &str) -> Result<BrokerClient, &'static str> {
    if !Path::new(socket).is_absolute() {
        return Err("JARVIS_IBKR_BROKER_SOCKET must be an absolute path");
    }
    let (uid, _) = crate::peer::lookup_user(BROKER_USER)
        .ok_or("IBKR broker account jarvis-ibkr is missing")?;
    // SAFETY: geteuid has no preconditions.
    if uid == 0 || uid == unsafe { libc::geteuid() } {
        return Err("IBKR broker account must not be root or Core's own user");
    }
    Ok(BrokerClient::new(socket, uid))
}

/// One broker call with [`BROKER_TIMEOUT`].
async fn bounded<T>(call: impl Future<Output = Result<T, BrokerError>>) -> Result<T, SyncError> {
    match tokio::time::timeout(BROKER_TIMEOUT, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(SyncError::Broker(error)),
        Err(_) => Err(SyncError::Timeout),
    }
}

/// A broker amount Core stores: at most 20 integer digits, trailing zeros
/// dropped, never `-0`.
pub(crate) fn amount(value: Decimal) -> Option<Decimal> {
    (value.abs() < MAX_ABS).then(|| {
        if value.is_zero() {
            Decimal::ZERO
        } else {
            value.normalize()
        }
    })
}

/// Bidi overrides, zero-width and other invisible format characters that
/// could make a symbol look like another one.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

/// Trimmed display text of 1..=`max` characters without control or
/// invisible format characters.
fn text(value: Option<&str>, max: usize) -> Option<String> {
    let value = value?.trim();
    let length = value.chars().count();
    ((1..=max).contains(&length) && !value.chars().any(|c| c.is_control() || is_invisible(c)))
        .then(|| value.to_string())
}

const MAX_CONID: i64 = 999_999_999_999;

fn holding(position: &Position, quantity: Decimal) -> Result<Holding, &'static str> {
    // Same bound as the bars route and the app: 1 to 12 digits.
    if !(1..=MAX_CONID).contains(&position.conid) {
        return Err("invalid contract id");
    }
    let symbol = text(position.ticker.as_deref(), 32)
        .or_else(|| text(position.description.as_deref(), 32))
        .ok_or("position without a valid symbol")?;
    let name = text(position.description.as_deref(), 128).filter(|name| *name != symbol);
    let asset_class = position
        .asset_class
        .as_deref()
        .filter(|class| {
            (1..=8).contains(&class.len()) && class.bytes().all(|b| b.is_ascii_uppercase())
        })
        .ok_or("position without a valid asset class")?
        .to_string();
    let required = |value: Option<Decimal>, what: &'static str| value.and_then(amount).ok_or(what);
    Ok(Holding {
        conid: position.conid,
        symbol,
        name,
        asset_class,
        currency: BASE_CURRENCY.to_string(),
        quantity: amount(quantity).ok_or("invalid quantity")?,
        // avgPrice is per unit like the price; avgCost includes multipliers.
        avg_cost: required(
            position.avg_price.or(position.avg_cost),
            "invalid average cost",
        )?,
        last_price: required(position.market_price, "invalid market price")?,
        market_value: required(position.market_value, "invalid market value")?,
        unrealized_pnl: position
            .unrealized_pnl
            .map(|pnl| amount(pnl).ok_or("invalid unrealized P&L"))
            .transpose()?,
    })
}

/// Validate one broker read into the stored shape. Closed positions
/// (quantity 0) are left out. A valid position in another currency than EUR
/// is left out too and reported in `skipped`: the app reads market values in
/// the base currency, so it would show it with the wrong unit. Anything else
/// that does not validate refuses the whole sync, so the stored portfolio
/// stays the last consistent one.
pub fn map_portfolio(
    positions: &[Position],
    ledger: &Ledger,
    as_of: OffsetDateTime,
) -> Result<Portfolio, &'static str> {
    let base = ledger.get("BASE").ok_or("ledger without BASE")?;
    let net_liquidation = base
        .netliquidationvalue
        .and_then(amount)
        .ok_or("invalid net liquidation")?;
    let cash = base
        .cashbalance
        .and_then(amount)
        .ok_or("invalid cash balance")?;
    let mut holdings = Vec::new();
    let mut skipped = Vec::new();
    let mut conids = Vec::new();
    for position in positions {
        let quantity = position.position.ok_or("position without quantity")?;
        if quantity.is_zero() {
            continue;
        }
        if conids.len() == MAX_POSITIONS {
            return Err("too many positions");
        }
        let holding = holding(position, quantity)?;
        conids.push(holding.conid);
        if position.currency.as_deref() == Some(BASE_CURRENCY) {
            holdings.push(holding);
        } else {
            skipped.push(Skipped {
                symbol: holding.symbol,
                currency: position
                    .currency
                    .as_deref()
                    .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()))
                    .map(str::to_string),
            });
        }
    }
    conids.sort_unstable();
    if conids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("duplicate position");
    }
    store::sort_positions(&mut holdings);
    let skipped_count = skipped.len();
    skipped.sort();
    skipped.truncate(MAX_SKIPPED);
    Ok(Portfolio {
        as_of,
        net_liquidation,
        cash,
        positions: holdings,
        skipped,
        skipped_count,
    })
}

/// The single cash account, which must have the EUR base currency.
fn the_account(accounts: &[Account]) -> Result<&str, &'static str> {
    let [account] = accounts else {
        return Err("expected exactly one IBKR account");
    };
    if account.currency.as_deref() != Some(BASE_CURRENCY) {
        return Err("account base currency is not EUR");
    }
    Ok(&account.account_id)
}

/// Read and validate the account from the broker. `as_of` is the time the
/// last read finished.
pub async fn fetch(client: &BrokerClient) -> Result<Portfolio, SyncError> {
    let accounts = bounded(client.accounts()).await?;
    let account = the_account(&accounts).map_err(SyncError::Invalid)?;
    let positions = bounded(client.positions(account)).await?;
    let ledger = bounded(client.ledger(account)).await?;
    map_portfolio(&positions, &ledger, OffsetDateTime::now_utc()).map_err(SyncError::Invalid)
}

async fn sync_once(
    db: &Database,
    client: &BrokerClient,
    socket: &Path,
) -> Result<Portfolio, SyncError> {
    if !tokio::fs::try_exists(socket).await.unwrap_or(false) {
        return Err(SyncError::BrokerNotRunning);
    }
    let portfolio = fetch(client).await?;
    match tokio::time::timeout(DB_TIMEOUT, store::write_snapshot(db, &portfolio)).await {
        Ok(Ok(())) => Ok(portfolio),
        Ok(Err(error)) => {
            tracing::warn!(%error, "IBKR portfolio write failed");
            Err(SyncError::Store)
        }
        Err(_) => {
            tracing::warn!("IBKR portfolio write timed out");
            Err(SyncError::Store)
        }
    }
}

/// Delay before the next sync after `failures` failed ones in a row.
fn next_delay(failures: u32) -> Duration {
    SYNC_INTERVAL
        .saturating_mul(1 << failures.min(3))
        .min(MAX_BACKOFF)
}

/// Spawn the sync task: one sync right away, then every [`SYNC_INTERVAL`].
/// A missing broker socket is waited for at the normal interval; other
/// failures back off. Each outcome is logged only when it changes.
pub fn start(db: Database, client: BrokerClient, socket: PathBuf) {
    tokio::spawn(async move {
        let mut failures: u32 = 0;
        let mut last: Option<Result<(), SyncError>> = None;
        let mut last_skipped: (usize, Vec<Skipped>) = (0, Vec::new());
        loop {
            let outcome = sync_once(&db, &client, &socket).await;
            let summary = outcome.as_ref().map(|_| ()).map_err(Clone::clone);
            if last.as_ref() != Some(&summary) {
                match &outcome {
                    Ok(portfolio) => tracing::info!(
                        positions = portfolio.positions.len(),
                        "IBKR portfolio synced"
                    ),
                    Err(SyncError::BrokerNotRunning) => {
                        tracing::info!("IBKR broker is not running; portfolio sync waits")
                    }
                    Err(error) => tracing::warn!(%error, "IBKR portfolio sync failed"),
                }
                last = Some(summary);
            }
            if let Ok(portfolio) = &outcome {
                if last_skipped.0 != portfolio.skipped_count || last_skipped.1 != portfolio.skipped
                {
                    if portfolio.skipped_count > 0 {
                        tracing::warn!(
                            skipped = portfolio.skipped_count,
                            "IBKR positions not in EUR are not shown"
                        );
                    } else {
                        tracing::info!("IBKR positions are all in EUR again");
                    }
                    last_skipped = (portfolio.skipped_count, portfolio.skipped.clone());
                }
            }
            failures = match outcome {
                Ok(_) | Err(SyncError::BrokerNotRunning) => 0,
                Err(_) => failures.saturating_add(1),
            };
            tokio::time::sleep(next_delay(failures)).await;
        }
    });
    tracing::info!("IBKR portfolio sync running (read-only)");
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use jarvis_ibkr::oauth::LedgerEntry;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    fn position(json: &str) -> Position {
        serde_json::from_str(json).unwrap()
    }

    const VWCE: &str = r#"{"conid":"12345","contractDesc":"VANGUARD FTSE ALL-WORLD","ticker":"VWCE","assetClass":"STK","currency":"EUR","position":"10","mktPrice":"120.50","mktValue":"1205.00","avgCost":"999","avgPrice":"101.2500","unrealizedPnl":"192.5"}"#;

    fn ledger(json: &str) -> Ledger {
        serde_json::from_str(json).unwrap()
    }

    const LEDGER: &str =
        r#"{"BASE":{"netliquidationvalue":2205.5,"cashbalance":"1000.50"},"EUR":{}}"#;

    #[test]
    fn maps_a_broker_read_to_exact_decimals() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let closed = position(
            r#"{"conid":7,"ticker":"OLD","assetClass":"STK","currency":"EUR","position":0}"#,
        );
        let portfolio = map_portfolio(&[position(VWCE), closed], &ledger(LEDGER), at).unwrap();
        assert_eq!(portfolio.as_of, at);
        assert_eq!(portfolio.net_liquidation.to_string(), "2205.5");
        assert_eq!(portfolio.cash.to_string(), "1000.5");
        assert_eq!(
            portfolio.positions,
            [Holding {
                conid: 12345,
                symbol: "VWCE".into(),
                name: Some("VANGUARD FTSE ALL-WORLD".into()),
                asset_class: "STK".into(),
                currency: "EUR".into(),
                quantity: d("10"),
                avg_cost: d("101.25"),
                last_price: d("120.5"),
                market_value: d("1205"),
                unrealized_pnl: Some(d("192.5")),
            }]
        );
        assert_eq!(portfolio.positions[0].avg_cost.to_string(), "101.25");
        // An empty account is valid.
        assert!(map_portfolio(&[], &ledger(LEDGER), at)
            .unwrap()
            .positions
            .is_empty());
    }

    #[test]
    fn refuses_what_it_cannot_show_truthfully() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let bad = |change: (&str, &str)| {
            let json = VWCE.replace(change.0, change.1);
            map_portfolio(&[position(&json)], &ledger(LEDGER), at).unwrap_err()
        };
        assert_eq!(bad((r#""12345""#, "-1")), "invalid contract id");
        assert_eq!(bad((r#""12345""#, "1000000000000")), "invalid contract id");
        assert_eq!(
            bad((r#""STK""#, r#""stk""#)),
            "position without a valid asset class"
        );
        assert_eq!(
            bad((r#""mktPrice":"120.50""#, r#""mktPrice":null"#)),
            "invalid market price"
        );
        assert_eq!(
            bad((r#""mktValue":"1205.00""#, r#""mktValue":"1e21""#)),
            "invalid market value"
        );
        assert_eq!(
            bad((r#""position":"10""#, r#""position":"x""#)),
            "position without quantity"
        );
        // A bad ticker falls back to the description, which must be short.
        let no_ticker = VWCE.replace(r#""ticker":"VWCE""#, r#""ticker":"\u0007""#);
        let fallback = map_portfolio(&[position(&no_ticker)], &ledger(LEDGER), at).unwrap();
        assert_eq!(fallback.positions[0].symbol, "VANGUARD FTSE ALL-WORLD");
        assert_eq!(fallback.positions[0].name, None);
        let unnamed = no_ticker.replace("VANGUARD FTSE ALL-WORLD", &"X".repeat(33));
        let spoofed = no_ticker.replace("VANGUARD FTSE ALL-WORLD", r"VW\u202eEC");
        assert_eq!(
            map_portfolio(&[position(&spoofed)], &ledger(LEDGER), at).unwrap_err(),
            "position without a valid symbol"
        );
        assert_eq!(
            map_portfolio(&[position(&unnamed)], &ledger(LEDGER), at).unwrap_err(),
            "position without a valid symbol"
        );
        let with_both = [position(VWCE), position(VWCE)];
        assert_eq!(
            map_portfolio(&with_both, &ledger(LEDGER), at).unwrap_err(),
            "duplicate position"
        );
        let many: Vec<Position> = (1..=MAX_POSITIONS as i64 + 1)
            .map(|conid| position(&VWCE.replace(r#""12345""#, &conid.to_string())))
            .collect();
        assert_eq!(
            map_portfolio(&many, &ledger(LEDGER), at).unwrap_err(),
            "too many positions"
        );
        for broken in [r#"{"EUR":{}}"#, r#"{"BASE":{"cashbalance":1}}"#] {
            assert!(map_portfolio(&[], &ledger(broken), at).is_err(), "{broken}");
        }
        assert_eq!(
            map_portfolio(
                &[],
                &Ledger::from([(
                    "BASE".to_string(),
                    LedgerEntry {
                        netliquidationvalue: Some(d("1")),
                        cashbalance: Some(MAX_ABS),
                        settledcash: None,
                        stockmarketvalue: None,
                        unrealizedpnl: None,
                        realizedpnl: None,
                        interest: None,
                    }
                )]),
                at
            )
            .unwrap_err(),
            "invalid cash balance"
        );
    }

    #[test]
    fn skips_positions_not_in_eur_and_reports_them() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let other = |conid: &str, ticker: &str, currency: &str| {
            position(
                &VWCE
                    .replace(r#""12345""#, conid)
                    .replace(r#""VWCE""#, &format!("\"{ticker}\""))
                    .replace(r#","currency":"EUR""#, currency),
            )
        };
        let read = [
            position(VWCE),
            other("2", "AAPL", r#","currency":"USD""#),
            other("3", "NOCUR", ""),
            other("4", "BAD", r#","currency":"us$""#),
        ];
        let portfolio = map_portfolio(&read, &ledger(LEDGER), at).unwrap();
        assert_eq!(portfolio.positions.len(), 1);
        assert_eq!(portfolio.positions[0].symbol, "VWCE");
        assert_eq!(portfolio.skipped_count, 3);
        let skipped = |symbol: &str, currency: Option<&str>| Skipped {
            symbol: symbol.into(),
            currency: currency.map(Into::into),
        };
        assert_eq!(
            portfolio.skipped,
            [
                skipped("AAPL", Some("USD")),
                skipped("BAD", None),
                skipped("NOCUR", None),
            ]
        );
        // A skipped position is still validated, counted against the limit
        // and checked for duplicates.
        let unpriced = position(
            &VWCE
                .replace(r#""EUR""#, r#""USD""#)
                .replace(r#""mktPrice":"120.50""#, r#""mktPrice":null"#),
        );
        assert_eq!(
            map_portfolio(&[unpriced], &ledger(LEDGER), at).unwrap_err(),
            "invalid market price"
        );
        let twice = [
            position(VWCE),
            other("12345", "VWCE", r#","currency":"USD""#),
        ];
        assert_eq!(
            map_portfolio(&twice, &ledger(LEDGER), at).unwrap_err(),
            "duplicate position"
        );
        let many: Vec<Position> = (1..=MAX_POSITIONS as i64 + 1)
            .map(|conid| other(&conid.to_string(), "USD1", r#","currency":"USD""#))
            .collect();
        assert_eq!(
            map_portfolio(&many, &ledger(LEDGER), at).unwrap_err(),
            "too many positions"
        );
        // The reported list is capped; the count is not.
        let all: Vec<Position> = (1..=MAX_POSITIONS as i64)
            .map(|conid| {
                other(
                    &conid.to_string(),
                    &format!("S{conid:04}"),
                    r#","currency":"USD""#,
                )
            })
            .collect();
        let capped = map_portfolio(&all, &ledger(LEDGER), at).unwrap();
        assert!(capped.positions.is_empty());
        assert_eq!(capped.skipped_count, MAX_POSITIONS);
        assert_eq!(capped.skipped.len(), MAX_SKIPPED);
        assert_eq!(capped.skipped[0].symbol, "S0001");
    }

    #[test]
    fn amounts_fit_the_schema() {
        assert_eq!(MAX_ABS, d("100000000000000000000"));
        assert_eq!(amount(d("-0.000")).unwrap().to_string(), "0");
        assert_eq!(amount(d("1.2300")).unwrap().to_string(), "1.23");
        assert_eq!(
            amount(d("-99999999999999999999.5")).unwrap().to_string(),
            "-99999999999999999999.5"
        );
        assert_eq!(amount(MAX_ABS), None);
    }

    #[test]
    fn backs_off_within_bounds() {
        assert_eq!(next_delay(0), SYNC_INTERVAL);
        assert_eq!(next_delay(1), SYNC_INTERVAL * 2);
        assert_eq!(next_delay(2), SYNC_INTERVAL * 4);
        assert_eq!(next_delay(u32::MAX), MAX_BACKOFF);
    }

    fn own_uid() -> u32 {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    /// A fake broker on a temporary socket: answers each request line with
    /// the reply for its `kind`. Returns the socket path; the directory lives
    /// as long as the returned guard.
    async fn fake_broker(replies: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let replies: std::sync::Arc<Vec<(String, String)>> = std::sync::Arc::new(
            replies
                .iter()
                .map(|(kind, reply)| (kind.to_string(), reply.to_string()))
                .collect(),
        );
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let replies = replies.clone();
                tokio::spawn(async move {
                    let (read, mut write) = stream.into_split();
                    let mut line = String::new();
                    if BufReader::new(read).read_line(&mut line).await.is_err() {
                        return;
                    }
                    let request: serde_json::Value =
                        serde_json::from_str(&line).unwrap_or_default();
                    let kind = request["kind"].as_str().unwrap_or_default();
                    let reply = replies
                        .iter()
                        .find(|(k, _)| k == kind)
                        .map_or(r#"{"error":{"code":"invalid_request"}}"#, |(_, r)| {
                            r.as_str()
                        });
                    let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                });
            }
        });
        (dir, socket)
    }

    const FAKE_BROKER: &[(&str, &str)] = &[
        (
            "accounts",
            r#"{"ok":[{"accountId":"U1234567","currency":"EUR"}]}"#,
        ),
        (
            "positions",
            r#"{"ok":[{"conid":12345,"contractDesc":"VANGUARD FTSE ALL-WORLD","ticker":"VWCE","assetClass":"STK","currency":"EUR","position":"10","mktPrice":"120.5","mktValue":"1205","avgPrice":"101.25","unrealizedPnl":"192.5"}]}"#,
        ),
        (
            "ledger",
            r#"{"ok":{"BASE":{"netliquidationvalue":"2205.5","cashbalance":"1000.5"}}}"#,
        ),
    ];

    #[tokio::test]
    async fn fetches_and_validates_through_the_broker_socket() {
        let (_dir, socket) = fake_broker(FAKE_BROKER).await;
        let before = OffsetDateTime::now_utc();
        let portfolio = fetch(&BrokerClient::new(&socket, own_uid())).await.unwrap();
        assert!(portfolio.as_of >= before);
        assert_eq!(portfolio.net_liquidation, d("2205.5"));
        assert_eq!(portfolio.positions.len(), 1);
        assert_eq!(portfolio.positions[0].symbol, "VWCE");

        // A socket served by another uid is refused before anything is sent.
        let error = fetch(&BrokerClient::new(&socket, own_uid().wrapping_add(1)))
            .await
            .unwrap_err();
        assert_eq!(error, SyncError::Broker(BrokerError::Unavailable));
    }

    #[tokio::test]
    async fn refuses_more_than_one_account_or_a_non_eur_base() {
        for (accounts, reason) in [
            (
                r#"{"ok":[{"accountId":"U1","currency":"EUR"},{"accountId":"U2","currency":"EUR"}]}"#,
                "expected exactly one IBKR account",
            ),
            (r#"{"ok":[]}"#, "expected exactly one IBKR account"),
            (
                r#"{"ok":[{"accountId":"U1","currency":"USD"}]}"#,
                "account base currency is not EUR",
            ),
            (
                r#"{"ok":[{"accountId":"U1"}]}"#,
                "account base currency is not EUR",
            ),
        ] {
            let (_dir, socket) = fake_broker(&[("accounts", accounts)]).await;
            let error = fetch(&BrokerClient::new(&socket, own_uid()))
                .await
                .unwrap_err();
            assert_eq!(error, SyncError::Invalid(reason), "{accounts}");
        }
    }

    #[tokio::test]
    async fn a_missing_socket_means_the_broker_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("broker.sock");
        let client = BrokerClient::new(&socket, own_uid());
        // No database call happens before the socket check.
        let db = jarvis_store::Database::init();
        assert_eq!(
            sync_once(&db, &client, &socket).await,
            Err(SyncError::BrokerNotRunning)
        );
    }

    #[test]
    fn client_requires_an_absolute_socket_and_a_foreign_broker_account() {
        assert_eq!(
            client("run/broker.sock").unwrap_err(),
            "JARVIS_IBKR_BROKER_SOCKET must be an absolute path"
        );
        // The jarvis-ibkr account does not exist on CI or dev machines.
        if crate::peer::lookup_user(BROKER_USER).is_none() {
            assert_eq!(
                client("/run/jarvis-ibkr-broker/broker.sock").unwrap_err(),
                "IBKR broker account jarvis-ibkr is missing"
            );
        }
    }
}
