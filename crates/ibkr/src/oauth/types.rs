//! Typed read-only responses. Every field is optional or defaulted and unknown
//! fields are ignored: IBKR shapes are only partly documented. Money and
//! quantities are `Decimal`, parsed from the JSON text (number or string).

use std::{collections::BTreeMap, str::FromStr};

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn decimal_from_text(text: &str) -> Option<Decimal> {
    let text = text.trim().trim_end_matches('%');
    Decimal::from_str(text)
        .or_else(|_| Decimal::from_scientific(text))
        .ok()
}

fn decimal_from_value(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(number) => decimal_from_text(&number.to_string()),
        Value::String(text) => decimal_from_text(text),
        _ => None,
    }
}

/// Number, numeric string or null; anything unparsable becomes `None`.
fn opt_decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Decimal>, D::Error> {
    Ok(Option::<Value>::deserialize(deserializer)?
        .as_ref()
        .and_then(decimal_from_value))
}

/// Contract ids come as numbers or numeric strings depending on the endpoint.
fn conid<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| serde::de::Error::custom("invalid conid"))
}

/// `GET /portfolio/accounts` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "accountTitle", default)]
    pub title: Option<String>,
    #[serde(rename = "type", default)]
    pub account_type: Option<String>,
    // Unverified: the spec lists `baseCurrency`; older docs show `currency`.
    #[serde(default, alias = "baseCurrency")]
    pub currency: Option<String>,
}

/// `GET /portfolio/{account}/positions/{page}` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    #[serde(deserialize_with = "conid")]
    pub conid: i64,
    #[serde(rename = "contractDesc", default)]
    pub description: Option<String>,
    #[serde(default)]
    pub ticker: Option<String>,
    #[serde(rename = "assetClass", default)]
    pub asset_class: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub position: Option<Decimal>,
    #[serde(rename = "mktPrice", default, deserialize_with = "opt_decimal")]
    pub market_price: Option<Decimal>,
    #[serde(rename = "mktValue", default, deserialize_with = "opt_decimal")]
    pub market_value: Option<Decimal>,
    #[serde(rename = "avgCost", default, deserialize_with = "opt_decimal")]
    pub avg_cost: Option<Decimal>,
    #[serde(rename = "avgPrice", default, deserialize_with = "opt_decimal")]
    pub avg_price: Option<Decimal>,
    #[serde(rename = "realizedPnl", default, deserialize_with = "opt_decimal")]
    pub realized_pnl: Option<Decimal>,
    #[serde(rename = "unrealizedPnl", default, deserialize_with = "opt_decimal")]
    pub unrealized_pnl: Option<Decimal>,
}

/// One currency of `GET /portfolio/{account}/ledger`, keyed by currency
/// (`BASE`, `USD`, ...). Field names are unverified (from memory of IBKR docs).
#[derive(Debug, Clone, Deserialize)]
pub struct LedgerEntry {
    #[serde(default, deserialize_with = "opt_decimal")]
    pub cashbalance: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub settledcash: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub netliquidationvalue: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub stockmarketvalue: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub unrealizedpnl: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub realizedpnl: Option<Decimal>,
    #[serde(default, deserialize_with = "opt_decimal")]
    pub interest: Option<Decimal>,
}

pub type Ledger = BTreeMap<String, LedgerEntry>;

/// `GET /iserver/secdef/search` entry. Fields beyond `conid` are unverified.
#[derive(Debug, Clone, Deserialize)]
pub struct SecdefMatch {
    #[serde(deserialize_with = "conid")]
    pub conid: i64,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(rename = "companyName", default)]
    pub company_name: Option<String>,
    /// Usually the primary exchange.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sections: Vec<SecdefSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SecdefSection {
    #[serde(rename = "secType", default)]
    pub sec_type: Option<String>,
    #[serde(default)]
    pub exchange: Option<String>,
}

/// Snapshot field ids requested by [`super::IbkrOAuthClient::snapshot`].
pub(crate) const SNAPSHOT_FIELDS: &str = "31,55,82,83,84,86,6509,7296,7741";
const DATA_FIELDS: [&str; 8] = ["31", "55", "82", "83", "84", "86", "7296", "7741"];

/// Market data availability (snapshot field 6509, first character).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketDataAvailability {
    RealTime,
    Delayed,
    Frozen,
    FrozenDelayed,
    NotSubscribed,
    Unknown,
}

impl MarketDataAvailability {
    fn parse(code: &str) -> Self {
        match code.chars().next() {
            Some('R') => Self::RealTime,
            Some('D') => Self::Delayed,
            Some('Z') => Self::Frozen,
            Some('Y') => Self::FrozenDelayed,
            Some('N') => Self::NotSubscribed,
            _ => Self::Unknown,
        }
    }
}

/// One contract of `GET /iserver/marketdata/snapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub conid: i64,
    pub symbol: Option<String>,
    pub last: Option<Decimal>,
    /// Field 31 carried the `C` prefix: `last` is the previous close.
    pub last_is_prior_close: bool,
    /// Field 31 carried the `H` prefix: trading is halted.
    pub halted: bool,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    pub change: Option<Decimal>,
    pub change_percent: Option<Decimal>,
    /// Field 7296 (today's close) and 7741 (prior close); which one is filled
    /// depends on the session, so both are kept.
    pub close: Option<Decimal>,
    pub prior_close: Option<Decimal>,
    pub availability: Option<MarketDataAvailability>,
    pub updated_ms: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct RawSnapshot {
    #[serde(deserialize_with = "conid")]
    pub(crate) conid: i64,
    #[serde(rename = "_updated", default)]
    updated: Option<i64>,
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

impl RawSnapshot {
    /// The first request for a contract only opens the subscription and
    /// returns no data fields.
    pub(crate) fn has_data(&self) -> bool {
        DATA_FIELDS.iter().any(|id| self.fields.contains_key(*id))
    }

    pub(crate) fn into_snapshot(self) -> Snapshot {
        let text = |id: &str| self.fields.get(id).and_then(Value::as_str);
        let decimal = |id: &str| self.fields.get(id).and_then(decimal_from_value);
        let last_raw = text("31").unwrap_or_default();
        let last_digits = last_raw.trim_start_matches(['C', 'H']);
        Snapshot {
            conid: self.conid,
            symbol: text("55").map(str::to_string),
            last: decimal_from_text(last_digits),
            last_is_prior_close: last_raw.starts_with('C'),
            halted: last_raw.starts_with('H'),
            bid: decimal("84"),
            ask: decimal("86"),
            change: decimal("82"),
            change_percent: decimal("83"),
            close: decimal("7296"),
            prior_close: decimal("7741"),
            availability: text("6509").map(MarketDataAvailability::parse),
            updated_ms: self.updated,
        }
    }
}

/// `GET /iserver/marketdata/history`. The top-level `high`/`low` composite
/// strings are ignored in favour of the bars.
#[derive(Debug, Clone, Deserialize)]
pub struct History {
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(rename = "data", default)]
    pub bars: Vec<Bar>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bar {
    /// Bar start, epoch milliseconds (unverified: the IBKR sample looks off).
    #[serde(rename = "t")]
    pub time_ms: i64,
    #[serde(rename = "o", default, deserialize_with = "opt_decimal")]
    pub open: Option<Decimal>,
    #[serde(rename = "h", default, deserialize_with = "opt_decimal")]
    pub high: Option<Decimal>,
    #[serde(rename = "l", default, deserialize_with = "opt_decimal")]
    pub low: Option<Decimal>,
    #[serde(rename = "c", default, deserialize_with = "opt_decimal")]
    pub close: Option<Decimal>,
    #[serde(rename = "v", default, deserialize_with = "opt_decimal")]
    pub volume: Option<Decimal>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    // Fixtures below are hand-written from the documented shapes, not captured
    // from IBKR.

    #[test]
    fn parses_accounts() {
        let json = r#"[{"id":"DU1234567","accountId":"DU1234567","accountTitle":"Paper","type":"DEMO","tradingType":"PMRGN","currency":"EUR"}]"#;
        let accounts: Vec<Account> = serde_json::from_str(json).unwrap();
        assert_eq!(accounts[0].account_id, "DU1234567");
        assert_eq!(accounts[0].account_type.as_deref(), Some("DEMO"));
        assert_eq!(accounts[0].currency.as_deref(), Some("EUR"));
    }

    #[test]
    fn parses_positions_as_exact_decimals() {
        let json = r#"[{"acctId":"DU1234567","conid":265598,"contractDesc":"AAPL","position":12.5,"mktPrice":190.1,"mktValue":"2376.25","avgCost":178.51,"avgPrice":178.51,"realizedPnl":0,"unrealizedPnl":1.1e2,"currency":"USD","assetClass":"STK","ticker":"AAPL","extra":{}}]"#;
        let positions: Vec<Position> = serde_json::from_str(json).unwrap();
        let p = &positions[0];
        assert_eq!(p.conid, 265598);
        assert_eq!(p.position, Some(dec("12.5")));
        assert_eq!(p.market_price, Some(dec("190.1")));
        assert_eq!(p.market_value, Some(dec("2376.25")));
        assert_eq!(p.avg_cost, Some(dec("178.51")));
        assert_eq!(p.unrealized_pnl, Some(dec("110")));
        assert_eq!(p.ticker.as_deref(), Some("AAPL"));
    }

    #[test]
    fn parses_ledger_by_currency() {
        let json = r#"{"BASE":{"cashbalance":1000.5,"netliquidationvalue":25000.25,"currency":"BASE"},"USD":{"cashbalance":-12.3,"settledcash":null}}"#;
        let ledger: Ledger = serde_json::from_str(json).unwrap();
        assert_eq!(ledger["BASE"].cashbalance, Some(dec("1000.5")));
        assert_eq!(ledger["BASE"].netliquidationvalue, Some(dec("25000.25")));
        assert_eq!(ledger["USD"].cashbalance, Some(dec("-12.3")));
        assert_eq!(ledger["USD"].settledcash, None);
    }

    #[test]
    fn parses_secdef_search_with_string_conids() {
        let json = r#"[{"conid":"265598","companyHeader":"APPLE INC - NASDAQ","companyName":"APPLE INC","symbol":"AAPL","description":"NASDAQ","sections":[{"secType":"STK"},{"secType":"OPT","months":"NOV26","exchange":"SMART"}]}]"#;
        let matches: Vec<SecdefMatch> = serde_json::from_str(json).unwrap();
        assert_eq!(matches[0].conid, 265598);
        assert_eq!(matches[0].sections[1].sec_type.as_deref(), Some("OPT"));
        assert!(serde_json::from_str::<Vec<SecdefMatch>>(r#"[{"conid":"x"}]"#).is_err());
    }

    #[test]
    fn parses_snapshot_fields_and_availability() {
        let json = r#"[{"conid":265598,"conidEx":"265598","_updated":1730000000000,"server_id":"q0","31":"C189.50","55":"AAPL","84":"189.40","86":"189.60","82":"-1.25","83":"-0.66%","7741":"190.75","6509":"DPB"},{"conid":8314,"_updated":1730000000001,"server_id":"q1"},{"conid":9,"31":"H12","6509":"RpB"}]"#;
        let raw: Vec<RawSnapshot> = serde_json::from_str(json).unwrap();
        assert_eq!(
            raw.iter().map(RawSnapshot::has_data).collect::<Vec<_>>(),
            [true, false, true]
        );
        let snaps: Vec<Snapshot> = raw.into_iter().map(RawSnapshot::into_snapshot).collect();
        let aapl = &snaps[0];
        assert_eq!(aapl.last, Some(dec("189.50")));
        assert!(aapl.last_is_prior_close && !aapl.halted);
        assert_eq!(aapl.change_percent, Some(dec("-0.66")));
        assert_eq!(aapl.prior_close, Some(dec("190.75")));
        assert_eq!(aapl.availability, Some(MarketDataAvailability::Delayed));
        assert_eq!(aapl.updated_ms, Some(1730000000000));
        assert_eq!(snaps[1].last, None);
        assert_eq!(snaps[1].availability, None);
        assert!(snaps[2].halted);
        assert_eq!(snaps[2].last, Some(dec("12")));
        assert_eq!(
            snaps[2].availability,
            Some(MarketDataAvailability::RealTime)
        );
        for (code, kind) in [
            ("ZPB", MarketDataAvailability::Frozen),
            ("YpB", MarketDataAvailability::FrozenDelayed),
            ("N", MarketDataAvailability::NotSubscribed),
            ("", MarketDataAvailability::Unknown),
        ] {
            assert_eq!(MarketDataAvailability::parse(code), kind);
        }
    }

    #[test]
    fn parses_history_bars() {
        let json = r#"{"serverId":"1","symbol":"AAPL","text":"APPLE INC","priceFactor":100,"high":"17510/472117.45/0","data":[{"o":173.4,"c":174.55,"h":175.1,"l":172.98,"v":472117.45,"t":1692345600000}],"points":0}"#;
        let history: History = serde_json::from_str(json).unwrap();
        assert_eq!(history.symbol.as_deref(), Some("AAPL"));
        let bar = &history.bars[0];
        assert_eq!(bar.time_ms, 1692345600000);
        assert_eq!(bar.open, Some(dec("173.4")));
        assert_eq!(bar.close, Some(dec("174.55")));
        assert_eq!(bar.volume, Some(dec("472117.45")));
    }
}
