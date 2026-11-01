//! Typed repository for the IBKR portfolio read model (schema 0015).
//!
//! A read-only mirror of what the broker reported at the last good sync. No
//! row here is an order, a mandate or a limit. Amounts cross this boundary
//! as `Decimal` and are stored as strings.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use jarvis_store::Database;

/// The only base currency: a EUR cash account.
pub const BASE_CURRENCY: &str = "EUR";
/// The most positions one sync may store (the app's limit as well).
pub const MAX_POSITIONS: usize = 1000;
/// The most skipped positions one sync reports by name.
pub const MAX_SKIPPED: usize = 50;
/// Daily history points are kept this long.
pub const HISTORY_DAYS: u32 = 3650;
/// The most history points one read returns.
pub const MAX_HISTORY_POINTS: usize = 5000;

/// Database detail is logged by the caller, never returned to clients.
#[derive(Debug)]
pub enum IbkrStoreError {
    Database(Box<surrealdb::Error>),
    InvalidData,
}

impl std::fmt::Display for IbkrStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(f, "IBKR portfolio database error: {error}"),
            Self::InvalidData => f.write_str("invalid IBKR portfolio row"),
        }
    }
}

impl std::error::Error for IbkrStoreError {}

impl From<surrealdb::Error> for IbkrStoreError {
    fn from(error: surrealdb::Error) -> Self {
        Self::Database(Box::new(error))
    }
}

type Result<T> = std::result::Result<T, IbkrStoreError>;

/// One position as validated by the sync. Prices and values are in the
/// position's currency, which the sync only accepts as [`BASE_CURRENCY`].
#[derive(Debug, Clone, PartialEq)]
pub struct Holding {
    pub conid: i64,
    pub symbol: String,
    pub name: Option<String>,
    pub asset_class: String,
    pub currency: String,
    pub quantity: Decimal,
    pub avg_cost: Decimal,
    pub last_price: Decimal,
    pub market_value: Decimal,
    pub unrealized_pnl: Option<Decimal>,
}

/// A valid position the sync left out because it is not in
/// [`BASE_CURRENCY`]. `currency` is absent when the broker reported none or
/// no three-letter code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Skipped {
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// The account at one good sync. Positions are sorted by market value,
/// largest first. `skipped` holds at most [`MAX_SKIPPED`] of the
/// `skipped_count` positions left out, sorted by symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Portfolio {
    pub as_of: OffsetDateTime,
    pub net_liquidation: Decimal,
    pub cash: Decimal,
    pub positions: Vec<Holding>,
    pub skipped: Vec<Skipped>,
    pub skipped_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub t: OffsetDateTime,
    pub value: Decimal,
}

/// What the bars route needs to label a held instrument.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Instrument {
    pub symbol: String,
    #[serde(default)]
    pub name: Option<String>,
    pub currency: String,
}

fn rfc3339(at: OffsetDateTime) -> Result<String> {
    at.format(&Rfc3339).map_err(|_| IbkrStoreError::InvalidData)
}

fn decimal(value: &str) -> Result<Decimal> {
    Decimal::from_str(value).map_err(|_| IbkrStoreError::InvalidData)
}

fn position_row(holding: &Holding) -> Value {
    let mut row = Map::new();
    row.insert("id".into(), holding.conid.to_string().into());
    row.insert("conid".into(), holding.conid.into());
    row.insert("symbol".into(), holding.symbol.clone().into());
    row.insert("asset_class".into(), holding.asset_class.clone().into());
    row.insert("currency".into(), holding.currency.clone().into());
    row.insert("quantity".into(), holding.quantity.to_string().into());
    row.insert("avg_cost".into(), holding.avg_cost.to_string().into());
    row.insert("last_price".into(), holding.last_price.to_string().into());
    row.insert(
        "market_value".into(),
        holding.market_value.to_string().into(),
    );
    // Absent rather than null: the optional fields are `option<string>`.
    if let Some(name) = &holding.name {
        row.insert("name".into(), name.clone().into());
    }
    if let Some(pnl) = holding.unrealized_pnl {
        row.insert("unrealized_pnl".into(), pnl.to_string().into());
    }
    Value::Object(row)
}

/// Replace the stored portfolio with `portfolio` and record its net
/// liquidation as the point of its UTC day, in one transaction. History
/// older than [`HISTORY_DAYS`] is pruned on the way.
pub async fn write_snapshot(db: &Database, portfolio: &Portfolio) -> Result<()> {
    if portfolio.positions.len() > MAX_POSITIONS || !skipped_fits(portfolio) {
        return Err(IbkrStoreError::InvalidData);
    }
    let day = portfolio.as_of.date().to_string();
    db.query(format!(
        "BEGIN TRANSACTION; \
         DELETE ibkr_positions RETURN NONE; \
         FOR $p IN $positions {{ CREATE ibkr_positions CONTENT $p RETURN NONE; }}; \
         UPSERT ibkr_portfolio:current SET id = 'current', as_of = <datetime>$as_of, \
         base_currency = $base, net_liquidation = $net_liquidation, cash = $cash, \
         skipped_count = $skipped_count, skipped_positions = $skipped RETURN NONE; \
         UPSERT $day SET id = $day_key, t = <datetime>$as_of, value = $net_liquidation RETURN NONE; \
         DELETE ibkr_portfolio_history WHERE t < time::now() - {HISTORY_DAYS}d RETURN NONE; \
         COMMIT TRANSACTION;"
    ))
    .bind(json!({
        "positions": portfolio.positions.iter().map(position_row).collect::<Vec<_>>(),
        "as_of": rfc3339(portfolio.as_of)?,
        "base": BASE_CURRENCY,
        "net_liquidation": portfolio.net_liquidation.to_string(),
        "cash": portfolio.cash.to_string(),
        "skipped_count": portfolio.skipped_count,
        "skipped": portfolio.skipped,
        "day_key": day,
    }))
    .bind((
        "day",
        surrealdb::RecordId::from_table_key("ibkr_portfolio_history", day),
    ))
    .await?
    .check()?;
    Ok(())
}

#[derive(Deserialize)]
struct PositionRow {
    conid: i64,
    symbol: String,
    #[serde(default)]
    name: Option<String>,
    asset_class: String,
    currency: String,
    quantity: String,
    avg_cost: String,
    last_price: String,
    market_value: String,
    #[serde(default)]
    unrealized_pnl: Option<String>,
}

#[derive(Deserialize)]
struct PortfolioRow {
    #[serde(with = "time::serde::rfc3339")]
    as_of: OffsetDateTime,
    base_currency: String,
    net_liquidation: String,
    cash: String,
    positions: Vec<PositionRow>,
    skipped_count: usize,
    skipped_positions: Vec<Skipped>,
}

/// At most [`MAX_SKIPPED`] named, never more than counted, and no more
/// positions in all than one sync may hold.
fn skipped_fits(portfolio: &Portfolio) -> bool {
    portfolio.skipped.len() <= MAX_SKIPPED.min(portfolio.skipped_count)
        && portfolio.positions.len() + portfolio.skipped_count <= MAX_POSITIONS
}

/// The last good sync, or `None` before the first one. One statement, so
/// the summary and its positions are read from the same snapshot.
pub async fn load_portfolio(db: &Database) -> Result<Option<Portfolio>> {
    let rows: Vec<PortfolioRow> = db
        .query(format!(
            "SELECT as_of, base_currency, net_liquidation, cash, skipped_count, skipped_positions, \
             (SELECT conid, symbol, name, asset_class, currency, quantity, avg_cost, \
             last_price, market_value, unrealized_pnl FROM ibkr_positions LIMIT {}) AS positions \
             FROM ibkr_portfolio:current",
            MAX_POSITIONS + 1
        ))
        .await?
        .check()?
        .take(0)?;
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    if row.base_currency != BASE_CURRENCY || row.positions.len() > MAX_POSITIONS {
        return Err(IbkrStoreError::InvalidData);
    }
    let mut positions = row
        .positions
        .into_iter()
        .map(|p| {
            Ok(Holding {
                conid: p.conid,
                symbol: p.symbol,
                name: p.name,
                asset_class: p.asset_class,
                currency: p.currency,
                quantity: decimal(&p.quantity)?,
                avg_cost: decimal(&p.avg_cost)?,
                last_price: decimal(&p.last_price)?,
                market_value: decimal(&p.market_value)?,
                unrealized_pnl: p.unrealized_pnl.as_deref().map(decimal).transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    sort_positions(&mut positions);
    let portfolio = Portfolio {
        as_of: row.as_of,
        net_liquidation: decimal(&row.net_liquidation)?,
        cash: decimal(&row.cash)?,
        positions,
        skipped: row.skipped_positions,
        skipped_count: row.skipped_count,
    };
    if !skipped_fits(&portfolio) {
        return Err(IbkrStoreError::InvalidData);
    }
    Ok(Some(portfolio))
}

/// Largest market value first, then by contract id for a stable order.
pub fn sort_positions(positions: &mut [Holding]) {
    positions.sort_by(|a, b| {
        b.market_value
            .cmp(&a.market_value)
            .then(a.conid.cmp(&b.conid))
    });
}

#[derive(Deserialize)]
struct AsOfRow {
    #[serde(with = "time::serde::rfc3339")]
    as_of: OffsetDateTime,
}

#[derive(Deserialize)]
struct PointRow {
    #[serde(with = "time::serde::rfc3339")]
    t: OffsetDateTime,
    value: String,
}

/// The last sync time and the daily points since `since`, oldest first, at
/// most [`MAX_HISTORY_POINTS`]. `None` before the first sync.
pub async fn load_history(
    db: &Database,
    since: OffsetDateTime,
) -> Result<Option<(OffsetDateTime, Vec<Point>)>> {
    let mut response = db
        .query(format!(
            "SELECT as_of FROM ibkr_portfolio:current; \
             SELECT t, value FROM ibkr_portfolio_history WHERE t >= <datetime>$since \
             ORDER BY t LIMIT {MAX_HISTORY_POINTS}"
        ))
        .bind(json!({"since": rfc3339(since)?}))
        .await?
        .check()?;
    let as_of: Vec<AsOfRow> = response.take(0)?;
    let points: Vec<PointRow> = response.take(1)?;
    let Some(as_of) = as_of.into_iter().next() else {
        return Ok(None);
    };
    let points = points
        .into_iter()
        .map(|p| {
            Ok(Point {
                t: p.t,
                value: decimal(&p.value)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some((as_of.as_of, points)))
}

/// The stored position with this contract id, if the account holds it.
pub async fn held_instrument(db: &Database, conid: i64) -> Result<Option<Instrument>> {
    let rows: Vec<Instrument> = db
        .query("SELECT symbol, name, currency FROM ibkr_positions WHERE conid = $conid LIMIT 1")
        .bind(json!({"conid": conid}))
        .await?
        .check()?
        .take(0)?;
    Ok(rows.into_iter().next())
}

#[cfg(test)]
mod tests {
    use std::env;

    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};
    use time::Duration;

    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    fn holding(conid: i64, market_value: &str) -> Holding {
        Holding {
            conid,
            symbol: format!("S{conid}"),
            name: None,
            asset_class: "STK".into(),
            currency: "EUR".into(),
            quantity: d("12.5"),
            avg_cost: d("101.123456789"),
            last_price: d("120"),
            market_value: d(market_value),
            unrealized_pnl: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable JARVIS_SURREAL_TEST_* database"]
    async fn portfolio_repository_round_trips_and_replaces(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = Surreal::new::<Ws>(env::var("JARVIS_SURREAL_TEST_ENDPOINT")?).await?;
        db.signin(Root {
            username: &env::var("JARVIS_SURREAL_TEST_USER")?,
            password: &env::var("JARVIS_SURREAL_TEST_PASS")?,
        })
        .await?;
        db.use_ns(format!("ibkr_{}", uuid::Uuid::now_v7().simple()))
            .use_db("core")
            .await?;
        jarvis_store::apply_baseline_schema(&db).await?;
        let now = OffsetDateTime::now_utc().replace_nanosecond(0)?;

        assert_eq!(load_portfolio(&db).await?, None);
        assert_eq!(load_history(&db, OffsetDateTime::UNIX_EPOCH).await?, None);
        assert_eq!(held_instrument(&db, 1).await?, None);

        let mut first = Portfolio {
            as_of: now - Duration::days(2),
            net_liquidation: d("2500.5"),
            cash: d("-12.25"),
            positions: vec![
                Holding {
                    name: Some("Vanguard FTSE All-World".into()),
                    unrealized_pnl: Some(d("-3.5")),
                    ..holding(2, "100")
                },
                holding(1, "1500.75"),
            ],
            skipped: vec![
                Skipped {
                    symbol: "AAPL".into(),
                    currency: Some("USD".into()),
                },
                Skipped {
                    symbol: "NOCUR".into(),
                    currency: None,
                },
            ],
            skipped_count: 3,
        };
        write_snapshot(&db, &first).await?;
        sort_positions(&mut first.positions);
        assert_eq!(load_portfolio(&db).await?, Some(first.clone()));
        assert_eq!(
            held_instrument(&db, 2).await?,
            Some(Instrument {
                symbol: "S2".into(),
                name: Some("Vanguard FTSE All-World".into()),
                currency: "EUR".into(),
            })
        );

        // More skipped positions named than counted, or than the limit, are
        // refused.
        let overnamed = Portfolio {
            skipped_count: 1,
            ..first.clone()
        };
        assert!(matches!(
            write_snapshot(&db, &overnamed).await,
            Err(IbkrStoreError::InvalidData)
        ));

        // A later sync replaces every position; one point per day remains,
        // holding the last value of that day.
        let second = Portfolio {
            as_of: now,
            net_liquidation: d("2600"),
            cash: d("0"),
            positions: vec![holding(3, "50")],
            skipped: vec![],
            skipped_count: 0,
        };
        write_snapshot(&db, &second).await?;
        let same_day = Portfolio {
            net_liquidation: d("2601"),
            ..second.clone()
        };
        write_snapshot(&db, &same_day).await?;
        assert_eq!(load_portfolio(&db).await?, Some(same_day.clone()));
        assert_eq!(held_instrument(&db, 1).await?, None);
        let (as_of, points) = load_history(&db, OffsetDateTime::UNIX_EPOCH)
            .await?
            .ok_or("no history")?;
        assert_eq!(as_of, now);
        assert_eq!(
            points,
            [
                Point {
                    t: first.as_of,
                    value: d("2500.5")
                },
                Point {
                    t: now,
                    value: d("2601")
                },
            ]
        );
        let (_, recent) = load_history(&db, now - Duration::days(1))
            .await?
            .ok_or("no history")?;
        assert_eq!(recent.len(), 1);

        // An empty account is a valid sync too.
        let empty = Portfolio {
            positions: vec![],
            ..same_day
        };
        write_snapshot(&db, &empty).await?;
        assert_eq!(load_portfolio(&db).await?, Some(empty));
        Ok(())
    }
}
