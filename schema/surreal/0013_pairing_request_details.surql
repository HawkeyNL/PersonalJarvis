BEGIN TRANSACTION;
-- Candidate details shown to the approving owner. They are unauthenticated
-- hints only: none of them is part of the signed pairing approval. Additive:
-- an older Core neither writes nor selects these fields.
DEFINE FIELD IF NOT EXISTS candidate_ip ON device_pairing_requests TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 45;
DEFINE FIELD IF NOT EXISTS candidate_user_agent ON device_pairing_requests TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 256;
DEFINE FIELD IF NOT EXISTS candidate_app_version ON device_pairing_requests TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 32;
DEFINE FIELD IF NOT EXISTS candidate_model ON device_pairing_requests TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 64;
UPDATE schema_version:baseline SET version = 13, applied_at = time::now();
COMMIT TRANSACTION;
