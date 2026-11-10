# Roadmaps

Core keeps the owner's roadmaps: ordered plans with items, an audit of every
change and the changes Jarvis proposes. They live in schema 0018
(`roadmaps`, `roadmap_items`, `roadmap_changes`, `roadmap_proposals`) and are
served through `/v1/roadmaps`. Nothing is seeded; every roadmap is the
owner's.

Jarvis's language model never changes a roadmap: what it produces is only
ever a proposal, which Core applies only when the owner accepts it from a
device, and only if the roadmap has not changed since the proposal was made.
The only writer besides the owner is deterministic code, the GitHub poller
(see [GitHub](#github)): it only moves an item's status forward, never out
of `blocked`, and an owner's status edit holds until GitHub shows something
new. Every write names the revision it was made on, so no writer, the owner
included, overwrites a change it has not seen.

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
| POST | `/v1/roadmaps/{id}/sync` | Ask the GitHub poller for a pass now (202), at most once a minute |
| GET | `/v1/roadmaps/github` | The GitHub poller: `configured`, `repos`, `state`, `last_poll_at`, `last_error` |

Writes and accepts answer with the whole roadmap: `{roadmap, items}`.
`roadmap` has `id`, `title`, `description`, `phases`, `revision`,
`created_at`, `updated_at` and `archived_at`. An item has `id`, `title`,
`status`, `auto_status`, `phase`, `position`, `notes`, `depends_on`, `links`
and `updated_at`.

Errors are `{"error": "…"}`: 400 for invalid input (the message says which
rule), 404 for an unknown roadmap, item or proposal, 409 for a revision
conflict, an archived roadmap, a stale proposal or a limit, 413 for a body
over 64 KiB, 429 for a second sync within a minute.

### Revisions

Every change raises the roadmap's `revision` by one. Every write names the
revision it was made on (`expected_revision`, required); if the roadmap has
moved on, nothing is written and the answer is 409, so the client reloads
and tries again. A write that changes nothing writes nothing and keeps the
revision.

After every committed write (the owner's, an accepted proposal or the
GitHub poller's) Core sends the realtime event
`roadmap.changed {"id": "…", "revision": N}` to every connected app (see
`REALTIME_PROTOCOL.md`). It carries only the roadmap id and the new
revision; the app reloads `GET /v1/roadmaps/{id}`. A refused, stale or empty
write sends nothing. Two writes close together may arrive out of order, so an
app ignores a `revision` at or below the one it already has.

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
  - Once `JARVIS_GITHUB_REPOS` is set, a new `pr`, `issue` or `release`
    link must name one of its repositories (400 otherwise), also when it
    comes with an accepted proposal (nothing is applied and the proposal
    stays pending). Links already on the item may stay, but are not
    fetched.
  - `state` is the GitHub state the poller cached. Clients cannot set it; a
    link that stays on the item keeps it.
- `auto_status` is the status the item's links stand for, from their
  cached GitHub states. Clients cannot set it; the poller sets it, and an
  edit of `links` derives it again from the links that stay (the item's
  `status` is not changed by that).

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

## GitHub

Core can follow the PRs, issues and releases that items link to and move
those items forward by itself. It polls GitHub every 10 minutes (there is
no webhook) with a read-only token the owner provides. It is off until both
the token and the repo allowlist are there.

### What it does

For each repository in the allowlist that a roadmap links to, a pass reads:

- `GET /repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page=30`,
  after the first pass only what changed since the previous one (`since`,
  with 5 minutes of overlap). This list holds pull requests as well as
  issues. The pull request list itself is not used: 50 pull requests with
  their bodies come to more than the 1 MiB answer limit. The window moves on
  only when every linked item it covers is stored; after a failed or
  conflicting write the next pass reads the same window again. A list too
  large to read leaves each linked item to a single fetch.
- `GET /repos/{repo}/releases?per_page=10`.
- One by one, and only when the lists cannot answer: a linked PR or issue
  that is not in the window or that links disagree on (`/pulls/{n}`,
  `/issues/{n}`; 404 or 410 is `not_found`), a linked release
  that is not among the latest ten (`/releases/tags/{tag}`), and once per
  merged PR its merge commit.
- `GET /repos/{repo}/compare/{tag}...{merge commit}` to see whether a
  published release contains a merged PR. The answer is cached.

At most 100 single fetches per pass; the rest waits for the next pass.
Every request has a 15 second timeout and an answer of at most 1 MiB, uses
`If-None-Match` with the ETag of the previous answer where the address does
not change, and goes only to `https://api.github.com`: a redirect is
followed only to that same origin, so the token never reaches another
host. No proxy is used.

Each link gets a cached `state`:

| Link | `state` | Item status it stands for |
| --- | --- | --- |
| `pr` | `open`, `draft` | `in_progress` |
| `pr` | `merged` | `ready` |
| `pr` | `released` (a published release contains the merge) | `live` |
| `issue` | `completed` (closed as completed) | `ready` |
| `release` | `published` | `live` |
| `pr`, `issue` | `closed`, `not_found`; issue `open` | badge only |
| `release` | `unreleased`, `prerelease` | badge only |

An item stands for the most advanced status among its links. When a pass
changes a link's state so that the item stands for a more advanced status
than before, and the item is behind that status and not `blocked`, the
status moves forward to it. It never moves back, never leaves `blocked`,
and an owner's status edit holds until GitHub shows something new (a PR
that stays open does not undo the owner moving the item back to `planned`;
the same PR being merged later moves it to `ready`). `auto_status` is set to
the status the links stand for.

Each roadmap with changed links is written in one revision, author
`github`, without a device, like any other write: when the owner changed
the roadmap in the meantime, nothing is written and the next pass builds
on the new revision. Every such write raises the revision, also when only a
badge changed (for example a PR closed without merging), so a client
editing at that moment gets a 409 and reloads, and a pending Jarvis
proposal becomes stale.

### What it never does

- Write anything to GitHub: it only sends `GET` requests, with a read-only
  token.
- Fetch a `url` link, a repository outside the allowlist or any host but
  `api.github.com`.
- Log the token or put it in Core's environment: it is only sent in the
  `Authorization` header to `api.github.com`. Child processes do not get
  it through the environment, but any process running as Core's user can
  read the credential file.
- Move an item backwards or out of `blocked`, or override an owner's edit
  without a new GitHub fact.

### Failures

- A timeout, a 5xx answer, an answer over 1 MiB or a repository the token
  cannot read: the pass keeps the last known states and `state` is `error`
  with a fixed `last_error`. When only some repositories or roadmaps
  failed, the others are stored and the next pass comes on time; when the
  whole pass failed (every linked repository, or loading the roadmaps), the
  next one waits 20, 40 and then 60 minutes until one succeeds.
- Rate limited (403 or 429 with `Retry-After` or an exhausted
  `X-RateLimit-Remaining`): `state` is `rate_limited` and the next pass
  waits until GitHub's reset, at least 10 minutes and at most an hour.
- 401 (wrong, expired or revoked token): `state` is `auth_failed` and the
  poller stops until Core restarts. Cached states stay.
- A missing token or allowlist: `state` is `not_configured`; an invalid
  one is logged and shown in `last_error`. Core always starts.

`POST /v1/roadmaps/{id}/sync` answers 409 when the poller is not configured,
could not start or stopped on a 401, and 429 within a minute of the previous sync. During a
rate limit the sync waits for the limit.

### Owner setup

1. Create a fine-grained personal access token on GitHub (Settings →
   Developer settings → Fine-grained tokens):
   - Expiration: 90 days.
   - Repository access: *Only select repositories*, the ones in the
     allowlist.
   - Repository permissions, all **Read-only**: Metadata, Contents, Pull
     requests, Issues. Nothing else, and no account permissions.
2. Store it on the Home Node as a systemd credential, root-only:

   ```bash
   sudo install -d -m 0700 -o root -g root /etc/credstore
   sudo bash -c 'umask 077; read -rsp "GitHub token: " token && printf "%s" "$token" > /etc/credstore/jarvis.github_token'
   sudo chown root:root /etc/credstore/jarvis.github_token
   sudo chmod 0400 /etc/credstore/jarvis.github_token
   ```

   The Core unit loads it with `LoadCredential=github_token:jarvis.github_token`.
   This names a credential instead of a path, so systemd looks in
   `/etc/credstore/` (and `/run/credstore/`, `/usr/lib/credstore/`), and a
   missing file does not stop Core from starting, which an absolute path
   would. Core reads `$CREDENTIALS_DIRECTORY/github_token` once at startup.
3. Set the allowlist in `/etc/jarvis/core.env`, comma-separated, at most 20:

   ```
   JARVIS_GITHUB_REPOS=HawkeyNL/PersonalJarvis
   ```

4. Restart Core (`sudo systemctl restart jarvis-core`): the token and the
   allowlist are read only at startup. `GET /v1/roadmaps/github` then shows
   `configured: true` and, after the first pass, `state: ok`.

To rotate the token before it expires, replace the file and restart Core.
To turn the poller off, remove the file or the allowlist and restart.

## What is next

- **Proposals from Jarvis:** a bounded task that reads the roadmap and typed
  GitHub facts and records proposals, never applying them itself.
