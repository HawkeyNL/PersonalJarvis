BEGIN TRANSACTION;
-- Jarvis's memory: short lasting facts about the owner that Jarvis saved
-- (or, later, the owner added). Core refuses credentials, markup and links
-- before a write; the asserts here bound the shape if that check regressed.
-- A fact keeps its source chat id after that chat is deleted (no cascade).
-- Additive: only new, empty tables, so defining their indexes here is safe
-- (see `apply_baseline_schema`).

DEFINE TABLE IF NOT EXISTS memories SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS id ON memories TYPE string ASSERT record::id($value) = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD IF NOT EXISTS user_id ON memories TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 128;
DEFINE FIELD IF NOT EXISTS text ON memories TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 300;
-- The normalised text Core de-duplicates on (lower case, whitespace
-- collapsed); lower-casing can make text longer, hence 400.
DEFINE FIELD IF NOT EXISTS key ON memories TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 400;
-- 1 = not sensitive, 10 = very sensitive. Core stores 10 when a score is
-- missing or invalid, and never sends 9 or 10 to a cloud model.
DEFINE FIELD IF NOT EXISTS sensitivity ON memories TYPE int ASSERT $value >= 1 AND $value <= 10;
DEFINE FIELD IF NOT EXISTS created_by ON memories TYPE string ASSERT $value IN ['jarvis', 'owner'];
DEFINE FIELD IF NOT EXISTS source_conversation_id ON memories TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
-- Every write bumps it; an update names the revision it builds on.
DEFINE FIELD IF NOT EXISTS revision ON memories TYPE int ASSERT $value >= 1;
DEFINE FIELD IF NOT EXISTS created_at ON memories TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON memories TYPE datetime;
DEFINE INDEX IF NOT EXISTS memories_id ON memories FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS memories_user_key ON memories FIELDS user_id, key UNIQUE;
DEFINE INDEX IF NOT EXISTS memories_user_updated ON memories FIELDS user_id, updated_at;

-- One row, `memory_guards:create`, that every fact create writes first in
-- its transaction (like `crypto_wallet_guards`): two creates at the same
-- time write the same key and only one commits, so both cannot pass the
-- 1000-fact check on the same count.
DEFINE TABLE IF NOT EXISTS memory_guards SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS touched_at ON memory_guards TYPE datetime;

UPDATE schema_version:baseline SET version = 25, applied_at = time::now();
COMMIT TRANSACTION;
