-- Durable budget reservations prevent independently scheduled long-running
-- tasks from each assuming the same remaining monthly budget.  Expiry is
-- enforced by callers as well as cleanup; records retain a small audit trail.
BEGIN TRANSACTION;

DEFINE TABLE IF NOT EXISTS llm_budget_reservations SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON llm_budget_reservations TYPE string;
DEFINE FIELD task_id ON llm_budget_reservations TYPE string;
DEFINE FIELD user_id ON llm_budget_reservations TYPE option<string>;
DEFINE FIELD projected_cents ON llm_budget_reservations TYPE int;
DEFINE FIELD status ON llm_budget_reservations TYPE string ASSERT $value IN ['active', 'released', 'expired'];
DEFINE FIELD created_at ON llm_budget_reservations TYPE datetime;
DEFINE FIELD expires_at ON llm_budget_reservations TYPE datetime;
DEFINE FIELD released_at ON llm_budget_reservations TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS llm_budget_reservation_task_unique ON llm_budget_reservations FIELDS task_id UNIQUE;
DEFINE INDEX IF NOT EXISTS llm_budget_reservation_status_expiry ON llm_budget_reservations FIELDS status, expires_at;

UPDATE schema_version:baseline SET version = 3, applied_at = time::now();
COMMIT TRANSACTION;
