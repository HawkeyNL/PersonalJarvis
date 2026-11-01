BEGIN TRANSACTION;
-- Read model of the memecoin PAPER trader. Paper only: no row here is an
-- order, an intent to execute or a limit. Money and token amounts are decimal
-- strings, never floats; Core parses them with rust_decimal. Mints are base58
-- Solana addresses. Rules that tie fields together sit on a required field,
-- whose ASSERT always runs.

-- Filter decisions, with the first failing rule; no reason means accepted.
-- Pruned after 30 days.
DEFINE TABLE IF NOT EXISTS memecoin_candidates SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_candidates TYPE string;
DEFINE FIELD mint ON memecoin_candidates TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD observed_at ON memecoin_candidates TYPE datetime;
DEFINE FIELD strategy_version ON memecoin_candidates TYPE int ASSERT $value >= 1;
DEFINE FIELD reason ON memecoin_candidates TYPE option<string> ASSERT $value = NONE OR $value IN ['data_unavailable', 'stale_snapshot', 'invalid_snapshot', 'denied', 'mint_authority_active', 'freeze_authority_active', 'unknown_extension', 'extension_not_allowed', 'extension_authority_active', 'lp_not_secured', 'lp_unlocks_too_soon', 'liquidity_too_low', 'pool_too_young', 'holder_concentration_too_high', 'no_sell_route', 'sell_impact_too_high'];
DEFINE FIELD price_usd ON memecoin_candidates TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD liquidity_usd ON memecoin_candidates TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE INDEX IF NOT EXISTS memecoin_candidates_id ON memecoin_candidates FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_candidates_observed ON memecoin_candidates FIELDS observed_at;

-- Strategy parameter sets (canonical JSON). A version never changes.
DEFINE TABLE IF NOT EXISTS memecoin_strategy_versions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_strategy_versions TYPE string;
DEFINE FIELD version ON memecoin_strategy_versions TYPE int ASSERT $value >= 1;
DEFINE FIELD params ON memecoin_strategy_versions TYPE string ASSERT string::len($value) >= 2 AND string::len($value) <= 8192;
DEFINE FIELD created_at ON memecoin_strategy_versions TYPE datetime;
DEFINE INDEX IF NOT EXISTS memecoin_strategy_versions_version ON memecoin_strategy_versions FIELDS version UNIQUE;

-- One row is one lot, the position it forms and its round trip: paper opens
-- at most one lot per mint and always sells it whole. `slot` is the mint
-- while open and the row id once closed, so the UNIQUE index allows one open
-- position per mint. Never pruned: the paper gate needs the whole history.
DEFINE TABLE IF NOT EXISTS memecoin_paper_trips SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_paper_trips TYPE string;
DEFINE FIELD slot ON memecoin_paper_trips TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 64;
DEFINE FIELD mint ON memecoin_paper_trips TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD strategy_version ON memecoin_paper_trips TYPE int ASSERT $value >= 1;
DEFINE FIELD status ON memecoin_paper_trips TYPE string ASSERT ($value = 'open' AND slot = mint AND closed_at = NONE AND usd_out = NONE AND exit_fees_usd = NONE AND net_pnl_usd = NONE AND exit_reason = NONE) OR ($value = 'closed' AND slot != mint AND closed_at != NONE AND usd_out != NONE AND exit_fees_usd != NONE AND net_pnl_usd != NONE AND exit_reason != NONE);
DEFINE FIELD tokens ON memecoin_paper_trips TYPE string ASSERT $value = /^[0-9]{1,28}(\.[0-9]{1,28})?$/;
DEFINE FIELD cost_usd ON memecoin_paper_trips TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD entry_liquidity_usd ON memecoin_paper_trips TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD opened_at ON memecoin_paper_trips TYPE datetime;
DEFINE FIELD closed_at ON memecoin_paper_trips TYPE option<datetime>;
DEFINE FIELD usd_out ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD exit_fees_usd ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD net_pnl_usd ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD exit_reason ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value IN ['liquidity_drop', 'stop_loss', 'take_profit', 'time_stop'];
DEFINE INDEX IF NOT EXISTS memecoin_paper_trips_id ON memecoin_paper_trips FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_paper_trips_slot ON memecoin_paper_trips FIELDS slot UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_paper_trips_closed ON memecoin_paper_trips FIELDS closed_at;

-- Costs without a fill: fees of failed attempts and attributed AI spend.
-- Never pruned: they are part of the net result.
DEFINE TABLE IF NOT EXISTS memecoin_paper_costs SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_paper_costs TYPE string;
DEFINE FIELD kind ON memecoin_paper_costs TYPE string ASSERT $value IN ['failed_tx', 'ai_cost'];
DEFINE FIELD usd ON memecoin_paper_costs TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD at ON memecoin_paper_costs TYPE datetime;
DEFINE FIELD mint ON memecoin_paper_costs TYPE option<string> ASSERT $value = NONE OR $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD strategy_version ON memecoin_paper_costs TYPE int ASSERT $value >= 1;
DEFINE INDEX IF NOT EXISTS memecoin_paper_costs_id ON memecoin_paper_costs FIELDS id UNIQUE;

-- Hourly paper-gate reports for the trend view. Pruned after 30 days.
DEFINE TABLE IF NOT EXISTS memecoin_gate_snapshots SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_gate_snapshots TYPE string;
DEFINE FIELD at ON memecoin_gate_snapshots TYPE datetime;
DEFINE FIELD strategy_version ON memecoin_gate_snapshots TYPE int ASSERT $value >= 1;
DEFINE FIELD closed_trades ON memecoin_gate_snapshots TYPE int ASSERT $value >= 0;
DEFINE FIELD duration_secs ON memecoin_gate_snapshots TYPE int ASSERT $value >= 0;
DEFINE FIELD trading_days ON memecoin_gate_snapshots TYPE int ASSERT $value >= 0;
DEFINE FIELD net_result_usd ON memecoin_gate_snapshots TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD net_expectancy_usd ON memecoin_gate_snapshots TYPE string ASSERT $value = /^-?[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE INDEX IF NOT EXISTS memecoin_gate_snapshots_id ON memecoin_gate_snapshots FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_gate_snapshots_at ON memecoin_gate_snapshots FIELDS at;

-- The owner's pause of paper entries: only the row `paper` is used.
DEFINE TABLE IF NOT EXISTS memecoin_paper_control SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_paper_control TYPE string;
DEFINE FIELD paused ON memecoin_paper_control TYPE bool ASSERT $value = (paused_at != NONE) AND $value = (paused_by != NONE);
DEFINE FIELD paused_at ON memecoin_paper_control TYPE option<datetime>;
DEFINE FIELD paused_by ON memecoin_paper_control TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 64;

UPDATE schema_version:baseline SET version = 14, applied_at = time::now();
COMMIT TRANSACTION;
