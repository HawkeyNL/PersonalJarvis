BEGIN TRANSACTION;
-- When the password login that a session descends from happened. A password
-- login sets it to now; a biometric renewal copies it from the device's
-- latest session and is refused once it is 30 days old. Additive: older
-- sessions have none and cannot be renewed, and an older Core never writes
-- or selects it.
DEFINE FIELD IF NOT EXISTS password_verified_at ON sessions TYPE option<datetime>;
-- The account password revision that login checked; renewal also requires
-- it to still be the current one.
DEFINE FIELD IF NOT EXISTS password_revision ON sessions TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 64;
UPDATE schema_version:baseline SET version = 16, applied_at = time::now();
COMMIT TRANSACTION;
