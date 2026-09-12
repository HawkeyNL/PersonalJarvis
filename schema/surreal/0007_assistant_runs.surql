BEGIN TRANSACTION;
-- Durable idempotency tombstones are not pruned automatically: losing one
-- could let an old mobile retry repeat a paid generation.
DEFINE TABLE IF NOT EXISTS assistant_runs SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS id ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS user_id ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS device_id ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS request_id ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS conversation_id ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS payload_hash ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS epoch ON assistant_runs TYPE string;
DEFINE FIELD IF NOT EXISTS state ON assistant_runs TYPE string ASSERT $value IN ['running', 'completed', 'failed'];
DEFINE FIELD IF NOT EXISTS created_at ON assistant_runs TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON assistant_runs TYPE datetime;
DEFINE INDEX IF NOT EXISTS assistant_runs_id_unique ON assistant_runs FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS assistant_runs_owner_request ON assistant_runs FIELDS user_id, device_id, request_id UNIQUE;
DEFINE INDEX IF NOT EXISTS assistant_runs_owner ON assistant_runs FIELDS user_id, created_at;
UPDATE schema_version:baseline SET version = 7, applied_at = time::now();
COMMIT TRANSACTION;
