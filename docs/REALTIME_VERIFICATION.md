# Realtime verification record

This is a test-evidence record, not a production deployment approval. See
[the protocol contract](REALTIME_PROTOCOL.md) for runtime behavior and limits.

## Scope and source identity

The September 14, 2026 local verification covers the Core changes through
`d2732fa` and the desktop TLS patch `45d2036` in PersonalJarvisApp. Both local
review branches are named `fix/realtime-tls-audit`. No push, merge, tag or
deployment was performed for these follow-up commits.

The desktop pins authoritative `crates/client-core` to
`47450264c3fd0149c6d5636cd60b669127e09ffc`. Comparing that directory at the pin
with the tested Core revision produced no differences. Android and iOS retain
native DTO implementations; they do not introduce another Rust protocol crate.

## Evidence and its limits

| Requirement | Evidence inspected or executed | What it does not prove |
| --- | --- | --- |
| Authenticated event-only channel | Actual WebSocket integration asserts HTTP 401 without auth, HTTP 400 with query parameters, and closes for command JSON, malformed JSON and a 2 KiB frame | Public ingress behavior on a deployed host |
| Per-owner fanout | Two authenticated owner sockets receive equal envelopes; another owner's socket receives no chat event | Every possible compromised-device attack |
| One canonical inference | Fake provider counter remains one across retry, fanout and playback reports; persisted history has one user and one assistant message | Live provider billing reconciliation |
| Durable request identity | `routes/runs.rs` derives reservation identity from authenticated owner/device/request and rejects payload conflicts; integration retries reuse the run | Power-loss durability of an operator's storage |
| Streaming | Real local HTTP SSE fixture observes a delta before final completion; parser tests exclude reasoning and bound frames | Streaming support in providers using the documented final-only fallback |
| Recovery | Integration disconnect/failure tests preserve the user message and do not repeat inference; reconnect reads canonical state | Physical-device sleep/network switching |
| Voice ownership | Shared speech gates allow only the origin device to speak; authenticated playback reports reject the other device without another model invocation | Audible playback on actual hardware |
| Bounded resources | Hub tests cover slow consumers, owner/device limits and subscriber cleanup; desktop tests cover queue overflow and cancellation | Production load/performance benchmarks |
| Native credential boundary | Desktop native tests exercise origin/session binding, response filtering and fixed voice-control paths; frontend receives typed events | An independent penetration test |
| Client reconciliation | Desktop projection and Android state tests cover deduplication/run scoping; mobile lifecycle and selected-conversation checks inspected | End-to-end simultaneous interaction on three physical clients |
| Local speech | Native desktop worker tests and Android speech/playback tests use fake engines; iOS uses an AVSpeechSynthesizer abstraction | Installed voice availability, pronunciation or sound quality |
| TLS dependencies | Core and desktop lockfiles updated to rustls 0.23.45; both audits exit successfully | Resolution of separately reported existing dependency warnings |

## Commands run

Core:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all --quiet
cargo audit
cargo test --locked -p jarvis-client-core --lib
cargo test --locked -p jarvis-api --lib realtime
cargo test --locked -p jarvis-llm stream
```

These checks passed. The normal workspace test command retains explicitly
ignored database tests; it must not be described as having executed them.
The two realtime database tests were separately run against a rootless,
disposable SurrealDB 2.6.5 in-memory instance on `127.0.0.1:18081`, with only
fixture credentials:

```sh
JARVIS_SURREAL_TEST_ENDPOINT=127.0.0.1:18081 \
JARVIS_SURREAL_TEST_USER=fixture JARVIS_SURREAL_TEST_PASS=fixture-only \
cargo test --locked -p jarvis-api --test surreal_api realtime:: -- --ignored
```

Both passed, including after the TLS update and protocol rejection assertions.
The disposable database was stopped afterward. No production database was used.
Sandbox-only attempts failed when local sockets or the advisory-cache lock were
blocked; reruns with the required test permissions passed without weakening tests.

PersonalJarvisApp:

```sh
cargo test --locked --manifest-path desktop/src-tauri/Cargo.toml --lib
cargo audit --file desktop/src-tauri/Cargo.lock
node --test desktop/tests/realtime.test.mjs \
  desktop/tests/speech-status.test.mjs desktop/tests/speech-rate.test.mjs
cd android
./gradlew :realtime-core:test --no-daemon --rerun-tasks
```

The 34 native desktop tests, three Node test files and 20 Android JVM tests
passed. Android tasks were forced to execute, rather than accepting only an
up-to-date cache result. No iOS test was executed locally on this Linux host.
The prior client CI run `34715124060` passed iOS simulator/device-build validation;
that is historical runner evidence, not proof of a new local iOS test or device
installation. The new local TLS commits have not been pushed for platform CI.

## Remaining acceptance boundaries

### September 19 concurrent-retry regression

Revalidation started from fetched Core main
`5e9334a` on local branch `audit/realtime-september-19`. Client main was
`ec562c9`; the existing release-preparation branch was left unchanged.

Extending the two-socket test to submit identical HTTP requests concurrently
reproduced a failure in an existing conversation: one request returned HTTP
409 before the other request's durable reservation became visible. It did not
create a second inference, but failed the retry/reuse contract. The submission
path now waits boundedly for an already active identical run's reservation,
validates its payload hash, and returns the existing identity without spawning
another worker. Distinct competing runs still conflict.

Both database integration tests passed ten consecutive times against a fresh
rootless in-memory SurrealDB 2.6.5 fixture. The test checks identical events and
canonical text on two sockets, one provider call, one voice owner, offline
recovery, and failures. Additional assertions preserve conflict rejection for
changed payloads and competing prompts. No production database was accessed.
The official test binary was downloaded into a temporary directory and checked
against its GitHub release asset SHA-256; each database process was stopped by
the test shell's exit trap.

Fresh checks passed: 12 shared client-core tests, 10 API realtime unit tests,
4 LLM streaming tests, three desktop frontend realtime/speech test files,
workspace tests, formatting and all-target/all-feature clippy. Database tests
remain ignored by the normal workspace command and were run separately above.
The LLM socket test required execution outside the socket-restricting sandbox.
Cargo audit exited zero with the same three allowed warnings listed below.
This does not establish physical iPhone/Android/desktop acceptance. No merge,
release, tag, deployment or client source change was performed in this audit.

### September 16 revalidation

The current Core review worktree is `dee8b692ce3ba6837d2d109250d3ec73adae11ca`;
its tree matches fetched Core main `b35e02288289e9880590d9cde7f6d7c51404eafd`.
Client verification uses `879471e416ad18693acef7111cbe0be58f395e2d` (the
Android release-tool path fix). Fetched client main is
`5fba8f484954cb187a6de9779da26a9e17d21eb0`. The immutable client-core pin
listed above still has no source differences from this Core worktree.

Client CI [34892924319](https://github.com/HawkeyNL/PersonalJarvisApp/actions/runs/34892924319)
completed successfully: desktop frontend, Linux/macOS/Windows native builds,
Android debug/release validation, iOS simulator, and release/privacy checks.
This supersedes the earlier statement that the TLS follow-up had no platform
CI evidence; it does not establish physical-device acceptance.

Fresh local checks passed: 11 shared client-core tests, 9 API realtime tests,
4 LLM streaming tests, 34 native desktop tests, 7 frontend realtime/speech
tests, and Core formatting. The SSE test initially failed because the sandbox
forbade its loopback listener, then passed with socket permission; no production
provider was contacted.
No merge, release or deployment was performed for this revalidation.

The 20 SDK-independent Android JVM tests also passed with `--rerun-tasks`.
Core `cargo clippy --locked --all-targets --all-features -- -D warnings` and
`cargo test --locked --all --quiet` passed (explicitly ignored database tests
remain ignored). `cargo audit` exited zero with three allowed warnings:
unmaintained `atomic-polyfill` and `bincode`, and yanked `chacha20`. This is not
a warning-free dependency audit.

A subsequent September 16 run executed both previously ignored realtime
database integration tests against a new unprivileged SurrealDB 2.6.5
in-memory process on `127.0.0.1:18081`, using fixture-only credentials. Both
passed (0.47 seconds total). The two-socket test now additionally asserts
strictly increasing event sequences, started-before-delta/completion ordering,
and byte-identical canonical REST history after reconnect without another
provider invocation. The disconnect/failure test also passed. Formatting and
all-target/all-feature clippy with warnings denied passed after these changes.
The disposable process is stopped after testing; production state is untouched.

Before claiming physical multi-device acceptance, use an isolated test Core and
two or more enrolled test clients. Verify the same conversation streams without
manual refresh, a different selected conversation is not forcibly opened, only
the origin voice owner speaks, stop/ownership loss cancels speech, and
background/resume and network loss reconcile persisted history. Use a counting
fake provider to avoid paid generation and verify one invocation per request.
This exercise requires access to the actual devices and is not yet recorded.

Windows local TTS remains explicitly unsupported with a graceful unavailable
state; supported initial local engines and final-only provider fallbacks are
documented in the protocol. Push notifications and additional local speech
engines remain separate follow-up work. No paid TTS or LLM speech rewrite is
introduced.

The schema compatibility gate and separate release acceptance policy are not
waived by any of these results. This record does not certify the entire goal as
complete and must not be used to bypass those gates.
