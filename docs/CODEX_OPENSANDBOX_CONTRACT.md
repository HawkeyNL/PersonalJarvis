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

The profile retains the existing OpenSandbox default-deny egress policy. Its
allowlist is limited to package/source registries; loopback, RFC1918, link-local
and Docker/host ranges remain denied, including through DNS rebinding.

## Broker-mediated authentication

The Codex broker retains the long-lived provider credential in its own
root-managed service environment; it is never represented in the sandbox
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

## Production activation gate

The present OpenSandbox network policy correctly blocks sandbox-to-host and
private-network connections. It therefore cannot yet reach the narrow broker
API without an explicit, reviewable OpenSandbox-native task proxy that preserves
the same per-run capability checks. The Home Node must not enable the Codex
broker socket or real runs until that reverse/task-proxy mechanism is proven
end-to-end. A missing broker-auth path fails before sandbox creation; it never
falls back to a host `codex` process. This is an activation gate, not a
convenience TODO.

When that gate is met, each completed, failed, timed-out or cancelled run still
terminates its disposable sandbox. Resume starts a new sandbox from the current
trusted repository snapshot and bounded factual checkpoint; it never resumes a
container. Applying a returned patch, committing or publishing remains a
separate owner-approved operation and cannot write protected main directly.
