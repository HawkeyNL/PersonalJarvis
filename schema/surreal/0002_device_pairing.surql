-- Device pairing is deliberately separate from active devices: a candidate key
-- has no authentication authority until an existing active device consumes a
-- request-bound, domain-separated Ed25519 approval.
BEGIN TRANSACTION;

DEFINE TABLE IF NOT EXISTS device_pairing_requests SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON device_pairing_requests TYPE string;
DEFINE FIELD user_id ON device_pairing_requests TYPE string;
DEFINE FIELD candidate_name ON device_pairing_requests TYPE string;
DEFINE FIELD candidate_platform ON device_pairing_requests TYPE string ASSERT $value IN ['macos', 'ios', 'windows', 'linux', 'android'];
DEFINE FIELD candidate_public_key ON device_pairing_requests TYPE bytes;
DEFINE FIELD candidate_fingerprint ON device_pairing_requests TYPE string;
DEFINE FIELD nonce ON device_pairing_requests TYPE bytes;
DEFINE FIELD status ON device_pairing_requests TYPE string ASSERT $value IN ['pending', 'approved', 'denied', 'expired'];
DEFINE FIELD created_at ON device_pairing_requests TYPE datetime;
DEFINE FIELD expires_at ON device_pairing_requests TYPE datetime;
DEFINE FIELD resolved_at ON device_pairing_requests TYPE option<datetime>;
DEFINE FIELD approved_by_device_id ON device_pairing_requests TYPE option<string>;
DEFINE FIELD activated_device_id ON device_pairing_requests TYPE option<string>;
DEFINE INDEX IF NOT EXISTS device_pairing_requests_id_unique ON device_pairing_requests FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS device_pairing_requests_user_status_idx ON device_pairing_requests FIELDS user_id, status;
DEFINE INDEX IF NOT EXISTS device_pairing_requests_fingerprint_status_idx ON device_pairing_requests FIELDS candidate_fingerprint, status;

-- A singleton claim makes first bootstrap permanently one-shot even across
-- restarts. Existing installations remain closed because Core also checks for
-- active trusted devices before claiming it.
DEFINE TABLE IF NOT EXISTS bootstrap_state SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON bootstrap_state TYPE string;
DEFINE FIELD claimed_at ON bootstrap_state TYPE datetime;
DEFINE FIELD device_id ON bootstrap_state TYPE string;
DEFINE INDEX IF NOT EXISTS bootstrap_state_id_unique ON bootstrap_state FIELDS id UNIQUE;

UPDATE schema_version:baseline SET version = 2, applied_at = time::now();
COMMIT TRANSACTION;
