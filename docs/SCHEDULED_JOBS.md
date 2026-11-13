# Scheduled jobs

Core runs some work by itself on a schedule the owner sets in the app: a
job runs once, daily, weekly or monthly at a wall-clock minute in
Europe/Amsterdam. Jobs and their runs live in schemas 0019 and 0021
(`scheduled_jobs`, `scheduled_job_runs`) and are served through `/v1/jobs`.

A job's kind is a closed list, each with typed parameters:

| Kind | Does | Params |
| --- | --- | --- |
| `dependency_review` | Reads Dependabot alerts and pull requests (below) | `roadmap_id?` |
| `prompt` | A scheduled research run: a web search for the instruction | `title`, `instruction`, `notify?` (default true) |
| `reminder` | Posts the owner's fixed text and pushes the title | `title`, `text?` |
| `disk_check` | Notes when a Home Node disk is fuller than the threshold | `threshold_percent?` (50 to 99, default 80) |

There is at most one `dependency_review` and at most 10 jobs of the other
kinds made by the owner. Jarvis may keep up to 3 `prompt` jobs of its own
apart from those; Core makes them itself (never through the API), and each
expires 30 days after it was made: it no longer runs or counts towards the
3, and the runner deletes it with its runs on its next check. Nothing is
seeded.

Each create, change and delete through the API writes a security audit row
(`jobs.created`, `jobs.updated`, `jobs.deleted`) with the device and the job
id, never the job's text.

## API

Every route needs an owner session (`Authorization: Bearer …`), like the rest
of the API. Request bodies are capped at 4 KiB and unknown fields are
refused. Each database call is bounded by 10 seconds (503 when it fails).

| Method | Path | Does |
| --- | --- | --- |
| GET | `/v1/jobs` | `status` (`ok` or `not_configured`), `time_zone` and the `jobs` |
| POST | `/v1/jobs` | Create: `{kind, cadence?, weekday?, month_day?, every?, start_date?, max_runs?, at_minute?, enabled?, params?}` |
| PATCH | `/v1/jobs/{id}?expected_revision=N` | Change any of those but `kind` |
| DELETE | `/v1/jobs/{id}?expected_revision=N` | Delete the job and its runs |
| POST | `/v1/jobs/{id}/run` | Run it now (202), at most once an hour |
| GET | `/v1/jobs/{id}/runs?limit=1..50` | The newest runs (default 20), newest first, without facts |
| GET | `/v1/jobs/runs/{run_id}` | One run with its `facts` |

A job has `id`, `kind`, `cadence`, `weekday`, `month_day`, `every`,
`start_date`, `max_runs`, `at_minute`, `enabled`, `revision`, `params`,
`last_slot_at`, `run_count`, `origin`, `owner_id`, `conversation_id`,
`expires_at`, `failure_count`, `paused_reason`, `created_at`, `updated_at`
and `next_run_at` (its next slot; null while it is disabled, expired or of a
kind that does not run yet, or the runner is off).

- `cadence`: `weekly` (default), `daily`, `monthly` or `once`.
- `weekday`: 1 (Monday, default) to 7 (Sunday) for a weekly job; other jobs
  have none. A job that becomes weekly keeps its weekday or gets Monday.
- `month_day`: 1 (default) to 31 for a monthly job; a shorter month runs on
  its last day (31 runs on 28 or 29 February). Other jobs have none.
- `every`: every this many days (1 to 365), weeks (1 to 104) or months (1 to
  24); default 1, always 1 for a one-time job. It falls back to 1 when the
  cadence changes.
- `start_date`: `YYYY-MM-DD` in Europe/Amsterdam, at most two years ahead.
  A one-time job runs on it and needs it; a repeating job runs from it on,
  and `every` counts from it (from the first slot on or after it). With
  `every` above 1 and no start date, Core uses today. Null clears it.
- `max_runs`: 1 to 100; after this many scheduled runs since the schedule
  last changed the job disables itself. A one-time job has none. Null
  clears it.
- `at_minute`: minutes after midnight in Europe/Amsterdam, 0 to 1439
  (default 420, 07:00).
- `enabled`: default true. An enabled one-time job must be in the future.
- `params`: the kind's fields only (table above). `roadmap_id` is a roadmap
  that exists and is not archived. `title` is 1 to 60 characters,
  `instruction` and `text` 1 to 500, each on one line without hidden
  characters. A `prompt` instruction leaves the Home Node in a web search,
  so it may not carry personal or financial details or a symbol the owner
  holds (the research privacy gate), and neither may its title, which can
  leave in a push; when the holdings cannot be read the answer is 503.

These are Core's, never the client's (a body that names one is refused):

- `origin`: `owner`, or `jarvis` for a job Jarvis made for itself.
- `owner_id`: the owner's user id; `conversation_id`: the conversation the
  job's results go to.
- `expires_at`: when a job of Jarvis's goes away; null for the owner's.
- `run_count`: scheduled runs since the schedule last changed.
- `failure_count` and `paused_reason` (`paused_after_failures`): failed runs
  in a row, and why Core disabled the job. Enabling the job clears both.

Examples:

```json
{"kind": "prompt", "cadence": "weekly", "weekday": 1, "at_minute": 480,
 "params": {"title": "AI news", "instruction": "Summarize this week's AI news"}}
{"kind": "reminder", "cadence": "once", "start_date": "2026-12-01", "at_minute": 540,
 "params": {"title": "Call the bank", "text": "Ask about the new card."}}
{"kind": "disk_check", "cadence": "daily", "at_minute": 360,
 "params": {"threshold_percent": 85}}
{"kind": "prompt", "cadence": "monthly", "month_day": 1, "every": 6,
 "start_date": "2027-01-01", "max_runs": 2,
 "params": {"title": "Energy prices", "instruction": "Dutch energy price outlook", "notify": false}}
```

Errors are `{"error": "…"}`: 400 for invalid input (the message says which
rule), 404 for an unknown job or run, 409 for a revision conflict, a second
`dependency_review`, no room for another job, a run request while the runner
is off, 413 for a body over 4 KiB, 422
for a body that does not parse (an unknown field), 429 for a run request
within an hour of the last one or while one is queued or running, 503 when
the database or the holdings cannot be read.

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
its schedule (cadence, weekday, month day, `every`, start date, `max_runs`,
time or `enabled`), sets it to the newest slot under the new schedule, so no
slot before the change runs, and sets `run_count` back to 0.

A one-time job disables itself after its slot, whether it ran or was
missed; a job with `max_runs` after the scheduled run that reaches it
(missed slots and the owner's runs do not count). Core then raises the
revision, so an app editing the old one reloads.

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
between wins. A run's work is bounded by 15 minutes (`timed_out`). Jobs that
reached `max_runs` or expired are skipped; expired jobs of Jarvis's are
deleted, with their runs, at the start of each check.

Due jobs run one at a time, reminders and disk checks before research and
dependency reviews, so a quick job is not held up by searches.

A run of an enabled chat-made job (`prompt`, `reminder`, `disk_check`) that
fails counts towards a streak (`failure_count`); one that succeeds ends it. The
third failed run in a row disables the job with `paused_reason:
"paused_after_failures"` at the next revision; the owner enables it again
(which clears both). A `dependency_review` has no streak. Each check
first marks runs still `running` as `interrupted` (no run is between two
checks, so such a run was left by an earlier Core or its end could not be
recorded), and runs nothing until that succeeded.

The schedule needs the Europe/Amsterdam zone from the system time zone
database (`/usr/share/zoneinfo`). When it cannot be read, Core logs it, the
runner stays off and `GET /v1/jobs` reports `status: "not_configured"`; jobs
can still be edited.

The owner may run a job now (also a disabled one) at most once an hour, and
one at a time; an interrupted run counts. A request stays queued while the
database is unavailable. Its row has `trigger: "owner"` and the time it
started as its `slot_at`; it does not change `last_slot_at`.

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

## Chat-made kinds

A run of these kinds can only write one assistant message to the job's own
conversation and send one push; it has no tools, memory, portfolio or
other access. The conversation is made on the first run that writes,
titled after the job, and kept in `conversation_id`; when the owner deletes
it, the next run makes a new one. Every message is announced with
`conversation.updated`.

A `prompt` run checks the privacy gate again first (title and instruction,
against the holdings at that moment; it fails closed with
`holdings_unavailable` when they cannot be read) and needs Research on (a
job Jarvis made for itself also needs automatic web search on), then runs
one web search
through the fixed Research path: the hard-tier subscription worker with
web search only, never a paid API, metered with `routing_mode` `job`. The
question is the instruction after `(Scheduled run, Monday 7 December
2026.) `. The search is bounded by 5 minutes. The answer goes to the
conversation as `web:<model>`, so later chats treat it as untrusted web
data; the run's `summary` is its first 2000 characters (`summary_source:
"web"`) and its `facts` are `{conversation_id, message_id, sources}` (up to
20 https links from the answer). With `params.notify` (the default) the
owner gets a push without content. A failed run leaves a short fixed notice
in the conversation and has one of these codes; it is not retried before
its next slot:

| Code | Means |
| --- | --- |
| `private_instruction` | The title or instruction may hold personal or financial details or a held symbol |
| `holdings_unavailable` | The holdings could not be read for the privacy gate |
| `not_configured` | Research is off or no subscription worker can run it |
| `research_unavailable` | The search did not run or was stopped |
| `timed_out` | The search took over 5 minutes |
| `conversation_unavailable` | The answer could not be stored |

A `reminder` run involves no model: it posts `params.text` (or the title)
to the conversation and pushes the title (summary: the text,
`deterministic`).

A `disk_check` run reads the Home Node disks the Health page reports
(`system` and `data`, by label, never a path) and changes nothing on the
host. Only when a disk is at or above `params.threshold_percent` does it
post `Disk data is at 91% (threshold 80%).` and send one push naming every
full disk (`Disk data at 91%`). The disks are read off Core's async threads
within 2 seconds.
Its `facts` hold the threshold and each disk's `used_percent`; with no
disk readable it fails with `disk_unavailable`.

## Dependency review

The `dependency_review` job reads what Dependabot knows about the
repositories in `JARVIS_GITHUB_REPOS`, with the roadmap poller's read-only
token, client and limits (see `ROADMAPS.md`): per repository one page of 30
open Dependabot alerts (`GET /repos/{repo}/dependabot/alerts?state=open`)
and one page of 15 open pull requests by `dependabot[bot]` (the issues
list), at most 1 MiB per answer, no redirect to another host. A refused
token or a rate limit stops the reads; the repositories left get the same
code. Without the token or the allowlist the run fails with
`not_configured`.

Its `facts` keep only ids, names, versions and severities, never advisory
or pull request text:

- `alerts`, most severe first and, within a severity, those with a fixed
  version first: `ref` (`a1`, `a2`, …), `repo`, `number`, `package`,
  `ecosystem`, `manifest`, `severity` (`low`, `medium`, `high`,
  `critical`), `ghsa`, `cve`, `vulnerable` (the vulnerable range),
  `patched` (the first fixed version) and `pr` (the `ref` of an open
  Dependabot pull request for the same package). A value outside a strict
  character set is left out, and so is an alert whose package name or
  ecosystem is; `vulnerable` holds only comparisons and versions.
- `pull_requests`: `ref` (`p1`, …), `repo`, `number` and, from the title,
  `package`, `from` and `to`, or `group` and `updates`.
- `errors`: `{repo, error_code}` for each repository that could not be read.
- `truncated`: a page was full, an alert was left out, or the facts were
  cut to 64 KiB (least severe alerts first, then pull requests).

A run is `ok` when every repository was read, `partial` when some
repository's alerts were (a repository whose pull request list fails keeps
its alerts) and `failed` when none were; `error_code` is the first
repository's code:

| Code | Means |
| --- | --- |
| `token_needs_dependabot_alerts_read` | GitHub refused the alerts (403 or 404): give the token the "Dependabot alerts: Read-only" permission for the repository and enable Dependabot alerts in its settings |
| `github_access_refused` | GitHub refused the pull request list; the repository's alerts are kept (a 404 or 410 on the list counts as no pull requests, without a code) |
| `github_token_refused` | GitHub refused the token (401) |
| `github_rate_limited` | GitHub's rate limit |
| `github_answer_too_large` | An answer over 1 MiB |
| `github_redirect_refused` | A redirect to another host |
| `github_unavailable` | Anything else: unreachable, timed out, an error answer |

Then the run puts the facts in order with one call to the Claude
subscription worker, the typed task `dependency_review` (see
`SUBSCRIPTION_WORKERS.md`): its fixed prompt, no tools, the hard model,
sharing the worker's one background slot with research and the memecoin
tasks, never the router or a paid API. The task's input is the facts cut to
16 KiB, least severe alerts first, and is data only. Its answer must be
exactly `{"priorities": [{"ref", "rank", "reason"}], "summary"}`: at most 20
priorities ranked 1, 2, 3 … without gaps, each `ref` one the call was given
and at most once, one-line reasons of at most 300 characters and a summary
of at most 2000; anything else is refused as a whole. Then, and when the
worker is busy, down or out of plan, or there is nothing to order, Core
writes its own: alerts with a fixed version first, then by severity, up to
20, each with a templated reason, and a templated summary
(`summary_source: "deterministic"`). An answer without priorities while
alerts are open is refused too. A failed run has neither. The answer is
display only and never feeds a later call: `summary` and `reason` are plain
text, which apps never render as Markdown or HTML.

With `params.roadmap_id`, a run that read anything also proposes the
upgrades to that roadmap, as one roadmap proposal per run (see
`ROADMAPS.md`; the owner accepts or rejects it, nothing changes on its
own). It is made by Core, not the model: one new item per alert with a
fixed version, most severe first, titled `Upgrade {package} to {version}
({repo})` and linked to the alert's single-package Dependabot pull request
when there is one, so the GitHub poller moves it when that pull request
merges. An upgrade whose title (in any case) or pull request is already on
the roadmap, in a pending proposal or earlier in this one is left out (so a
second alert for the same pull request adds nothing), and so is one whose
title would be over 200 characters; at most 20 items, and no more than the
roadmap has room for. When nothing is left, no proposal is made. A pending
proposal that went stale still counts until the owner accepts it (409,
marked stale), rejects it or it expires; the next run proposes again. A
proposal that fails makes the run `partial` with one of these codes (unless
it already had one):

| Code | Means |
| --- | --- |
| `roadmap_unavailable` | The roadmap is gone or archived |
| `roadmap_changed` | The roadmap changed while the run read it; the next run proposes again |
| `roadmap_limit` | The roadmap holds its maximum of items or pending proposals |
| `roadmap_proposal_failed` | Anything else |

