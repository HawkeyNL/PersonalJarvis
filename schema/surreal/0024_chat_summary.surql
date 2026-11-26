BEGIN TRANSACTION;
-- Long-chat summary: a model-written summary of a conversation's turns that
-- the chat context no longer shows. Core writes it only forward (a newer
-- `summary_through` never goes back) and only while the conversation exists;
-- deleting the conversation deletes it with the row. App routes name their
-- fields, so the summary never reaches the app API.
-- Additive: three new optional fields on conversations; rows stored before
-- 0024 stay valid as they are. No index is defined or removed and no row is
-- written (see `apply_baseline_schema`).

DEFINE FIELD IF NOT EXISTS history_summary ON conversations TYPE option<string> ASSERT $value = NONE OR string::len($value) <= 2000;
-- The `created_at` of the newest message the summary covers.
DEFINE FIELD IF NOT EXISTS summary_through ON conversations TYPE option<datetime>;
-- True once any message the summary was made from was untrusted (web text,
-- a page read, a recalled chat, a job answer), and it stays true: each new
-- summary is made from the previous one.
DEFINE FIELD IF NOT EXISTS summary_untrusted ON conversations TYPE option<bool>;

UPDATE schema_version:baseline SET version = 24, applied_at = time::now();
COMMIT TRANSACTION;
