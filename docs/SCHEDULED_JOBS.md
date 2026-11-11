# Scheduled jobs

Core runs some work by itself on a schedule the owner sets in the app: a
job runs daily or weekly at a wall-clock minute in Europe/Amsterdam. Jobs
and their runs live in schema 0019 (`scheduled_jobs`, `scheduled_job_runs`)
and are served through `/v1/jobs`.

A job's kind is a closed list. There is no free-text job: the owner picks a
kind and its typed parameters, never a prompt. The only kind is
`dependency_review`. There is at most one job per kind; nothing is seeded.

## API

Every route needs an owner session (`Authorization: Bearer …`), like the rest
of the API. Request bodies are capped at 4 KiB and unknown fields are
refused. Each database call is bounded by 10 seconds (503 when it fails).

| Method | Path | Does |
| --- | --- | --- |
| GET | `/v1/jobs` | `status` (`ok` or `not_configured`), `time_zone` and the `jobs` |
| POST | `/v1/jobs` | Create: `{kind, cadence?, weekday?, at_minute?, enabled?, params?}` |
| PATCH | `/v1/jobs/{id}?expected_revision=N` | Change `cadence`, `weekday`, `at_minute`, `enabled` or `params` |
| DELETE | `/v1/jobs/{id}?expected_revision=N` | Delete the job and its runs |
| POST | `/v1/jobs/{id}/run` | Run it now (202), at most once an hour |
| GET | `/v1/jobs/{id}/runs?limit=1..50` | The newest runs (default 20), newest first, without facts |
| GET | `/v1/jobs/runs/{run_id}` | One run with its `facts` |

A job has `id`, `kind`, `cadence`, `weekday`, `at_minute`, `enabled`,
`revision`, `params`, `last_slot_at`, `created_at`, `updated_at` and
`next_run_at` (its next slot; null while it is disabled or the runner is
off).

- `cadence`: `weekly` (default) or `daily`.
- `weekday`: 1 (Monday, default) to 7 (Sunday) for a weekly job; a daily job
  has none. A job that becomes weekly keeps its weekday or gets Monday.
- `at_minute`: minutes after midnight in Europe/Amsterdam, 0 to 1439
  (default 420, 07:00).
- `enabled`: default true.
- `params.roadmap_id`: optional; a roadmap that exists and is not archived.

Errors are `{"error": "…"}`: 400 for invalid input (the message says which
rule), 404 for an unknown job or run, 409 for a revision conflict, a second
job of the same kind or a run request while the runner is off, 413 for a
body over 4 KiB, 429 for a run request within an hour of the last one or
while one is queued or running.

Every change raises the job's `revision` by one; every write names the
revision it was made on (`expected_revision`, required), and when the job has
moved on, nothing is written and the answer is 409. A change that changes
nothing keeps the revision.

## Schedule

A job's slots are wall-clock minutes in Europe/Amsterdam, so they follow
daylight saving time:

- On the spring-forward day a minute that does not exist (02:00 to 02:59)
  moves forward by the gap: 02:30 runs at 03:30.
- On the fall-back day a minute that occurs twice (02:00 to 02:59) runs
  once, at its first occurrence.

Each slot is handled once: a run row is unique per job and slot, and the job
keeps the newest slot handled (`last_slot_at`). Creating a job, or changing
its cadence, weekday, time or `enabled`, sets it to the newest slot under the
new schedule, so no slot before the change runs.

When Core was down at a slot:

- a slot less than 24 hours late runs once, as soon as Core is back;
- an older one is not run: one `missed` row records it, only for the newest
  missed slot, however many passed.

## Runner

One task in Core checks every minute (and at once when the owner asks for a
run) which slots are due and runs them one at a time: the owner's queued run
first, then each job's due slot. It writes the run row (`running`) before the
work starts, in one transaction with the slot it handles, and only while the
job is still enabled and at the revision it read, so an owner change in
between wins. A run's work is bounded by 15 minutes (`timed_out`). Each check
first marks runs still `running` as `interrupted` (no run is between two
checks, so such a run was left by an earlier Core or its end could not be
recorded), and runs nothing until that succeeded.

The schedule needs the Europe/Amsterdam zone from the system time zone
database (`/usr/share/zoneinfo`). When it cannot be read, Core logs it, the
runner stays off and `GET /v1/jobs` reports `status: "not_configured"`; jobs
can still be edited.

The owner may run a job now (also a disabled one) at most once an hour, and
one at a time; an interrupted run counts. A request stays queued while the
database is unavailable. Its row has `trigger: "owner"` and the time asked as its
`slot_at`; it does not change `last_slot_at`.

## Runs

A run has `id`, `job_id`, `kind`, `slot_at`, `trigger` (`schedule` or
`owner`), `status`, `error_code`, `summary`, `summary_source`, `priorities`,
`started_at`, `finished_at` and, from `GET /v1/jobs/runs/{run_id}`, `facts`.

- `status`: `running`, `ok`, `partial` (some of the work failed; what
  succeeded is kept), `failed`, `missed` or `interrupted`.
- `error_code`: a fixed code, never upstream text: `not_configured`,
  `timed_out`, `interrupted`, `invalid_result` (a result Core refused to
  store), or a code of the job's kind (see below).
- `facts`: the JSON the run collected, at most 64 KiB.
- `summary`: display text, at most 2000 characters; `summary_source` says
  whether Claude or Core's template wrote it.
- `priorities`: up to 20 `{ref, rank, reason}`; `ref` names an entry of the
  run's `facts`.

Runs are deleted after 180 days by the hourly housekeeping.

After every run that ends, and every `missed` row, Core sends the realtime
event `jobs.run_finished {"job_id": "…", "run_id": "…", "status": "…"}` to
every connected app (see `REALTIME_PROTOCOL.md`); the app reloads
`GET /v1/jobs/runs/{run_id}`.

## Dependency review

The `dependency_review` job reads what Dependabot knows about the
repositories in `JARVIS_GITHUB_REPOS`, with the roadmap poller's read-only
token, client and limits (see `ROADMAPS.md`): per repository one page of
open Dependabot alerts (`GET /repos/{repo}/dependabot/alerts?state=open`)
and one page of open pull requests by `dependabot[bot]` (the issues list),
30 each, at most 1 MiB per answer, no redirect to another host. Without the
token or the allowlist the run fails with `not_configured`.

Its `facts` keep only ids, names, versions and severities, never advisory
or pull request text:

- `alerts`, most severe first and, within a severity, those with a fixed
  version first: `ref` (`a1`, `a2`, …), `repo`, `number`, `package`,
  `ecosystem`, `manifest`, `severity` (`low`, `medium`, `high`,
  `critical`), `ghsa`, `cve`, `vulnerable` (the vulnerable range),
  `patched` (the first fixed version) and `pr` (the `ref` of an open
  Dependabot pull request for the same package). A value outside a strict
  character set is left out, and so is an alert whose package name is.
- `pull_requests`: `ref` (`p1`, …), `repo`, `number` and, from the title,
  `package`, `from` and `to`, or `group` and `updates`.
- `errors`: `{repo, error_code}` for each repository that could not be read.
- `truncated`: a page was full, or the facts were cut to 64 KiB (least
  severe alerts first, then pull requests).

A run is `ok` when every repository was read, `partial` when some were and
`failed` when none was; `error_code` is the first repository's code:

| Code | Means |
| --- | --- |
| `token_needs_dependabot_alerts_read` | GitHub refused the alerts (403 or 404): give the token the "Dependabot alerts: Read-only" permission for the repository and enable Dependabot alerts in its settings |
| `github_access_refused` | GitHub refused the pull request list |
| `github_token_refused` | GitHub refused the token (401) |
| `github_rate_limited` | GitHub's rate limit |
| `github_answer_too_large` | An answer over 1 MiB |
| `github_redirect_refused` | A redirect to another host |
| `github_unavailable` | Anything else: unreachable, timed out, an error answer |
