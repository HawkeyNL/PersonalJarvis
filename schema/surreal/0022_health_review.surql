BEGIN TRANSACTION;
-- The morning check: a `health_review` job kind next to the others, at most
-- one per node like `dependency_review` (Core keeps that rule in its store).
-- It takes only `params.roadmap_id`, which exists since 0019. Additive: rows
-- stored before 0022 stay valid as they are. No index is defined or removed
-- and no row of a populated table is written (see `apply_baseline_schema`).

DEFINE FIELD OVERWRITE kind ON scheduled_jobs TYPE string ASSERT $value IN ['dependency_review', 'prompt', 'reminder', 'disk_check', 'health_review'];
DEFINE FIELD OVERWRITE kind ON scheduled_job_runs TYPE string ASSERT $value IN ['dependency_review', 'prompt', 'reminder', 'disk_check', 'health_review'];

-- One row, `scheduled_job_guards:create`, that every job create writes first
-- in its transaction: two creates at the same time then write the same key,
-- and the database lets only one of them commit, so the room rules read in
-- each transaction cannot both pass on the same count.
DEFINE TABLE IF NOT EXISTS scheduled_job_guards SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD IF NOT EXISTS touched_at ON scheduled_job_guards TYPE datetime;

UPDATE schema_version:baseline SET version = 22, applied_at = time::now();
COMMIT TRANSACTION;
