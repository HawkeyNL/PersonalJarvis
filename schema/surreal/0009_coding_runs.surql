BEGIN TRANSACTION;
-- Owner constraints belong to the logical session, not a single run. Existing
-- sessions without this field retain the empty-constraint default in Rust.
DEFINE FIELD IF NOT EXISTS owner_constraints ON coding_sessions TYPE option<array<string>>;
-- Runs are durable factual records distinct from logical coding sessions.
-- Capability plaintext and provider authentication never enter this table.
DEFINE TABLE IF NOT EXISTS coding_runs SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON coding_runs TYPE string;
DEFINE FIELD request_id ON coding_runs TYPE string;
DEFINE FIELD coding_session_id ON coding_runs TYPE string;
DEFINE FIELD user_id ON coding_runs TYPE string;
DEFINE FIELD device_id ON coding_runs TYPE string;
DEFINE FIELD repository_id ON coding_runs TYPE string;
DEFINE FIELD repository_owner ON coding_runs TYPE string;
DEFINE FIELD repository_name ON coding_runs TYPE string;
DEFINE FIELD base_sha ON coding_runs TYPE string;
DEFINE FIELD snapshot_sha256 ON coding_runs TYPE option<string>;
DEFINE FIELD reservation_id ON coding_runs TYPE string;
DEFINE FIELD reservation_units ON coding_runs TYPE int ASSERT $value = 1;
DEFINE FIELD reservation_status ON coding_runs TYPE string ASSERT $value IN ['active','released'];
DEFINE FIELD status ON coding_runs TYPE string ASSERT $value IN ['queued','preparing','running','cancelling','completed','failed','timed_out','cancelled'];
DEFINE FIELD summary ON coding_runs TYPE option<string>;
DEFINE FIELD failure_category ON coding_runs TYPE option<string>;
DEFINE FIELD artifacts ON coding_runs TYPE array<string>;
DEFINE FIELD compute_class ON coding_runs TYPE string ASSERT $value = 'subscription';
DEFINE FIELD created_at ON coding_runs TYPE datetime;
DEFINE FIELD updated_at ON coding_runs TYPE datetime;
DEFINE FIELD completed_at ON coding_runs TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS coding_runs_id_unique ON coding_runs FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS coding_runs_request_unique ON coding_runs FIELDS user_id, request_id UNIQUE;
DEFINE INDEX IF NOT EXISTS coding_runs_reservation_unique ON coding_runs FIELDS reservation_id UNIQUE;
DEFINE INDEX IF NOT EXISTS coding_runs_owner_session ON coding_runs FIELDS user_id, coding_session_id;
UPDATE schema_version:baseline SET version = 9, applied_at = time::now();
COMMIT TRANSACTION;
