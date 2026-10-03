BEGIN TRANSACTION;
-- Subscription execution is a run-slot/runtime budget, never a EUR API charge.
-- A signed UUID alone is not authority: the broker must lease this record.
DEFINE TABLE IF NOT EXISTS coding_reservations SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON coding_reservations TYPE string;
DEFINE FIELD user_id ON coding_reservations TYPE string;
DEFINE FIELD coding_session_id ON coding_reservations TYPE string;
DEFINE FIELD purpose ON coding_reservations TYPE string ASSERT $value = 'codex_coding_run';
DEFINE FIELD compute_class ON coding_reservations TYPE string ASSERT $value = 'subscription';
DEFINE FIELD api_spend_cents ON coding_reservations TYPE int ASSERT $value = 0;
DEFINE FIELD execution_units ON coding_reservations TYPE int ASSERT $value = 1;
DEFINE FIELD max_runtime_secs ON coding_reservations TYPE int ASSERT $value > 0 AND $value <= 3600;
DEFINE FIELD max_provider_turns ON coding_reservations TYPE int ASSERT $value > 0 AND $value <= 32;
DEFINE FIELD status ON coding_reservations TYPE string ASSERT $value IN ['reserved','leased','released','settled'];
DEFINE FIELD run_id ON coding_reservations TYPE option<string>;
DEFINE FIELD created_at ON coding_reservations TYPE datetime;
DEFINE FIELD expires_at ON coding_reservations TYPE datetime;
DEFINE FIELD updated_at ON coding_reservations TYPE datetime;
DEFINE INDEX IF NOT EXISTS coding_reservations_id_unique ON coding_reservations FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS coding_reservations_owner_session ON coding_reservations FIELDS user_id, coding_session_id;
-- A manager sandbox ID is recorded before any upload or execution. Metadata
-- remains an independent recovery key for the create/DB-write crash window.
DEFINE FIELD IF NOT EXISTS sandbox_provider ON coding_runs TYPE option<string>;
DEFINE FIELD IF NOT EXISTS sandbox_id ON coding_runs TYPE option<string>;
DEFINE FIELD IF NOT EXISTS sandbox_state ON coding_runs TYPE option<string> ASSERT $value = NONE OR $value IN ['not_created','active','termination_requested','terminated','cleanup_failed','orphan_recovered'];
DEFINE FIELD IF NOT EXISTS sandbox_created_at ON coding_runs TYPE option<datetime>;
DEFINE FIELD IF NOT EXISTS sandbox_cleanup_status ON coding_runs TYPE option<string>;
-- The resume checkpoint is a structured object. A SCHEMAFULL table strips
-- undeclared nested keys, which stored every checkpoint as {} and denied
-- every resume.
DEFINE FIELD OVERWRITE checkpoint ON coding_sessions FLEXIBLE TYPE option<object>;
UPDATE schema_version:baseline SET version = 10, applied_at = time::now();
COMMIT TRANSACTION;
