BEGIN TRANSACTION;
-- Jobs via chat: three new job kinds next to `dependency_review` (`prompt`,
-- `reminder`, `disk_check`), more schedules (one time, monthly, every N
-- days, weeks or months, a run limit), who made a job and when one of
-- Jarvis's own expires, and its failure streak. Additive: every new field is
-- optional and rows stored before 0021 stay valid as they are. One change is
-- not additive: a kind may now have several jobs, so the kind index is no
-- longer unique; Core keeps "one `dependency_review`" in its store.

DEFINE FIELD OVERWRITE kind ON scheduled_jobs TYPE string ASSERT $value IN ['dependency_review', 'prompt', 'reminder', 'disk_check'];
-- `once` runs on `start_date`; `monthly` on `month_day`, or a shorter month's
-- last day.
DEFINE FIELD OVERWRITE cadence ON scheduled_jobs TYPE string ASSERT $value IN ['once', 'daily', 'weekly', 'monthly'] AND ($value = 'weekly') = (weekday != NONE) AND ($value = 'monthly') = (month_day != NONE) AND ($value != 'once' OR (start_date != NONE AND (every = NONE OR every = 1) AND max_runs = NONE));
DEFINE FIELD IF NOT EXISTS month_day ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 31);
-- Every this many days, weeks or months, counted from `start_date`; NONE
-- (rows before 0021) is 1.
DEFINE FIELD IF NOT EXISTS every ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 365 AND ($value = 1 OR start_date != NONE));
-- A civil date in Europe/Amsterdam: the day of a one-time job, or the first
-- day a repeating job may run, from which `every` counts.
DEFINE FIELD IF NOT EXISTS start_date ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9]{4}-[0-9]{2}-[0-9]{2}$/;
-- The job disables itself after this many scheduled runs since its schedule
-- last changed (`run_count`).
DEFINE FIELD IF NOT EXISTS max_runs ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 100);
DEFINE FIELD IF NOT EXISTS run_count ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR $value >= 0;
-- NONE (rows before 0021) is the owner. Jarvis makes only `prompt` jobs of
-- its own, and each expires.
DEFINE FIELD IF NOT EXISTS origin ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR ($value IN ['owner', 'jarvis'] AND ($value = 'owner' OR (kind = 'prompt' AND expires_at != NONE)));
DEFINE FIELD IF NOT EXISTS owner_id ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD IF NOT EXISTS conversation_id ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD IF NOT EXISTS expires_at ON scheduled_jobs TYPE option<datetime>;
-- Failed runs in a row; a paused job is disabled until the owner enables it.
DEFINE FIELD IF NOT EXISTS failure_count ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR $value >= 0;
DEFINE FIELD IF NOT EXISTS paused_reason ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR ($value IN ['paused_after_failures'] AND enabled = false);

-- Typed parameters of the new kinds.
DEFINE FIELD IF NOT EXISTS params.title ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 60);
DEFINE FIELD IF NOT EXISTS params.instruction ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 500);
DEFINE FIELD IF NOT EXISTS params.text ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 500);
DEFINE FIELD IF NOT EXISTS params.notify ON scheduled_jobs TYPE option<bool>;
DEFINE FIELD IF NOT EXISTS params.threshold_percent ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR ($value >= 50 AND $value <= 99);

-- The new index has a new name. SurrealDB 2.x builds an index over existing
-- rows in transactions of its own, outside this one; rebuilding the removed
-- index's name would rewrite the very keys REMOVE INDEX deletes here, and on
-- RocksDB this transaction then fails to commit with a read/write conflict.
REMOVE INDEX IF EXISTS scheduled_jobs_kind ON scheduled_jobs;
DEFINE INDEX IF NOT EXISTS scheduled_jobs_by_kind ON scheduled_jobs FIELDS kind;

DEFINE FIELD OVERWRITE kind ON scheduled_job_runs TYPE string ASSERT $value IN ['dependency_review', 'prompt', 'reminder', 'disk_check'];
-- `web`: a research answer from a web search.
DEFINE FIELD OVERWRITE summary_source ON scheduled_job_runs TYPE option<string> ASSERT $value = NONE OR $value IN ['claude', 'deterministic', 'web'];

UPDATE schema_version:baseline SET version = 21, applied_at = time::now();
COMMIT TRANSACTION;
