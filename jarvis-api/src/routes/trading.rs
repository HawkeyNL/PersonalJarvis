//! Trading read API of the app (`desktop/src/trading.ts` in the app repo):
//! the synced IBKR portfolio, its daily history and the bars of a held
//! instrument. Read-only, owner session like every `/v1` route. Decimal
//! values are strings. Every body carries `source` and `as_of`;
//! `delay_minutes` is left out because the broker does not report the price
//! delay, and the app shows an absent delay as unknown.
//!
//! A 503 means IBKR is not configured, has no data yet or cannot be reached
//! (the app shows "not connected"); a 500 is a Core failure.

use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use jarvis_ibkr::oauth::Bar;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::bad_request;
use crate::ibkr::{self, store};
use crate::{AppState, Authed};

const SOURCE: &str = "IBKR";
const DB_TIMEOUT: Duration = Duration::from_secs(10);
/// The app's limit for one bars response.
const MAX_BARS: usize = 5000;
/// History ranges and their window in days; `ALL` is everything kept.
const HISTORY_RANGES: [(&str, i64); 4] = [
    ("1M", 31),
    ("3M", 92),
    ("1Y", 366),
    ("ALL", store::HISTORY_DAYS as i64),
];
/// Bar ranges and the IBKR `period` and `bar` they read.
const BAR_RANGES: [(&str, &str, &str); 6] = [
    ("1D", "1d", "5min"),
    ("1W", "1w", "1h"),
    ("1M", "1m", "1d"),
    ("3M", "3m", "1d"),
    ("1Y", "1y", "1d"),
    ("5Y", "5y", "1w"),
];

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn unavailable(message: &str) -> ApiError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": message })),
    )
}

fn not_configured() -> ApiError {
    unavailable("IBKR portfolio sync is not configured")
}

fn no_data() -> ApiError {
    unavailable("no IBKR portfolio data yet")
}

fn internal() -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
}

async fn read<T>(
    future: impl std::future::Future<Output = Result<T, store::IbkrStoreError>>,
) -> Result<T, ApiError> {
    match tokio::time::timeout(DB_TIMEOUT, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(%error, "IBKR portfolio read failed");
            Err(internal())
        }
        Err(_) => {
            tracing::warn!("IBKR portfolio read timed out");
            Err(internal())
        }
    }
}

fn ts(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

fn freshness(as_of: OffsetDateTime) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("source".into(), SOURCE.into());
    body.insert("as_of".into(), ts(as_of).into());
    body
}

#[derive(Deserialize)]
pub(crate) struct RangeQuery {
    range: Option<String>,
}

/// `Portfolio` of the app contract.
pub(crate) fn portfolio_body(portfolio: &store::Portfolio) -> Value {
    let net = portfolio.net_liquidation;
    let positions: Vec<Value> = portfolio
        .positions
        .iter()
        .map(|h| {
            let mut row = json!({
                "conid": h.conid,
                "symbol": h.symbol,
                "asset_class": h.asset_class,
                "currency": h.currency,
                "quantity": h.quantity.to_string(),
                "avg_cost": h.avg_cost.to_string(),
                "last_price": h.last_price.to_string(),
                "market_value": h.market_value.to_string(),
            });
            if let Some(name) = &h.name {
                row["name"] = name.as_str().into();
            }
            if let Some(pnl) = h.unrealized_pnl {
                row["unrealized_pnl"] = pnl.to_string().into();
            }
            // A calculation, not a broker value: share of net liquidation.
            if net > Decimal::ZERO {
                if let Some(pct) = h
                    .market_value
                    .checked_div(net)
                    .and_then(|weight| weight.checked_mul(Decimal::ONE_HUNDRED))
                {
                    row["weight_pct"] = pct.round_dp(2).to_string().into();
                }
            }
            row
        })
        .collect();
    let mut body = freshness(portfolio.as_of);
    body.insert("base_currency".into(), store::BASE_CURRENCY.into());
    body.insert("net_liquidation".into(), net.to_string().into());
    body.insert("cash".into(), portfolio.cash.to_string().into());
    body.insert("positions".into(), positions.into());
    // Positions not in EUR the sync left out; absent when there are none.
    if portfolio.skipped_count > 0 {
        body.insert("skipped_count".into(), portfolio.skipped_count.into());
        body.insert("skipped_positions".into(), json!(portfolio.skipped));
    }
    Value::Object(body)
}

/// `PortfolioHistory` of the app contract.
pub(crate) fn history_body(range: &str, as_of: OffsetDateTime, points: &[store::Point]) -> Value {
    let mut body = freshness(as_of);
    body.insert("range".into(), range.into());
    body.insert("base_currency".into(), store::BASE_CURRENCY.into());
    body.insert(
        "points".into(),
        points
            .iter()
            .map(|p| json!({ "t": ts(p.t), "value": p.value.to_string() }))
            .collect::<Vec<_>>()
            .into(),
    );
    Value::Object(body)
}

/// One validated bar, or `None` for a bar without a time in range or
/// without all four prices (the app would drop it anyway).
fn bar_row(bar: &Bar, latest_ms: i64) -> Option<Value> {
    if !(0..=latest_ms).contains(&bar.time_ms) {
        return None;
    }
    let t = OffsetDateTime::from_unix_timestamp_nanos(i128::from(bar.time_ms) * 1_000_000).ok()?;
    let price = |value: Option<Decimal>| value.and_then(ibkr::amount).map(|v| v.to_string());
    let mut row = json!({
        "t": ts(t),
        "o": price(bar.open)?,
        "h": price(bar.high)?,
        "l": price(bar.low)?,
        "c": price(bar.close)?,
    });
    if let Some(volume) = price(bar.volume) {
        row["v"] = volume.into();
    }
    Some(row)
}

/// `InstrumentBars` of the app contract. `bars` come from the broker in any
/// order; they are sorted, validated and capped here.
pub(crate) fn bars_body(
    conid: i64,
    instrument: &store::Instrument,
    range: &str,
    as_of: OffsetDateTime,
    mut bars: Vec<Bar>,
) -> Value {
    bars.sort_by_key(|bar| bar.time_ms);
    // A bar may start at most a day ahead of Core's clock.
    let latest_ms =
        i64::try_from((as_of.unix_timestamp_nanos() / 1_000_000) + 86_400_000).unwrap_or(i64::MAX);
    let mut rows: Vec<Value> = bars
        .iter()
        .filter_map(|bar| bar_row(bar, latest_ms))
        .collect();
    // Keep the newest bars.
    rows.drain(..rows.len().saturating_sub(MAX_BARS));
    let mut body = freshness(as_of);
    body.insert("conid".into(), conid.into());
    body.insert("symbol".into(), instrument.symbol.as_str().into());
    if let Some(name) = &instrument.name {
        body.insert("name".into(), name.as_str().into());
    }
    body.insert("currency".into(), instrument.currency.as_str().into());
    body.insert("range".into(), range.into());
    body.insert("bars".into(), rows.into());
    Value::Object(body)
}

/// A contract id from the path: 1 to 12 digits, positive.
fn parse_conid(value: &str) -> Option<i64> {
    ((1..=12).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
        .filter(|conid| *conid > 0)
}

pub(crate) async fn portfolio(_authed: Authed, State(state): State<AppState>) -> ApiResult {
    if state.ibkr.is_none() {
        return Err(not_configured());
    }
    let portfolio = read(store::load_portfolio(&state.db))
        .await?
        .ok_or_else(no_data)?;
    Ok(Json(portfolio_body(&portfolio)))
}

pub(crate) async fn history(
    _authed: Authed,
    State(state): State<AppState>,
    Query(query): Query<RangeQuery>,
) -> ApiResult {
    if state.ibkr.is_none() {
        return Err(not_configured());
    }
    let (range, days) = HISTORY_RANGES
        .into_iter()
        .find(|(range, _)| Some(*range) == query.range.as_deref())
        .ok_or_else(|| bad_request("range must be 1M, 3M, 1Y or ALL"))?;
    let since = OffsetDateTime::now_utc() - time::Duration::days(days);
    let (as_of, points) = read(store::load_history(&state.db, since))
        .await?
        .ok_or_else(no_data)?;
    Ok(Json(history_body(range, as_of, &points)))
}

/// Bars of an instrument the account holds, read live through the broker.
/// Other contract ids are refused: Core is not a general market data proxy,
/// and the held position supplies the symbol and currency.
pub(crate) async fn bars(
    _authed: Authed,
    State(state): State<AppState>,
    Path(conid): Path<String>,
    Query(query): Query<RangeQuery>,
) -> ApiResult {
    let client = state.ibkr.clone().ok_or_else(not_configured)?;
    let conid = parse_conid(&conid).ok_or_else(|| bad_request("invalid contract id"))?;
    let (range, period, bar) = BAR_RANGES
        .into_iter()
        .find(|(range, _, _)| Some(*range) == query.range.as_deref())
        .ok_or_else(|| bad_request("range must be 1D, 1W, 1M, 3M, 1Y or 5Y"))?;
    let instrument = read(store::held_instrument(&state.db, conid))
        .await?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "instrument not in the portfolio" })),
            )
        })?;
    let history = match tokio::time::timeout(
        ibkr::BROKER_TIMEOUT,
        client.history(conid, period, bar),
    )
    .await
    {
        Ok(Ok(history)) => history,
        Ok(Err(error)) => {
            tracing::warn!(%error, "IBKR bars read failed");
            return Err(unavailable("IBKR data unavailable"));
        }
        Err(_) => return Err(unavailable("IBKR data unavailable")),
    };
    Ok(Json(bars_body(
        conid,
        &instrument,
        range,
        OffsetDateTime::now_utc(),
        history.bars,
    )))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    /// The app's decimal string pattern.
    fn is_decimal(value: &Value) -> bool {
        let Some(text) = value.as_str() else {
            return false;
        };
        let digits = text.strip_prefix('-').unwrap_or(text);
        let (int, frac) = digits.split_once('.').unwrap_or((digits, "1"));
        !int.is_empty()
            && !frac.is_empty()
            && int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit())
    }

    /// `isFresh` of the app: source and as_of strings, delay absent here.
    fn assert_fresh(body: &Value) {
        assert_eq!(body["source"], "IBKR");
        let as_of = body["as_of"].as_str().unwrap();
        assert!(OffsetDateTime::parse(as_of, &Rfc3339).is_ok(), "{as_of}");
        assert!(body.get("delay_minutes").is_none());
    }

    fn holding(conid: i64, market_value: &str) -> store::Holding {
        store::Holding {
            conid,
            symbol: "VWCE".into(),
            name: None,
            asset_class: "STK".into(),
            currency: "EUR".into(),
            quantity: d("10"),
            avg_cost: d("101.25"),
            last_price: d("120.5"),
            market_value: d(market_value),
            unrealized_pnl: None,
        }
    }

    #[test]
    fn portfolio_body_matches_the_app_contract() {
        let portfolio = store::Portfolio {
            as_of: OffsetDateTime::UNIX_EPOCH,
            net_liquidation: d("3000"),
            cash: d("-12.5"),
            positions: vec![
                store::Holding {
                    name: Some("Vanguard FTSE All-World".into()),
                    unrealized_pnl: Some(d("192.5")),
                    ..holding(12345, "1000")
                },
                holding(7, "0.01"),
            ],
            skipped: vec![],
            skipped_count: 0,
        };
        let body = portfolio_body(&portfolio);
        assert_fresh(&body);
        assert_eq!(body["as_of"], "1970-01-01T00:00:00Z");
        assert_eq!(body["base_currency"], "EUR");
        assert_eq!(body["net_liquidation"], "3000");
        assert_eq!(body["cash"], "-12.5");
        let first = &body["positions"][0];
        assert_eq!(first["conid"], 12345);
        assert_eq!(first["name"], "Vanguard FTSE All-World");
        assert_eq!(first["unrealized_pnl"], "192.5");
        assert_eq!(first["weight_pct"], "33.33");
        for field in [
            "quantity",
            "avg_cost",
            "last_price",
            "market_value",
            "weight_pct",
        ] {
            assert!(is_decimal(&first[field]), "{field}: {}", first[field]);
        }
        let second = &body["positions"][1];
        assert_eq!(second["weight_pct"], "0.00");
        // Optional fields are absent, never null.
        assert!(second.get("name").is_none() && second.get("unrealized_pnl").is_none());

        assert!(body.get("skipped_count").is_none() && body.get("skipped_positions").is_none());

        // Positions left out are reported by count and by symbol; an
        // unknown currency is absent.
        let with_skipped = store::Portfolio {
            skipped: vec![
                store::Skipped {
                    symbol: "AAPL".into(),
                    currency: Some("USD".into()),
                },
                store::Skipped {
                    symbol: "NOCUR".into(),
                    currency: None,
                },
            ],
            skipped_count: 60,
            ..portfolio.clone()
        };
        let body = portfolio_body(&with_skipped);
        assert_eq!(body["skipped_count"], 60);
        assert_eq!(
            body["skipped_positions"],
            json!([{ "symbol": "AAPL", "currency": "USD" }, { "symbol": "NOCUR" }])
        );

        // No weight against a zero or negative net liquidation.
        let empty = store::Portfolio {
            net_liquidation: d("0"),
            ..portfolio
        };
        assert!(portfolio_body(&empty)["positions"][0]
            .get("weight_pct")
            .is_none());
    }

    #[test]
    fn history_body_matches_the_app_contract() {
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let body = history_body(
            "1M",
            at,
            &[store::Point {
                t: at,
                value: d("2500.5"),
            }],
        );
        assert_fresh(&body);
        assert_eq!(body["range"], "1M");
        assert_eq!(body["base_currency"], "EUR");
        assert_eq!(
            body["points"],
            json!([{ "t": "1970-01-02T00:00:00Z", "value": "2500.5" }])
        );
    }

    fn bar(t: i64, high: Option<&str>, volume: Option<&str>) -> Bar {
        Bar {
            time_ms: t,
            open: Some(d("120")),
            high: high.map(d),
            low: Some(d("119")),
            close: Some(d("121.50")),
            volume: volume.map(d),
        }
    }

    #[test]
    fn bars_body_sorts_validates_and_caps() {
        let as_of = OffsetDateTime::UNIX_EPOCH + time::Duration::days(10);
        let instrument = store::Instrument {
            symbol: "VWCE".into(),
            name: Some("Vanguard FTSE All-World".into()),
            currency: "EUR".into(),
        };
        let day = 86_400_000;
        let body = bars_body(
            12345,
            &instrument,
            "1M",
            as_of,
            vec![
                bar(2 * day, Some("122"), None),
                bar(day, Some("122"), Some("1500")),
                // No high, before the epoch, and too far ahead: dropped.
                bar(3 * day, None, None),
                bar(-1, Some("122"), None),
                bar(12 * day, Some("122"), None),
            ],
        );
        assert_fresh(&body);
        assert_eq!(body["conid"], 12345);
        assert_eq!(body["symbol"], "VWCE");
        assert_eq!(body["name"], "Vanguard FTSE All-World");
        assert_eq!(body["currency"], "EUR");
        assert_eq!(body["range"], "1M");
        assert_eq!(
            body["bars"],
            json!([
                { "t": "1970-01-02T00:00:00Z", "o": "120", "h": "122", "l": "119", "c": "121.5", "v": "1500" },
                { "t": "1970-01-03T00:00:00Z", "o": "120", "h": "122", "l": "119", "c": "121.5" },
            ])
        );
        let many = (0..MAX_BARS as i64 + 10)
            .map(|t| bar(t, Some("1"), None))
            .collect();
        let body = bars_body(1, &instrument, "1D", as_of, many);
        assert_eq!(body["bars"].as_array().unwrap().len(), MAX_BARS);
        assert_eq!(
            body["bars"][0]["t"],
            ts(OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(10))
        );
    }

    #[test]
    fn ranges_and_contract_ids_are_closed_sets() {
        assert_eq!(parse_conid("265598"), Some(265598));
        for bad in ["0", "-1", "", "1234567890123", "12a", "+1", " 1"] {
            assert_eq!(parse_conid(bad), None, "{bad}");
        }
        // The app's HISTORY_RANGES and BAR_RANGES, and periods and bars the
        // broker accepts.
        assert_eq!(HISTORY_RANGES.map(|r| r.0), ["1M", "3M", "1Y", "ALL"]);
        assert_eq!(
            BAR_RANGES.map(|r| r.0),
            ["1D", "1W", "1M", "3M", "1Y", "5Y"]
        );
    }
}
