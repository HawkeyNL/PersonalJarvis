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
- `assistant.started` with run/request/conversation IDs
- `assistant.delta` with run identity and public assistant text only
- `assistant.completed` with the canonical persisted assistant message
- `assistant.failed` with a fixed safe reason code
- `voice.owner_changed`, `voice.started`, `voice.stopped`, `voice.failed`

There are no arbitrary tool payloads, hidden reasoning fields, provider response
objects, authorization headers, system prompts or private scratchpads in these
types. Logs record lifecycle/status, not message text or bearer-bearing URLs.

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
reservation; a changed payload with the same ID conflicts. Reservations remain
after failure/deletion and are not automatically pruned or replayed. After a
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
