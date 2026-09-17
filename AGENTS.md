# PersonalJarvis — Public Source Agent Guide

This repository contains the public source snapshot of PersonalJarvis. Private
product plans, ADRs, deployment runbooks, security architecture documents and
agent profiles intentionally live outside this repository. Do not re-add them
here or infer missing operational details from older history.

Before changing code, inspect the current source, tests, manifests and CI
configuration. Keep changes narrow and avoid speculative infrastructure.

## Security boundaries

- `jarvis-policy` remains the authoritative capability and risk decision layer.
- Jarvis runtime, system-administration and other security-sensitive product
  actions require real device-signed, action-bound, unexpired approval; a
  Boolean approval flag or ordinary session is never sufficient.
- Owner-approved exception: local Home Node device enrollment/revocation may
  use freshly authenticated OS administrator authority through a narrow,
  root-peer-verified local IPC boundary. This is not an HTTP/session bypass,
  does not authorize agents, and does not relax signed approval for remote
  clients or other privileged actions. Core Admin remains unprivileged and
  must never receive the OS password; its five-minute inactivity lock remains.
- Normal source-control and release-engineering operations in this repository
  (including editing, committing, pushing, tagging and dispatching CI/release
  workflows) are outside the Jarvis device-signing protocol. They still require
  explicit owner authorization proportional to their external impact, must
  preserve protected-branch/environment review, and never implicitly authorize
  a Home Node or production runtime mutation.
- Never expose secrets, private keys, tokens, credentials, full inherited
  environments or protected Core paths to an agent or sandbox.
- Preserve the sandbox, Core and `.git` protections, resource bounds, kill
  switches and security audit trail. Fail closed.
- Do not add public management, shell, database or internal-service endpoints.

## Verification

Run applicable checks before proposing a change:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo audit
```

Do not claim completion without reproducible verification.
