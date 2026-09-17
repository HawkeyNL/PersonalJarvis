-- Backend-only account verifiers. Passwords and verifier strings are never
-- included in user/device DTOs or audit records.
BEGIN TRANSACTION;
DEFINE TABLE IF NOT EXISTS account_passwords SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD user_id ON account_passwords TYPE string;
DEFINE FIELD verifier ON account_passwords TYPE string;
DEFINE FIELD revision ON account_passwords TYPE string;
DEFINE FIELD updated_at ON account_passwords TYPE datetime;
DEFINE INDEX IF NOT EXISTS account_passwords_owner ON account_passwords FIELDS user_id UNIQUE;

DEFINE TABLE IF NOT EXISTS account_actions SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON account_actions TYPE string;
DEFINE FIELD user_id ON account_actions TYPE string;
DEFINE FIELD device_id ON account_actions TYPE string;
DEFINE FIELD action ON account_actions TYPE string ASSERT $value IN ['password-set', 'device-revoke'];
DEFINE FIELD target ON account_actions TYPE string;
DEFINE FIELD nonce ON account_actions TYPE bytes;
DEFINE FIELD verifier ON account_actions TYPE option<string>;
DEFINE FIELD created_at ON account_actions TYPE datetime;
DEFINE FIELD expires_at ON account_actions TYPE datetime;
DEFINE FIELD consumed_at ON account_actions TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS account_actions_id ON account_actions FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS account_actions_owner ON account_actions FIELDS user_id;
DEFINE INDEX IF NOT EXISTS account_actions_device ON account_actions FIELDS user_id, device_id UNIQUE;
UPDATE schema_version:baseline SET version = 8, applied_at = time::now();
COMMIT TRANSACTION;
