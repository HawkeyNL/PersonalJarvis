BEGIN TRANSACTION;

DEFINE TABLE IF NOT EXISTS owner_brain_preferences SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS id ON owner_brain_preferences TYPE string;
DEFINE FIELD IF NOT EXISTS user_id ON owner_brain_preferences TYPE string;
DEFINE FIELD IF NOT EXISTS provider ON owner_brain_preferences TYPE option<string>;
DEFINE FIELD IF NOT EXISTS model ON owner_brain_preferences TYPE option<string>;
DEFINE FIELD IF NOT EXISTS updated_at ON owner_brain_preferences TYPE datetime;
DEFINE INDEX IF NOT EXISTS owner_brain_preferences_user_unique ON owner_brain_preferences FIELDS user_id UNIQUE;

DEFINE FIELD IF NOT EXISTS brain_provider ON conversations TYPE option<string>;
DEFINE FIELD IF NOT EXISTS brain_model ON conversations TYPE option<string>;

UPDATE schema_version:baseline SET version = 5, applied_at = time::now();
COMMIT TRANSACTION;
