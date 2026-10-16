-- Native push targets: at most one per device. A row carries only the APNs
-- device token and environment; no message content is ever stored. Device
-- revocation deletes the row in the same transaction, and the sender only
-- reads rows of active devices.
BEGIN TRANSACTION;
DEFINE TABLE IF NOT EXISTS device_push_targets SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON device_push_targets TYPE string;
DEFINE FIELD device_id ON device_push_targets TYPE string;
DEFINE FIELD kind ON device_push_targets TYPE string ASSERT $value IN ['apns'];
DEFINE FIELD target ON device_push_targets TYPE string ASSERT string::len($value) >= 64 AND string::len($value) <= 200;
DEFINE FIELD apns_env ON device_push_targets TYPE string ASSERT $value IN ['sandbox', 'production'];
DEFINE FIELD updated_at ON device_push_targets TYPE datetime;
DEFINE INDEX IF NOT EXISTS device_push_targets_device ON device_push_targets FIELDS device_id UNIQUE;
DEFINE INDEX IF NOT EXISTS device_push_targets_target ON device_push_targets FIELDS target UNIQUE;
UPDATE schema_version:baseline SET version = 12, applied_at = time::now();
COMMIT TRANSACTION;
