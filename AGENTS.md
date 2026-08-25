# PersonalJarvis — Public Source Agent Guide

This repository contains the public source snapshot of PersonalJarvis. Private
product plans, ADRs, deployment runbooks, security architecture documents and
agent profiles intentionally live outside this repository. Do not re-add them
here or infer missing operational details from older history.

Before changing code, inspect the current source, tests, manifests and CI
configuration. Keep changes narrow and avoid speculative infrastructure.

## Security boundaries

- `jarvis-policy` remains the authoritative capability and risk decision layer.
- Mutating or high-risk actions require real device-signed, action-bound,
  unexpired approval; a Boolean approval flag or ordinary session is never
  sufficient.
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
