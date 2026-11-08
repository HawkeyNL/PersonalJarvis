# Roadmaps

Core keeps the owner's roadmaps: ordered plans with items, an audit of every
change and the changes Jarvis proposes. They live in schema 0018
(`roadmaps`, `roadmap_items`, `roadmap_changes`, `roadmap_proposals`) and are
served through `/v1/roadmaps`. Nothing is seeded; every roadmap is the
owner's.

Owner edits always win. Changes that do not come from the owner are either
applied by deterministic code (the GitHub poller, planned) or proposed and
applied only when the owner accepts them (Jarvis).

## API

Every route needs an owner session (`Authorization: Bearer …`), like the rest
of the API. Request bodies are capped at 64 KiB and unknown fields are
refused. Each database call is bounded by 10 seconds (503 when it fails).

| Method | Path | Does |
| --- | --- | --- |
| GET | `/v1/roadmaps[?archived=true]` | The roadmaps that are not archived, oldest first; or the 100 most recently archived |
| POST | `/v1/roadmaps` | Create: `{title, description?, phases?}` |
| GET | `/v1/roadmaps/{id}` | The roadmap with its items (also when archived) |
| PATCH | `/v1/roadmaps/{id}?expected_revision=N` | Change `title`, `description` (`null` clears) or `phases` |
| DELETE | `/v1/roadmaps/{id}?expected_revision=N` | Archive; nothing is deleted |
| POST | `/v1/roadmaps/{id}/items?expected_revision=N` | Add an item; the answer also has `item_id` |
| PATCH | `/v1/roadmaps/{id}/items/{item}?expected_revision=N` | Change an item; absent fields stay, `phase: null` and `notes: null` clear |
| DELETE | `/v1/roadmaps/{id}/items/{item}?expected_revision=N` | Delete an item; items that depended on it no longer do |
| GET | `/v1/roadmaps/{id}/history?limit=1..100` | The newest changes (default 50), newest first |
| GET | `/v1/roadmaps/{id}/proposals` | Pending proposals, oldest first |
| POST | `/v1/roadmaps/{id}/proposals/{pid}/accept` | Apply a proposal |
| POST | `/v1/roadmaps/{id}/proposals/{pid}/reject` | Reject a proposal |

Writes and accepts answer with the whole roadmap: `{roadmap, items}`.
`roadmap` has `id`, `title`, `description`, `phases`, `revision`,
`created_at`, `updated_at` and `archived_at`. An item has `id`, `title`,
`status`, `auto_status`, `phase`, `position`, `notes`, `depends_on`, `links`
and `updated_at`.

Errors are `{"error": "…"}`: 400 for invalid input (the message says which
rule), 404 for an unknown roadmap, item or proposal, 409 for a revision
conflict, an archived roadmap, a stale proposal or a limit, 413 for a body
over 64 KiB.

### Revisions

Every change raises the roadmap's `revision` by one. Every write names the
revision it was made on (`expected_revision`, required); if the roadmap has
moved on, nothing is written and the answer is 409, so the client reloads
and tries again. A write that changes nothing writes nothing and keeps the
revision.

### Items

- `title`: 1 to 200 characters on one line.
- `status`: `planned` (default), `in_progress`, `ready`, `live` or `blocked`.
- `phase`: one of the roadmap's `phases`, or none. Removing a phase from the
  roadmap clears it on its items.
- `position`: 0 to 100000, sorted ascending; a new item goes last by default.
- `notes`: 1 to 4000 characters, line breaks allowed.
- `depends_on`: up to 20 other items of the same roadmap, without cycles.
- `links`: up to 10, stored as given:
  - `{"kind": "pr" | "issue", "repo": "owner/name", "number": N}`
  - `{"kind": "release", "repo": "owner/name", "tag": "v1.2.3"}`
  - `{"kind": "url", "url": "https://…"}` (https only, no credentials, at most
    500 characters)
  - `state` is the GitHub state the poller cached. Clients cannot set it; a
    link that stays on the item keeps it.
- `auto_status` is the status the GitHub poller last derived. Owner edits
  never write it.

Roadmap `title` is 1 to 120 characters on one line, `description` 1 to 2000
characters, `phases` at most 20 distinct names of 1 to 60 characters. Text
must not contain control or invisible characters.

### Limits

| What | Limit |
| --- | --- |
| Roadmaps that are not archived | 20 |
| Items per roadmap | 300 |
| Pending proposals per roadmap | 10 |
| Operations per proposal | 20 |
| Proposal lifetime | 14 days |
| History and decided proposals | kept 365 days |

## Audit

Each change writes a `roadmap_changes` row in the same transaction as the
change itself: the new `revision`, `at`, the `author` (`owner`, `jarvis` or
`github`), the `device_id` it came from, the `proposal_id` for Jarvis's
changes, the `action` (`roadmap_created`, `roadmap_updated`,
`roadmap_archived`, `item_created`, `item_updated`, `item_deleted`), the
`item_id`, the title at the time, the changed `fields` and, for a status
change, `from_status` and `to_status`. The schema enforces the attribution:
an owner change has a device, a GitHub change none, and a Jarvis change is
always an accepted proposal accepted from a device.

The hourly audit housekeeping deletes changes older than 365 days, marks
pending proposals past their 14 days as `expired` and deletes proposals
decided more than 365 days ago. Roadmaps and items are never pruned.

## Proposals

A proposal is a list of 1 to 20 operations made against one revision:

```json
{"op": "create_item", "item": {"title": "…", "status": "planned"}}
{"op": "update_item", "item_id": "…", "patch": {"status": "ready"}}
```

`item` and `patch` follow the same rules as the item routes. The
`rationale` (up to 1000 characters) is for display only. Core checks the
operations against the roadmap when the proposal is recorded.

Accepting applies all operations in one transaction as one revision, author
`jarvis`, with the accepting device. If the roadmap changed since the
proposal was made, the proposal becomes `stale`, nothing is applied and the
answer is 409. Rejecting only records the decision.

There is no route that creates a proposal: only Core code does, through
`roadmaps::store::create_proposal`.

## What is next

- **GitHub poller:** periodically, for linked PRs, issues and releases in
  an allowlisted set of repositories, with a read-only token. It caches each
  link's `state` and moves an item's status forward only (PR open →
  `in_progress`; merged or issue completed → `ready`; release containing the
  merge → `live`), as author `github`. An owner's status edit wins. The repo
  allowlist comes with it; until then links are stored as given and never
  fetched.
- **Realtime event:** `roadmap.changed {id, revision}`, once every client
  ignores event types it does not know.
- **Proposals from Jarvis:** a bounded task that reads the roadmap and typed
  GitHub facts and records proposals, never applying them itself.
