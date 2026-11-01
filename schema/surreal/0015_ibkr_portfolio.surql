BEGIN TRANSACTION;
-- Read model of the IBKR portfolio: what the read-only broker reported at
-- Core's last good sync. No row here is an order, a mandate or a limit.
-- Amounts are decimal strings, never floats; Core parses them with
-- rust_decimal. Cash account, EUR base currency, EUR positions only.

-- The last good sync: only the row `current` is used.
DEFINE TABLE IF NOT EXISTS ibkr_portfolio SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON ibkr_portfolio TYPE string;
DEFINE FIELD as_of ON ibkr_portfolio TYPE datetime;
DEFINE FIELD base_currency ON ibkr_portfolio TYPE string ASSERT $value = 'EUR';
DEFINE FIELD net_liquidation ON ibkr_portfolio TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD cash ON ibkr_portfolio TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
-- Valid positions the sync left out because they are not in EUR: how many,
-- and at most 50 of them by symbol and currency (absent when IBKR reported
-- no three-letter code).
DEFINE FIELD skipped_count ON ibkr_portfolio TYPE int ASSERT $value >= 0 AND $value <= 1000;
DEFINE FIELD skipped_positions ON ibkr_portfolio TYPE array<object, 50>;
DEFINE FIELD skipped_positions.*.symbol ON ibkr_portfolio TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 32;
DEFINE FIELD skipped_positions.*.currency ON ibkr_portfolio TYPE option<string> ASSERT $value = NONE OR $value = /^[A-Z]{3}$/;

-- Positions of that sync, replaced as a whole on every good sync. The row id
-- is the IBKR contract id.
DEFINE TABLE IF NOT EXISTS ibkr_positions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON ibkr_positions TYPE string;
DEFINE FIELD conid ON ibkr_positions TYPE int ASSERT $value >= 1;
DEFINE FIELD symbol ON ibkr_positions TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 32;
DEFINE FIELD name ON ibkr_positions TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 128);
DEFINE FIELD asset_class ON ibkr_positions TYPE string ASSERT $value = /^[A-Z]{1,8}$/;
DEFINE FIELD currency ON ibkr_positions TYPE string ASSERT $value = /^[A-Z]{3}$/;
DEFINE FIELD quantity ON ibkr_positions TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD avg_cost ON ibkr_positions TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD last_price ON ibkr_positions TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD market_value ON ibkr_positions TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD unrealized_pnl ON ibkr_positions TYPE option<string> ASSERT $value = NONE OR $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE INDEX IF NOT EXISTS ibkr_positions_id ON ibkr_positions FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS ibkr_positions_conid ON ibkr_positions FIELDS conid UNIQUE;

-- Net liquidation per UTC day (the last good sync of that day), for the
-- portfolio chart. The row id is the date. Pruned after 3650 days.
DEFINE TABLE IF NOT EXISTS ibkr_portfolio_history SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON ibkr_portfolio_history TYPE string;
DEFINE FIELD t ON ibkr_portfolio_history TYPE datetime;
DEFINE FIELD value ON ibkr_portfolio_history TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE INDEX IF NOT EXISTS ibkr_portfolio_history_id ON ibkr_portfolio_history FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS ibkr_portfolio_history_t ON ibkr_portfolio_history FIELDS t;

UPDATE schema_version:baseline SET version = 15, applied_at = time::now();
COMMIT TRANSACTION;
