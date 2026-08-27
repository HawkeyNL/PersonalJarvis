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

## Credential activation gate

OpenSandbox's current provider adapter intentionally returns unavailable for
`provide_scoped_secret`. Consequently a missing task-scoped Codex credential
fails before sandbox creation; it never falls back to a host `codex` process.
The Home Node must not enable the Codex broker socket until the runtime proves a
narrow credential-vault integration (or a short-lived broker-mediated token)
that does not expose a long-lived API key in the image, environment, artifacts
or logs. This is an activation gate, not a convenience TODO.

When that gate is met, each completed, failed, timed-out or cancelled run still
terminates its disposable sandbox. Resume starts a new sandbox from the current
trusted repository snapshot and bounded factual checkpoint; it never resumes a
container. Applying a returned patch, committing or publishing remains a
separate owner-approved operation and cannot write protected main directly.
