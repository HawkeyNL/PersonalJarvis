# ADR-040 — OpenSandbox execution boundary

- Status: **accepted for provider foundation; patched Home Node control-plane candidate pending Ubuntu adversarial verification**
- Date: 2026-08-23

## Context

Jarvis needs a process, filesystem and network isolation boundary for untrusted
code, downloaded repositories, dependency installation, generated data analysis
and future Codex/browser work. Jarvis API/Core, SurrealDB, device identity,
policy, signed approvals and audit remain trusted and stay outside that boundary.

## Decision

`jarvis-sandbox` defines the provider boundary and explicit resource/network
profiles. It invokes the existing `jarvis-policy::Capability::ExecuteCode` path;
it does not invent a second risk classifier or treat `approved: bool` as an
approval. A provider failure is always
`sandbox unavailable`: it may never execute the command on the Home Node.

OpenSandbox is the selected integration candidate. Source inspection on
2026-08-23 showed that its Docker runtime publishes execd/HTTP/egress ports on
`0.0.0.0`, while egress policy requires bridge mode. The reviewed, pinned source
patches in `deploy/opensandbox/patches/` add `docker.publish_host`, remove the
single-tenant exemption for sandbox proxy paths, and reject private/special-use
DNS answers before they enter the dynamic nft allow set. The egress sidecar
also inherits the configured PID cap, dropped capabilities (apart from its
minimal `NET_ADMIN` need), `no-new-privileges`, and fixed CPU/memory ceilings.
The Home Node
configuration fixes published ports to `127.0.0.1` and requires the
control-plane API key for every non-health route. The port/auth work passed the relevant upstream
Docker/config test selection (235 tests). The Rust provider accepts only an
authenticated loopback endpoint, explicitly ignores process proxy environment
variables, and accepts only digest-pinned, profile-owned images. It
creates sandboxes with fixed deny-by-default egress, uploads task data only
under `/workspace/input`, executes bounded/quoted commands via the authenticated
server proxy, and only retrieves explicitly requested files from
`/workspace/artifacts`. Scoped-secret injection remains fail-closed.

The trusted control-plane image is built only from immutable Python and `uv`
base-image references supplied by the root-managed environment file; it does
not execute a floating remote `uv` installer during the build.

The public Jarvis API gets no sandbox-exec endpoint. The trusted sandbox manager
is internal-only, policy-gated and must verify device-signed approval immediately
before a mutating task starts. Provider code is not wired to that manager until
the activation gate below has passed.

The previous optional host-process `ClaudeCode` path is deliberately denied even
when its legacy environment flag is set. It cannot become an accidental fallback
while the OpenSandbox-backed Codex broker is still pending.

## Required gate before activation

An OpenSandbox implementation can replace the blocked adapter only when all are
proven by source review and an Ubuntu integration test:

1. lifecycle API binds only to `127.0.0.1`/`::1` and requires an API key;
2. workload exec/file endpoints are reachable only through a loopback-only
   trusted manager path (no `0.0.0.0` dynamic Docker mappings);
3. bridge-mode egress uses `dns+nft`, IPv6 is disabled unless equally enforced,
   and denies loopback, RFC1918, link-local, Docker/internal and metadata ranges;
4. sandbox host bind mounts are disabled, Docker socket is absent from workloads,
   and images are digest-pinned and verified;
5. every profile has fixed CPU, memory, disk, PID, timeout, output, artifact and
   concurrency limits; cancellation and orphan cleanup pass;
6. secrets are task-scoped and never logged or returned in artifacts; and
7. adversarial tests prove that host filesystem, SurrealDB, Jarvis API and LAN
   targets are unavailable.

## Runtime evaluation

- Docker/runc: acceptable only for local development after the port-binding gate;
  containers share the host kernel and are not VM-equivalent.
- Docker + gVisor: OpenSandbox documents a server-level `runsc` runtime, but
  the currently reviewed egress implementation does not support it. It is not
  an activation option until that incompatibility is resolved and retested.
- Kata: stronger VM-style isolation at more operational cost; the preferred
  phase-one Home Node runtime for egress-enabled browser/Codex profiles.
- Firecracker: OpenSandbox documents it through Kata RuntimeClass on Kubernetes,
  so it is not a phase-one single-node Docker path.

## Consequences

The provider protocol foundation is testable now, while Core integration remains
disabled rather than creating a tempting unsafe host-execution fallback. A later
activation is a small, separately reviewed change with an Ubuntu integration
test and rollback runbook.
