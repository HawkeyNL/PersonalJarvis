# Realtime protocol 1

Authoritative wire types: `crates/client-core/src/realtime.rs`. Desktop consumes
an exact Git revision; Android and iOS use native projections of that contract.
Core and application SemVer are unrelated to this protocol number.

## Transport and authentication

`GET /v1/events/capability` is authenticated capability negotiation. A protocol-1
client uses native `Authorization: Bearer …` headers to upgrade `GET /v1/events`.
Use `wss://<runtime-configured-origin>/v1/events` through the existing HTTPS
reverse proxy. No additional port, public Core binding, browser token, URL token,
websocket RPC or unauthenticated subscription is introduced. Caddy's existing
reverse proxy supports the upgrade; deployment policy is unchanged.

The existing device/session extractor authenticates subscriptions. Core checks
session expiry/revocation and active device ownership again every 30 seconds.
Failures close the connection. Incoming application messages are forbidden:
commands continue over authenticated HTTP. Incoming frames/messages are limited
to 1 KiB; server public envelopes are limited to 256 KiB. Heartbeats are 30
seconds, missing pong tolerance 75 seconds, and writes time out after 5 seconds.

Subscriptions are owner-partitioned on the server. Limits: 256 total, 16 per
owner, two per device; 64 queued envelopes per socket. A full queue disconnects
that subscriber instead of blocking generation. Shared `Arc` payloads avoid
copying each message per device. Dropping a socket unregisters its subscriber.
Connect attempts are limited to 30 per device/minute; voice controls to 30 and
playback reports to 120, in addition to the existing authenticated API limiter.

The disposable-database integration test also exercises actual authenticated
`/v1/voice/playback` requests: a second non-owning device is refused, the owning
device's start/stop statuses reach both WebSocket clients identically, and the
fake provider's invocation count remains one throughout playback reporting.
This validates server fanout/accounting, not audible device playback.

## Envelope

Fields: `protocol`, `epoch`, `event_id`, `sequence`, `at`, `type`, `payload`.
UUID `epoch` changes after Core restart. Sequence numbers increase in-process;
they are not database revision numbers and gaps are normal. No replay guarantee
is made. Every new connection begins with `connection.ready` and
`reconcile: true`. Clients discard duplicate/stale sequence numbers within the
same epoch. A changed epoch requires a new ready event and REST reconciliation.

Public events:

- `conversation.created`, `conversation.updated`, `conversation.deleted`
- `message.created` with canonical message and originating request ID
- `assistant.started` with run/request/conversation IDs and, only for an
  automatic web search run, `web_search: true` (absent otherwise; older
  clients ignore it). Such a run follows the `assistant.completed` of the
  answer it extends, reuses that run's request ID, has a new run ID and no
  `message.created` of its own; see
  [Automatic web search](#automatic-web-search).
- `assistant.delta` with run identity and public assistant text only
- `assistant.completed` with the canonical persisted assistant message and,
  only when research was advised (by the intent classifier, or by the model's
  `[[web_search]]` line, see
  [Automatic web search](#automatic-web-search)) and an owner-started
  research run could start now, `research_offer: {"question": "..."}`: the
  owner's own latest question, trimmed. It is advisory and not stored. Core
  never searches on it; an app may show a button whose click sends exactly
  that question as a new run with `"mode": "research"`. Older apps ignore it.
  It is absent when Core starts an automatic web search instead.
- `assistant.failed` with a fixed safe reason code
- `voice.owner_changed`, `voice.started`, `voice.stopped`, `voice.failed`
- `trading.meme.strategy_changed` with `strategy_id`, `version` and `enabled`,
  and `trading.meme.decision` with the decision row's `id`; see
  `MEMECOIN_PAPER.md`
- `roadmap.changed` with the roadmap `id` and its new `revision`, after every
  committed roadmap write; see `ROADMAPS.md`
- `jobs.run_finished` with the `job_id`, `run_id` and the run's `status`,
  after a scheduled job's run ends or a slot is recorded as missed; see
  `SCHEDULED_JOBS.md`

The trading, roadmap and job events are hints to reload over REST and carry
only ids, versions, statuses and flags, never market data or model text.
They go to every connected owner. Apps before 0.1.27 close the socket on an
event type they do not know; 0.1.27 and later ignore it.

There are no arbitrary tool payloads, hidden reasoning fields, provider response
objects, authorization headers, system prompts or private scratchpads in these
types. Logs record lifecycle/status, not message text or bearer-bearing URLs.

## Automatic web search

With `research_web_search: "auto"` (see `MODEL_ROUTING_OPERATIONS.md`) Core
may extend a runs-API answer by itself; `POST /v1/assistant/chat` never does.
After the answer's `assistant.completed` (without `research_offer`), Core
publishes `assistant.started` with `web_search: true`, runs Research on the
same latest question, stores the answer as `web:<model>` and publishes
`assistant.completed`. It only does so when research was advised (the
classifier said so, or the answer ended with a final line that is exactly
`[[web_search]]`), a research run can start, the privacy gate passes, no
automatic run is in flight and the rate cap allows; otherwise the answer carries the usual
`research_offer`. The run claims no voice ownership, so it is not spoken, and
it does not hold the conversation's run reservation: a new owner message may
run beside it, and the web answer is then stored after that message.

If the search does not run after the run started (the switch went off, the
subscription worker's one background slot was busy with another research or
task run, or no subscription answered), Core publishes `assistant.failed` with
`provider_unavailable` and stores nothing: an unrequested run never leaves a
"research unavailable" notice in the conversation. The answer before it
stands and the owner can still ask for Research. A failure cannot fall back to
the offer, because the answer's `assistant.completed` was already sent.

The web answer is stored only while the conversation still exists, in one
transaction that also writes the conversation row. When the owner deleted the
conversation during the search, the run ends with `assistant.failed` and
`interrupted`, nothing is stored and no `conversation.updated` follows. A
completed run's `conversation.updated` carries the title read in that
transaction.

## Runs and persistence

New clients POST `/v1/assistant/runs` with `request_id` (UUID), `messages`, and
optional `conversation_id`. The response contains run/request/conversation IDs
and state. `GET /v1/assistant/runs/{id}` is owner-scoped recovery/status.
`POST /v1/assistant/chat` remains the synchronous compatibility endpoint.
It publishes the same live events with one provider invocation, but old requests
without a client UUID cannot offer retry idempotency. Its worker also survives
an HTTP disconnect. New clients use the durable runs endpoint instead.

A run has a durable unique reservation derived from authenticated user, device
and request UUID, plus a hash of the original typed payload. A retry reuses the
reservation; a changed payload with the same ID conflicts. This also applies
to concurrent retries: if the same authenticated run already owns the
conversation but its initial transaction is not yet visible, the duplicate
submission waits up to two seconds for that reservation (50 ms polling). It
never launches another worker. Unavailable/ambiguous persistence returns an
error; clients retain the request ID for recovery rather than inventing a new
one. A different run competing for the conversation still conflicts.
Reservations remain after failure/deletion and are not automatically pruned or replayed. After a
restart, unfinished reservations are reported as interrupted, never inferred
to be safe to repeat. This deliberately prefers an explicit new owner request
over accidentally repeating an ambiguous paid inference.
The request reservation, initial user message and any new conversation are
committed in one transaction before dispatching the worker. A crash between
reservation and worker startup therefore cannot discard the user's prompt.

At most one new run per conversation executes at once, with four runs per owner
and 16 globally. The detached worker survives HTTP/socket disconnect. User
message persistence is confirmed before `message.created`/`assistant.started`.
Deltas are ephemeral. Final message persistence is confirmed before completion.
Database statement errors are checked, not just transport errors. A failure
retains the user message and cannot cause another model call through fanout or
speech. Usage correlation uses the run ID and records the reply once.
Missing/invalid provider usage on a successful reply uses conservative input-byte
and output-limit estimates, explicitly classified as `conservative_usage` rather
than presented as exact or treated as zero.

Schema migration `0007_assistant_runs.surql` adds durable reservations. Existing
schema-fingerprint upgrade/rollback refusal is intentionally unchanged. This
branch is **not** a routine production update candidate: the separate reviewed
schema-migration deployment procedure is required before any future release.

## Streaming and recovery

Core removes a trailing `[[web_search]]` line from every chat answer before
it is stored or published. While deltas stream, trailing whitespace and a last
line that could still become that marker wait for more text; what is held back
when the stream ends is never sent as a delta, and the completed message
carries the final text.

The provider-neutral `chat_stream` interface defaults to a final-only result.
OpenAI-compatible backends decode bounded SSE and emit only `delta.content`.
They do not emit `reasoning_content` or tool calls. Router streaming makes one
provider attempt: ambiguous failure does not switch models or replay inference.
Other providers currently use the documented final-only fallback, not simulated
token splitting. Topic titles are deterministic; there is no title-classifier
LLM invocation.

Clients reconcile conversation metadata and selected history after reconnect.
Other conversations update their list entries without forcing navigation.
Canonical message IDs/request IDs reconcile optimistic messages and provisional
run text. Final persisted content wins over provisional streaming content.
Backoff is bounded exponential with jitter, capped near 30 seconds. No database
correctness depends on a permanently connected socket. iOS/Android stop sockets
and local speech when backgrounded/locked and reconnect after authenticated
foreground unlock. Push notifications are outside this protocol.

## Canonical inference context

Conversation history responses include additive `assistant_running` state,
derived from the owner's active conversation reservation. Reconnecting clients
must not assume generation stopped just because they missed live deltas.
The field is false after the reservation ends, including failure, and after a
Core restart; durable run status distinguishes interrupted work from completion.
Run status checks the exact active run identity, not merely whether its
conversation is busy. A newer run (or a conversation deletion reservation)
cannot make an older interrupted run appear to be running again.
`GET /v1/assistant/requests/{request_id}` recovers a lost submission acknowledgement
without another POST. Its lookup is bound to both authenticated user and device;
another device's same request UUID does not address the original run. This route
only reads the existing reservation and never invokes a provider. A missing record
is not proof a still-in-flight submission failed, so clients must not blindly
resubmit with a fresh request ID.

Asynchronous `/v1/assistant/runs` uses the owner-scoped persisted conversation,
not a device's supplied replica, as model context. The submitted final user turn
is committed first and must match the newest database row before inference.
The context retains up to 32 latest complete messages within 128,000 UTF-8 bytes,
ordered chronologically; it never summarizes through another model call or
truncates an individual message. Missing/mismatched persistence fails before
provider invocation. Legacy synchronous request-context behavior is retained.
The system text is always the server persona plus a small block of live
runtime facts (see `SUBSCRIPTION_WORKERS.md`, "Runtime facts in the system
text"); a client-supplied system prompt is ignored.
Request-id payload matching remains exact, even if a retry carries different
client history. Tests include a stale client supplying an invented assistant
turn: the fake provider receives the canonical stored answer instead.

## Voice

The originating authenticated device claims voice for its run. Explicit HTTP
`/v1/voice/claim` and `/v1/voice/release` derive the device from authentication;
release cannot release another device's ownership. New clients send `{"run_id":"<UUID>"}` when
releasing a run: the hub atomically checks both authenticated device and run,
so delayed release cannot clear a newer run on the same device. An empty body
retains legacy device-wide release. Any nonempty body must be a valid bounded
run binding (maximum 256 bytes, no unknown fields); malformed data never falls
back to unconditional release. `/v1/voice/owner` returns
non-secret current ownership. Leases expire after 90 seconds without renewal;
authenticated heartbeat responses renew the current owner's lease. A new prompt
may replace the owner immediately. Playback reports use `/v1/voice/playback`
and are accepted only for the current owner/run.
Ownership validation and playback publication occur under one hub lock. Duplicate
statuses are acknowledged without another event; stopped/failed are terminal for
that lease/run, so a delayed callback cannot restart it or impersonate a new owner.

Speech is a client preference, default off, independent of text synchronization.
Local engines consume public assistant content directly with deterministic
Markdown cleanup, omitted fenced code, phrase buffering and per-run received
offsets. Completion only flushes the remaining suffix. Ownership loss/lock stops
speech. No paid TTS service or LLM speech-rewrite call exists.

Initial native engines: iOS `AVSpeechSynthesizer`, Android offline-only
`TextToSpeech` voices, macOS `/usr/bin/say`, Linux `/usr/bin/espeak-ng` when
already installed. Desktop sends text through stdin, never shell/argv. Windows
local speech is not yet implemented; text/realtime remain available.

## Reproducible server verification

Use a disposable in-memory SurrealDB 2.6.5 on a separate loopback port, never the
Home Node production database. Then run:

```sh
JARVIS_SURREAL_TEST_ENDPOINT=127.0.0.1:18081 \
JARVIS_SURREAL_TEST_USER=fixture JARVIS_SURREAL_TEST_PASS=fixture-only \
cargo test -p jarvis-api --test surreal_api -- --ignored
```

The realtime fixture uses actual authenticated sockets, a counting fake model,
two owner devices, another isolated owner, shared speech gates and canonical DB
reads. It verifies one call/usage record, identical final text, origin-only
speech, idempotent retries, owner-scoped voice release and reconnect behavior.
No live model, paid TTS, production credential or Home Node deployment is used.
