BEGIN TRANSACTION;
-- Scheduled jobs: work Core runs by itself on a daily or weekly schedule,
-- and a record of every run. A job's kind is a closed list; there are no
-- free-text jobs. Core writes them only through typed store functions;
-- nothing is seeded. Additive: no existing table changes.

-- One job per kind. Times are Europe/Amsterdam wall-clock minutes; a weekly
-- job has an ISO weekday (Monday = 1). `revision` grows by one on every owner
-- change and guards concurrent edits. `last_slot_at` is the newest slot the
-- runner handled (ran or recorded as missed); NONE after a schedule change.
DEFINE TABLE IF NOT EXISTS scheduled_jobs SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON scheduled_jobs TYPE string;
DEFINE FIELD kind ON scheduled_jobs TYPE string ASSERT $value IN ['dependency_review'];
DEFINE FIELD cadence ON scheduled_jobs TYPE string ASSERT $value IN ['daily', 'weekly'] AND ($value = 'weekly') = (weekday != NONE);
DEFINE FIELD weekday ON scheduled_jobs TYPE option<int> ASSERT $value = NONE OR ($value >= 1 AND $value <= 7);
DEFINE FIELD at_minute ON scheduled_jobs TYPE int ASSERT $value >= 0 AND $value <= 1439;
DEFINE FIELD enabled ON scheduled_jobs TYPE bool;
DEFINE FIELD revision ON scheduled_jobs TYPE int ASSERT $value >= 1;
DEFINE FIELD params ON scheduled_jobs TYPE object;
DEFINE FIELD params.roadmap_id ON scheduled_jobs TYPE option<string> ASSERT $value = NONE OR $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD last_slot_at ON scheduled_jobs TYPE option<datetime>;
DEFINE FIELD created_at ON scheduled_jobs TYPE datetime;
DEFINE FIELD updated_at ON scheduled_jobs TYPE datetime;
DEFINE INDEX IF NOT EXISTS scheduled_jobs_id ON scheduled_jobs FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS scheduled_jobs_kind ON scheduled_jobs FIELDS kind UNIQUE;

-- One row per run, written before the run starts. A scheduled run has the
-- slot it ran for; an owner's run the time it was asked for. `facts` is the
-- JSON the run collected, `priorities` point into it by `ref`, `summary` is
-- display only and comes with its source. Pruned after 180 days.
DEFINE TABLE IF NOT EXISTS scheduled_job_runs SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD id ON scheduled_job_runs TYPE string;
DEFINE FIELD job_id ON scheduled_job_runs TYPE string ASSERT $value = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
DEFINE FIELD kind ON scheduled_job_runs TYPE string ASSERT $value IN ['dependency_review'];
DEFINE FIELD slot_at ON scheduled_job_runs TYPE datetime;
DEFINE FIELD trigger ON scheduled_job_runs TYPE string ASSERT $value IN ['schedule', 'owner'];
DEFINE FIELD status ON scheduled_job_runs TYPE string ASSERT $value IN ['running', 'ok', 'partial', 'failed', 'missed', 'interrupted'] AND ($value = 'running') = (finished_at = NONE) AND (summary = NONE) = (summary_source = NONE);
DEFINE FIELD error_code ON scheduled_job_runs TYPE option<string> ASSERT $value = NONE OR $value = /^[a-z][a-z_]{0,47}$/;
DEFINE FIELD facts ON scheduled_job_runs TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 2 AND bytes::len(<bytes>$value) <= 65536);
DEFINE FIELD summary ON scheduled_job_runs TYPE option<string> ASSERT $value = NONE OR (string::len($value) >= 1 AND string::len($value) <= 2000);
DEFINE FIELD summary_source ON scheduled_job_runs TYPE option<string> ASSERT $value = NONE OR $value IN ['claude', 'deterministic'];
DEFINE FIELD priorities ON scheduled_job_runs TYPE array<object, 20>;
DEFINE FIELD priorities.*.ref ON scheduled_job_runs TYPE string ASSERT $value = /^[ap][1-9][0-9]{0,3}$/;
DEFINE FIELD priorities.*.rank ON scheduled_job_runs TYPE int ASSERT $value >= 1 AND $value <= 20;
DEFINE FIELD priorities.*.reason ON scheduled_job_runs TYPE string ASSERT string::len($value) >= 1 AND string::len($value) <= 300;
DEFINE FIELD started_at ON scheduled_job_runs TYPE datetime;
DEFINE FIELD finished_at ON scheduled_job_runs TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS scheduled_job_runs_id ON scheduled_job_runs FIELDS id UNIQUE;
DEFINE INDEX IF NOT EXISTS scheduled_job_runs_slot ON scheduled_job_runs FIELDS job_id, slot_at UNIQUE;
DEFINE INDEX IF NOT EXISTS scheduled_job_runs_job ON scheduled_job_runs FIELDS job_id, started_at;
DEFINE INDEX IF NOT EXISTS scheduled_job_runs_started ON scheduled_job_runs FIELDS started_at;

UPDATE schema_version:baseline SET version = 19, applied_at = time::now();
COMMIT TRANSACTION;
