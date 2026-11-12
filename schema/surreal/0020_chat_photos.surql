BEGIN TRANSACTION;
-- Chat photos: images the owner attaches to a chat message. The image bytes
-- live as files under Core's state directory, never in the database; a row
-- only records who owns a photo, where it was sent and its sanitized size.
-- Additive: one new table and one new optional field on chat_messages.

-- `conversation_id` is NONE while a photo is uploaded but not yet sent; it is
-- set once, inside the transaction that stores the message that carries it.
-- `saved` photos are kept until the owner deletes them; unsaved ones expire.
DEFINE TABLE IF NOT EXISTS chat_photos SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON chat_photos TYPE string ASSERT record::id($value) = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD user_id ON chat_photos TYPE string;
DEFINE FIELD conversation_id ON chat_photos TYPE option<string>;
DEFINE FIELD saved ON chat_photos TYPE bool;
DEFINE FIELD width ON chat_photos TYPE int ASSERT $value >= 1 AND $value <= 1600;
DEFINE FIELD height ON chat_photos TYPE int ASSERT $value >= 1 AND $value <= 1600;
DEFINE FIELD bytes ON chat_photos TYPE int ASSERT $value >= 1 AND $value <= 1048576;
DEFINE FIELD created_at ON chat_photos TYPE datetime;
DEFINE INDEX IF NOT EXISTS chat_photos_id ON chat_photos FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS chat_photos_user_created ON chat_photos FIELDS user_id, created_at;
DEFINE INDEX IF NOT EXISTS chat_photos_conversation ON chat_photos FIELDS conversation_id;
DEFINE INDEX IF NOT EXISTS chat_photos_saved_created ON chat_photos FIELDS saved, created_at;

-- The photo ids a message carries, at most four and each once. A message
-- keeps its ids after a photo is deleted; clients show a placeholder.
DEFINE FIELD IF NOT EXISTS attachments ON chat_messages TYPE option<array<string, 4>> ASSERT $value = NONE OR array::len(array::distinct($value)) = array::len($value);
DEFINE FIELD OVERWRITE attachments.* ON chat_messages TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

UPDATE schema_version:baseline SET version = 20, applied_at = time::now();
COMMIT TRANSACTION;
