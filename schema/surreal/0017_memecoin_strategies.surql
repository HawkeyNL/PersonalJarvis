BEGIN TRANSACTION;
-- Several paper strategies, the decisions of the strategy agent and the
-- market data it reads. Paper only, like 0014: no row here is an order, an
-- intent to execute or a limit. Additive: rows written before 0017 stay
-- valid, and an older Core never writes or selects the new fields or tables.

-- A strategy version may belong to a strategy of a family, derive from a
-- parent version and record who wrote it and why. Versions recorded before
-- 0017 have none of these fields (legacy). `rationale` is display only;
-- `backtest` is canonical JSON.
DEFINE FIELD IF NOT EXISTS strategy_id ON memecoin_strategy_versions TYPE option<string> ASSERT $value = NONE OR ($value = /^[a-z0-9-]{1,32}$/ AND family != NONE AND author != NONE);
DEFINE FIELD IF NOT EXISTS family ON memecoin_strategy_versions TYPE option<string> ASSERT $value = NONE OR ($value IN ['a', 'b', 'c'] AND strategy_id != NONE);
DEFINE FIELD IF NOT EXISTS parent_version ON memecoin_strategy_versions TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value < version AND strategy_id != NONE);
DEFINE FIELD IF NOT EXISTS author ON memecoin_strategy_versions TYPE option<string> ASSERT $value = NONE OR ($value IN ['seed', 'claude'] AND strategy_id != NONE);
DEFINE FIELD IF NOT EXISTS rationale ON memecoin_strategy_versions TYPE option<string> ASSERT $value = NONE OR (string::len($value) <= 600 AND strategy_id != NONE);
DEFINE FIELD IF NOT EXISTS backtest ON memecoin_strategy_versions TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 2 AND bytes::len(<bytes>$value) <= 4096 AND strategy_id != NONE);
DEFINE INDEX IF NOT EXISTS memecoin_strategy_versions_strategy ON memecoin_strategy_versions FIELDS strategy_id, version;

-- One row per strategy: the version it trades and whether it may enter.
-- An owner's disable holds until the owner enables it again.
DEFINE TABLE IF NOT EXISTS memecoin_strategies SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_strategies TYPE string ASSERT record::id($value) = /^[a-z0-9-]{1,32}$/;
DEFINE FIELD active_version ON memecoin_strategies TYPE int ASSERT $value >= 1;
DEFINE FIELD enabled ON memecoin_strategies TYPE bool ASSERT $value = false OR disabled_by_owner = false;
DEFINE FIELD disabled_by_owner ON memecoin_strategies TYPE bool;
DEFINE FIELD changed_at ON memecoin_strategies TYPE datetime;
DEFINE FIELD changed_by ON memecoin_strategies TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 64;
DEFINE INDEX IF NOT EXISTS memecoin_strategies_id ON memecoin_strategies FIELDS id UNIQUE;

-- What the strategy agent decided on each call, or why it could not.
-- `rationale` is display only. Pruned after 90 days.
DEFINE TABLE IF NOT EXISTS memecoin_decisions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_decisions TYPE string;
DEFINE FIELD at ON memecoin_decisions TYPE datetime;
DEFINE FIELD call_id ON memecoin_decisions TYPE string ASSERT $value = /^[A-Za-z0-9_-]{1,64}$/;
DEFINE FIELD kind ON memecoin_decisions TYPE string ASSERT $value IN ['enter', 'skip', 'exit', 'hold', 'propose', 'worker_unavailable', 'invalid_output'] AND ($value = 'enter') = (size_bps != NONE) AND (($value != 'enter' AND $value != 'exit') OR mint != NONE);
DEFINE FIELD strategy_version ON memecoin_decisions TYPE option<int> ASSERT $value = NONE OR $value >= 1;
DEFINE FIELD mint ON memecoin_decisions TYPE option<string> ASSERT $value = NONE OR $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD size_bps ON memecoin_decisions TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 10000);
DEFINE FIELD reason_code ON memecoin_decisions TYPE option<string> ASSERT $value = NONE OR $value = /^[a-z][a-z0-9_]{0,31}$/;
DEFINE FIELD rationale ON memecoin_decisions TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 300;
DEFINE FIELD model ON memecoin_decisions TYPE option<string> ASSERT $value = NONE OR $value = /^[A-Za-z0-9._:-]{1,64}$/;
DEFINE FIELD input_tokens ON memecoin_decisions TYPE option<int> ASSERT $value = NONE OR ($value >= 0 AND $value <= 100000000);
DEFINE FIELD output_tokens ON memecoin_decisions TYPE option<int> ASSERT $value = NONE OR ($value >= 0 AND $value <= 100000000);
DEFINE INDEX IF NOT EXISTS memecoin_decisions_id ON memecoin_decisions FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_decisions_at ON memecoin_decisions FIELDS at;

-- Price and liquidity seen per mint. All are kept for 72 hours; after that
-- only the `daily` one (the first of each mint and UTC day), for 60 days.
DEFINE TABLE IF NOT EXISTS memecoin_observations SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_observations TYPE string;
DEFINE FIELD mint ON memecoin_observations TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD at ON memecoin_observations TYPE datetime;
DEFINE FIELD price_usd ON memecoin_observations TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD liquidity_usd ON memecoin_observations TYPE string ASSERT $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;
DEFINE FIELD daily ON memecoin_observations TYPE bool;
DEFINE INDEX IF NOT EXISTS memecoin_observations_id ON memecoin_observations FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_observations_mint_at ON memecoin_observations FIELDS mint, at;
DEFINE INDEX IF NOT EXISTS memecoin_observations_at ON memecoin_observations FIELDS at;

-- Recently migrated tokens that discovery may consider, one row per mint.
-- Pruned 7 days after the migration.
DEFINE TABLE IF NOT EXISTS memecoin_universe SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON memecoin_universe TYPE string;
DEFINE FIELD mint ON memecoin_universe TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD pool ON memecoin_universe TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD program ON memecoin_universe TYPE string ASSERT $value = /^[1-9A-HJ-NP-Za-km-z]{32,44}$/;
DEFINE FIELD migrated_at ON memecoin_universe TYPE datetime;
DEFINE INDEX IF NOT EXISTS memecoin_universe_mint ON memecoin_universe FIELDS mint UNIQUE;
DEFINE INDEX IF NOT EXISTS memecoin_universe_migrated ON memecoin_universe FIELDS migrated_at;

-- Trailing stops and agent exits; the highest value a position reached.
DEFINE FIELD OVERWRITE exit_reason ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value IN ['liquidity_drop', 'stop_loss', 'take_profit', 'time_stop', 'trailing_stop', 'agent_exit'];
DEFINE FIELD IF NOT EXISTS peak_value_usd ON memecoin_paper_trips TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{1,20}(\.[0-9]{1,28})?$/;

UPDATE schema_version:baseline SET version = 17, applied_at = time::now();
COMMIT TRANSACTION;
