-- Forgotten-password recovery: a device-signed request that only takes effect
-- after local root confirmation on the Home Node. `verifier` holds the new
-- password's Argon2 verifier while pending and is cleared once resolved.
-- `slot` is the owner id while pending and the record id afterwards, so the
-- UNIQUE index allows at most one pending recovery per owner.
BEGIN TRANSACTION;
DEFINE FIELD OVERWRITE action ON account_actions TYPE string ASSERT $value IN ['password-set', 'device-revoke', 'password-reset'];

DEFINE TABLE IF NOT EXISTS account_recoveries SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON account_recoveries TYPE string;
DEFINE FIELD user_id ON account_recoveries TYPE string;
DEFINE FIELD device_id ON account_recoveries TYPE string;
DEFINE FIELD slot ON account_recoveries TYPE string;
DEFINE FIELD verifier ON account_recoveries TYPE option<string>;
DEFINE FIELD status ON account_recoveries TYPE string ASSERT $value IN ['pending', 'confirmed', 'denied', 'expired'];
DEFINE FIELD created_at ON account_recoveries TYPE datetime;
DEFINE FIELD expires_at ON account_recoveries TYPE datetime;
DEFINE FIELD resolved_at ON account_recoveries TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS account_recoveries_id ON account_recoveries FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS account_recoveries_slot ON account_recoveries FIELDS slot UNIQUE;
DEFINE INDEX IF NOT EXISTS account_recoveries_owner ON account_recoveries FIELDS user_id;
UPDATE schema_version:baseline SET version = 11, applied_at = time::now();
COMMIT TRANSACTION;
