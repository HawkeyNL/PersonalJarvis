# AI Security Hardening Plan

## Purpose

This document is an implementation brief for the coding AI working on PersonalJarvis.

The current architecture already has an agent sandbox, optional Claude Code execution, LLM routing, a resource registry, production fail-closed migrations, and documented capability boundaries. The next step is to turn the security intent into enforceable runtime boundaries.

Do not solve this by adding broad permissions or by giving the model more direct host access. Jarvis should remain capable while being deliberately constrained.

## Priority 1 — Agent execution boundary

Review and harden the complete path:

`LLM -> agent/tool request -> policy/capability check -> sandbox -> executor -> host`

Requirements:

- No LLM response may directly execute a shell command.
- Every mutating action must pass an explicit policy/capability gate.
- Read-only actions and mutating actions must be distinguishable in code.
- Destructive/high-risk operations must require explicit approval.
- Claude Code execution must inherit the same policy boundary; it must not become a backdoor around Jarvis policy.
- Workspace paths must be canonicalized and prevented from escaping the configured workspace.
- Symlink/path traversal attacks must be rejected.
- Do not pass the complete Jarvis process environment to child agents.
- Provider API keys, database credentials, signing keys and unrelated secrets must not be inherited by agent processes.
- Add command/process timeouts and reasonable output limits.
- Avoid unbounded child-process creation.
- Make failures fail closed.

## Priority 2 — Authentication and abuse protection

Audit all authentication-sensitive endpoints, especially challenge/login/enrollment flows.

Implement where appropriate:

- per-IP and per-identity rate limiting
- bounded request body sizes
- replay protection
- strict challenge expiry
- constant-time verification where applicable
- lockout/backoff for repeated failures
- generic external error messages
- security-relevant structured audit logs without logging secrets

Do not expose cryptographic material, internal errors or provider credentials in API responses.

## Priority 3 — API input/output hardening

Review public API handlers for:

- unbounded strings
- unbounded JSON payloads
- unchecked identifiers
- unsafe enum fallbacks
- path parameters
- query parameters
- error responses leaking internal implementation details

Prefer typed request/response DTOs over `serde_json::Value` at API boundaries when practical.

The `/readyz` endpoint should expose only a generic readiness result externally. Detailed database/provider errors belong in logs/metrics, not in the response body.

## Priority 4 — LLM/provider isolation

Review the provider router and registry with these rules:

- API keys stay server-side.
- Provider credentials must never be inserted into prompts or model-visible context.
- Agent child processes receive only the credentials they strictly need.
- Disabled providers should not be initialized as if they were available.
- Budget enforcement must fail closed for paid providers once the configured limit is reached.
- Provider/model selection must remain deterministic and auditable.
- Log provider/model decisions without logging prompts containing secrets or sensitive personal data.

## Priority 5 — Signed approvals / high-risk actions

The architecture refers to signed approval for agent mutations. Verify whether this is actually enforced in runtime code.

If it is only documented, implement a minimal approval primitive before expanding agent capabilities.

An approval should bind at least:

- actor/device identity
- requested capability/action
- normalized arguments or an equivalent request hash
- creation time
- expiry time
- unique request identifier
- approval state

The executor must verify the approval immediately before executing the action. Changing the action after approval must invalidate the approval.

Do not implement a fake approval mechanism that merely checks a boolean flag.

## Priority 6 — Observability

Every privileged/agentic action should be traceable without storing secrets.

Capture useful metadata such as:

- request/task ID
- actor/device
- capability
- tool/executor
- provider/model when relevant
- approval ID when relevant
- start/end time
- success/failure
- reason for denial

Never log API keys, private keys, authentication secrets or full sensitive prompts.

## Testing requirements

For every security boundary added or changed, add automated tests where practical.

At minimum test:

1. path traversal is rejected
2. workspace escape through symlinks is rejected
3. unauthorized mutation is rejected
4. expired approval is rejected
5. modified arguments invalidate approval
6. agent processes do not inherit protected secrets
7. authentication endpoints rate-limit repeated failures
8. oversized requests are rejected
9. readiness responses do not expose internal database errors
10. budget limits prevent further paid-provider calls

Prefer focused unit/integration tests over broad mocks that do not exercise the actual policy boundary.

## Implementation rules

- Inspect the existing code and ADRs before changing architecture.
- Reuse existing abstractions when they are sound; do not create duplicate policy systems.
- Keep PRs small and independently reviewable.
- Do not break the existing provider/router behavior unnecessarily.
- Do not add Redis or another infrastructure dependency merely to implement rate limiting if an in-process solution is sufficient for the current single-node deployment. Document the scaling trade-off.
- Do not expose SSH, databases or privileged host APIs to the public internet.
- Do not give the LLM arbitrary `sudo` access.
- Do not weaken a security check to make a test pass.
- Update relevant ADRs/documentation when the implementation changes an architectural decision.

## Definition of done

This work is complete when the AI can demonstrate, with code and tests, that:

- a model cannot bypass Jarvis policy through Claude Code;
- an agent cannot escape its workspace;
- privileged mutations require a real, bound, unexpired approval;
- secrets do not leak into child processes or API responses;
- authentication and API boundaries have bounded abuse/input;
- security failures fail closed;
- important agent actions are auditable;
- CI covers the new security behavior.

Do not mark this complete based only on documentation. The important parts must be enforced by executable code and tests.
