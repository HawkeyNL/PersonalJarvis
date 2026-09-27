# Codex → OpenSandbox execution contract

Production coding is deliberately not a Core subprocess:

```text
Jarvis Core → signed owner approval → local Codex broker
            → disposable OpenSandbox Codex profile → structured result
            → trusted validation → Jarvis Core
```

The public API can create logical coding sessions, but it cannot execute a
command. A start or resume request is a typed `SignedCodingRequest`; it binds a
domain-separated Ed25519 signature to the operation, request ID, nonce, owner
and device IDs, issue/expiry times, repository identity, exact base commit,
objective, factual checkpoint, reservation and resource limits. The local
broker must revalidate the active device and consume the request ID once before
performing a mutation. A Bearer session is only a transport gate and is never
enough to start a run.

There are no protocol fields for host paths, Git URLs, shell commands,
environment variables, image references or arbitrary network destinations. A
root-managed repository registry resolves a logical repository identity and
must supply an isolated archive at the signed base commit. The runner uses only
the server-owned `Codex` OpenSandbox profile and fixed runtime command. It
uploads a snapshot and request data, never mounts a live checkout, `/etc`, a
home directory, Docker socket, Jarvis secrets or provider environment.

The sandbox also receives a finite `task-context.json` projection of the
signed objective, logical repository and exact SHA, resource ceilings and
latest factual checkpoint. A trusted compiler may add selected provenanced
facts and recent deltas; it never forwards whole conversation history by
default. The envelope is capped at 32 KiB. Old raw deltas are dropped before
explicit owner constraints or the latest checkpoint; if those cannot fit,
execution fails closed.

For a resumed run, the objective must come from the existing owner-scoped
coding session, not from the checkpoint summary. The checkpoint records facts
about prior work; it is not a replacement authorization or a new objective.
If the trusted session objective cannot be loaded and matched to the signed
session/repository/revision, resume is refused before sandbox creation.

The profile retains the existing OpenSandbox default-deny egress policy. Its
allowlist is limited to package/source registries; loopback, RFC1918, link-local
and Docker/host ranges remain denied, including through DNS rebinding.

## Reviewed repository snapshot foundation

`jarvis-codex::snapshot` implements a fail-closed, read-only registry used by
the broker execution path after its provider activation gate. Production registry bytes come only from
root-owned `/etc/jarvis/codex-repositories.json` (version 1), with at most 32
entries. Each entry contains a validated logical `RepositoryIdentity` and one
reviewed `refs/heads/...` ref; it cannot name a path or Git URL. A matching
root-owned bare mirror must exist as
`/var/lib/jarvis-codex-repositories/<id>.git`. An exact commit must be reachable
from that ref. The snapshot is a bounded Git archive from that exact commit,
not a live checkout or bind mount. Symlinks, submodules, traversal, common
credential filenames, and oversized trees fail closed. The resulting tar is
parsed and validated again before upload. The archive hash is
available for run provenance. The fixed local Git subprocess has a 30-second
deadline and receives a clean environment.

No repository registry, mirror, or Codex workload is installed or activated
merely by adding this code. A filename denylist alone is not proof that a
reviewed repository contains no secrets; the owner must approve the mirror
contents before allowing a coding run.

## Broker-mediated authentication

Provisioning, reconnecting, disconnecting or switching the long-lived Codex/ChatGPT account is an explicit owner-admin operation. Jarvis Core, agents, MCP tools and sandbox workloads may consume only the already-authorized runtime capability; they have no account-lifecycle authority. A subscription-auth failure must not silently activate separately billed API credentials.

The official Codex runtime retains the long-lived provider credential under
the dedicated `jarvis-codex` identity; it is never represented in the sandbox
provider API, image, environment, artifacts or logs. Following a signed start
or resume, the broker may mint a cryptographically random, opaque capability
token. It stores only the SHA-256 token verifier with these claims:

- run and coding-session IDs;
- repository identity and exact base SHA;
- expiry, reservation ID and budget ceiling;
- the sole allowed operation: `codex.run_approved_task`.

The sandbox receives that temporary token only in its generated task input. A
broker request must repeat every binding; the broker checks token hash, TTL,
run state, operation, repository/base SHA, request-ID replay and budget before
using its own provider credential. Cancellation, completion and broker restart
revoke outstanding tokens. The broker does not expose model selection, a
generic OpenAI endpoint, credential inspection, arbitrary request bodies or
general command execution.

## Current execution path and remaining activation gate

The sandbox never opens a host/private-network connection. The fixed
`jarvis-codex-runtime` writes one bounded transient task request inside its
disposable workspace. The broker reads it through OpenSandbox's authenticated
manager file API, checks every capability binding, and writes one fixed task
response through that same manager API. This is a per-run, one-operation relay,
not a general HTTP/OpenAI proxy or mounted broker socket. CI uses a fake
subscription adapter to exercise the full runtime/relay/teardown sequence;
this is not evidence of a live Codex subscription run.

The broker has owner-scoped durable run records, status/cancel handling,
bounded artifact validation and a fixed private artifact store. An owner-run
image build uses only the checksum-bound release runtime plus a reviewed
digest-pinned Ubuntu base, offline. The resulting registry image digest must
be explicitly configured; no mutable image is accepted. `sudo jarvis sandbox
verify` is read-only and never claims that merely seeing Docker proves Kata,
egress, quota or account acceptance.

Core now issues a short-lived subscription reservation from an authenticated
coding session. The signed request carries its ID, but the broker must lease
the matching database row for that owner/session/run before minting a run
capability. This is a non-monetary execution slot with an explicit maximum
runtime and provider-turn ceiling; successful subscription work settles with
zero Jarvis API-spend cents, while failure releases the slot without making
it reusable. A random UUID from the client is not reservation authority.

The OpenSandbox create request carries bounded `jarvis.run_id`,
`jarvis.session_id` and `jarvis.profile=codex` metadata. The manager-issued
sandbox ID is recorded on the durable run before source upload or execution.
At broker start and periodically, the broker lists only manager workloads
with Jarvis Codex metadata and terminates stale owned workloads before
releasing their reservations. Incomplete cleanup stops admission of new runs;
unrelated OpenSandbox workloads are never deleted. This recovery path still
requires disposable-manager/SurrealDB integration and crash-injection proof
before production activation.

The remaining provider activation gate is precise: the current official
[Codex App Server documentation](https://learn.chatgpt.com/docs/app-server)
labels the app-server command and WebSocket transport experimental and not
supported for production workloads. The stable `codex exec` command runs
model-generated shell commands; invoking it with the long-lived subscription
credential in a host process is not a provider-only task proxy. Neither route
is a reviewed, provider-only subscription adapter for this isolated channel.
The production adapter therefore reports unavailable and `start_run` refuses
before creating a run or sandbox. This gate must not be replaced with a host
Codex CLI, `OPENAI_API_KEY`, an undocumented OAuth endpoint, or a credential
mount. Real Home Node/Kata and owner-account acceptance are also still pending.

When that gate is met, each completed, failed, timed-out or cancelled run still
terminates its disposable sandbox. Resume starts a new sandbox from the current
trusted repository snapshot and bounded factual checkpoint; it never resumes a
container. Applying a returned patch, committing or publishing remains a
separate owner-approved operation and cannot write protected main directly.
