-- Non-content routing telemetry. No prompts, answers, credentials or device
-- signatures belong here; this table only supports owner cost/health diagnosis.
BEGIN TRANSACTION;

DEFINE FIELD IF NOT EXISTS request_id ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS routing_mode ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS quality_tier ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS agent_id ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS latency_ms ON llm_usage TYPE option<int>;
DEFINE FIELD IF NOT EXISTS status ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS failure_category ON llm_usage TYPE option<string>;
DEFINE FIELD IF NOT EXISTS fallback_count ON llm_usage TYPE option<int>;
DEFINE INDEX IF NOT EXISTS llm_usage_request_idx ON llm_usage FIELDS request_id;
DEFINE INDEX IF NOT EXISTS llm_usage_status_idx ON llm_usage FIELDS status, ts;

UPDATE schema_version:baseline SET version = 4, applied_at = time::now();
COMMIT TRANSACTION;
