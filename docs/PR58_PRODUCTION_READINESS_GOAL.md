# Goal: finish PR #58 to merge-ready — subscription workers, bounded context and Codex/OpenSandbox activation

Continue **in the existing branch `feat/subscription-account-linking` and PR #58**. Do not create a parallel branch or replacement PR.

Work against the current branch state and current `main`. Preserve every existing trust boundary. The objective is to turn the current draft/foundation into the most complete, merge-ready implementation possible, leaving only explicitly owner-run live acceptance steps.

## Explicit owner-run / out-of-scope items

The owner will perform these manually after the code is ready:

- the real Claude account login in Core Admin / `sudo jarvis accounts connect claude`;
- the real Codex/ChatGPT account login in Core Admin / `sudo jarvis accounts connect codex`;
- checking/disabling Claude account-level extra usage / overage outside Jarvis;
- the final live subscription-backed Claude/Codex acceptance run on the Home Node;
- editing/installing/rolling back the private `Jarvis.md` persona.

Do **not** automate provider login, browser interaction, provider account settings, extra-usage settings or `Jarvis.md` mutation to make the PR appear complete.

A remaining manual acceptance step is acceptable. A remaining code/security/CI blocker is not.

---

# 1. First make current PR CI fully green

Current PR head has one failing CI job:

```text
Deployment and sandbox security fixtures
→ test-subscription-worker-release.sh / systemd verification
→ jarvis-claude.service:
   Command /opt/jarvis/current/jarvis-claude-worker is not executable
```

Fix the fixture correctly.

Requirements:

- do not weaken `jarvis-claude.service`;
- do not remove the production fixed `ExecStart=/opt/jarvis/current/jarvis-claude-worker`;
- do not skip `systemd-analyze verify` merely to get green;
- make the disposable test resolve the fixture worker deterministically;
- keep legacy release compatibility tests;
- rerun the complete CI/security fixture set.

Before marking ready:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo audit
Core Admin Rust tests
Core Admin frontend typecheck/build
release/package fixtures
deployment/sandbox security fixtures
client protocol/update mirror CI
```

All required GitHub checks must be green on the final head SHA.

---

# 2. Preserve the subscription-account security model

The existing direction is correct:

```text
jarvis          → Core
jarvis-claude   → Claude subscription worker
jarvis-codex    → Codex broker/runtime identity
```

Keep these hard rules:

- Core never reads Claude/Codex auth stores.
- Normal Jarvis HTTP/API has no account mutation endpoint.
- Agents and MCP servers cannot login/logout/reconnect/switch account.
- Provider tokens never enter Tauri frontend state, Jarvis API responses,
  SurrealDB memory, prompts, logs, audit text, sandbox artifacts or command
  arguments.
- Worker services never inherit `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`,
  auth tokens, SSH agent state or the owner's home.
- A subscription failure never silently enables PAYG/API billing.
- Explicit API fallback is allowed only if that API model is independently
  owner-enabled and passes existing budget/model policy.

Do not relax these rules while finishing the remaining work.

---

# 3. Make Claude worker code-ready without requiring owner login

The owner will perform the real login later. Codex must make everything around
that live step production-ready.

## Runtime capability verification

Do not assume provider CLI flags forever.

At install/status/preflight time, safely verify the installed reviewed Claude
CLI supports the exact non-interactive flags the worker needs. Do not parse
secrets or full provider output into Jarvis state.

If required flags/output shape are unsupported:

```text
state = incompatible_runtime
worker remains disabled
no paid fallback
```

The current worker uses functionality equivalent to:

- headless prompt execution;
- structured JSON output;
- explicit model selection;
- custom system prompt from a private file;
- no session persistence;
- tools/MCP disabled;
- subscription auth status.

Reconcile exact flags with the installed official CLI. Do not invent flags to
satisfy tests.

## Billing safety

`overage_unverified` is an honest state and may remain because the CLI cannot
prove the provider's account-level extra-usage setting.

Code must:

- never display that state as "subscription-only guaranteed";
- keep automatic worker activation disabled;
- require an explicit owner activation after the owner verifies the account
  setting;
- never attempt to change that provider account setting automatically.

The final live billing acceptance remains owner-run.

## Worker protocol

Keep the worker socket protocol finite and bounded:

```text
status
run
cancel (if implemented)
```

No generic shell/cwd/env/executable fields.

Test at least:

- wrong peer UID denied;
- oversized request/reply denied;
- malformed protocol denied;
- API-key auth does not count as subscription auth;
- missing/logged-out runtime returns safe unavailable state;
- plan/rate limit returns a bounded plan-limit state;
- provider stderr/secrets never cross the socket;
- Core never executes `claude` directly.

---

# 4. Finish the bounded Context Compiler requirement

The existing "last 8 turns / 24k chars" cap is useful, but it is not the complete
context-compiler goal for long-running agent/coding work.

Add or reuse a provider-neutral bounded task-context structure for delegated
work, conceptually:

```text
TaskEnvelope
- objective / intent
- owner constraints
- relevant recent delta(s)
- selected memory facts + provenance
- agent id/version
- repository/file/source references
- latest factual checkpoint
- prior logical session/run reference
- context/output/runtime limits
```

Requirements:

- no complete long chat replay by default;
- no hidden chain-of-thought;
- deterministic hard size/count ceilings;
- oldest/irrelevant raw conversation data drops before explicit owner
  constraints or latest factual checkpoint;
- references are preferred over copying large source/repository contents;
- persisted checkpoints contain facts, decisions, tests/results, blockers and
  artifact references only;
- resume = checkpoint + current state + relevant new delta.

Do not build a second memory system. Reuse current conversation/session/memory
state where practical.

For ordinary short chat, the current bounded recent-turn behavior may remain.
The reusable compiler is primarily required for agents, long research and
Codex session resume.

Add tests proving a very long conversation does not become a very large worker
request.

---

# 5. Finish the Codex/OpenSandbox production activation gate

This is the main remaining red item.

Current state intentionally denies valid broker requests with:

```text
scoped Codex credential unavailable
```

The foundation already contains:

- `SignedCodingRequest`;
- owner/device signature verification;
- exact repository/base-SHA binding;
- `RunCapabilityClaims`;
- opaque random `RunCapabilityToken`;
- SHA-256 verifier storage;
- replay protection;
- operation binding;
- TTL/budget binding;
- `RepositorySnapshot`;
- `execute_in_sandbox(...)`;
- fixed `CODEX_SANDBOX_COMMAND`;
- OpenSandbox provider/profile/resource/network/artifact bounds.

Use these existing primitives instead of building a parallel path.

## Required final flow

```text
Jarvis Core
  ↓ signed owner-approved coding request
Codex broker
  ↓ revalidate device/signature/replay/budget
trusted repository snapshot at exact base SHA
  ↓
mint short-lived run capability
  ↓
disposable OpenSandbox Codex workload
  ↓ narrow task proxy / broker capability
Codex subscription runtime
  ↓
bounded result + diff/tests/artifacts
  ↓
trusted validation
  ↓
Jarvis
```

There must be **no host Codex fallback**.

---

# 6. Implement the narrow OpenSandbox-native task proxy

The current network policy correctly prevents a sandbox from reaching the Home
Node/private network. Do not punch a general exception through that policy.

Implement the narrowest practical task-proxy mechanism that preserves all
existing isolation.

Required properties:

- only the Codex sandbox profile can use it;
- only a currently active run can use it;
- sandbox receives only an opaque per-run capability token;
- long-lived Codex/ChatGPT auth remains outside the sandbox;
- no generic HTTP/OpenAI proxy;
- no arbitrary URL/model/headers/request body;
- no generic host-network route;
- no bind mount of the owner's auth directory;
- no Docker socket;
- no Core/provider secret directory;
- no arbitrary host Unix socket exposed inside the sandbox.

The proxy accepts only the fixed operation:

```text
codex.run_approved_task
```

and repeated binding fields:

- request ID;
- run ID;
- coding session ID;
- repository identity;
- exact base SHA;
- reservation ID;
- fixed operation.

Broker checks them against the stored capability claims before using the
already owner-authorized Codex runtime.

### Preferred isolation shape

Prefer an OpenSandbox-native/per-run sidecar or task proxy with **no public host
port**, scoped to the individual sandbox/run. If a host-side bridge is required,
it must be reachable only through that managed sandbox mechanism, not by
ordinary LAN/host/private-network routing.

Do not solve this by allowing RFC1918/host gateway access from the sandbox.

If the installed OpenSandbox version genuinely lacks a safe primitive, keep the
system fail-closed and document the exact technical blocker instead of
implementing an insecure shortcut. But first exhaust the safe sidecar/task-proxy
approach and add reproducible tests.

---

# 7. Make the Codex broker execute a real bounded run

After all validation succeeds, the broker must stop being deny-only.

Implement the trusted lifecycle around the existing primitives:

1. parse finite broker request;
2. validate shape;
3. verify expiry + device signature;
4. enforce policy and owner approval;
5. atomically consume/replay-protect the signed request;
6. verify/reserve budget;
7. resolve logical repository through a root-managed allowlist/registry;
8. create exact-base immutable archive/snapshot;
9. mint run capability;
10. create disposable OpenSandbox workload;
11. upload only:
    - bounded task request;
    - exact repository archive;
    - opaque capability input;
12. apply fixed profile/network/resource policy;
13. execute the fixed runtime command;
14. collect only allowlisted bounded artifacts;
15. validate result/diff metadata;
16. terminate sandbox on success/failure/timeout/cancel;
17. revoke capability;
18. checkpoint factual result;
19. audit non-secret outcome.

Do not expose caller-selected host paths, Git URLs, commands, images,
environment variables or network destinations.

---

# 8. Implement the missing repository snapshot authority

The public request contains a logical repository identity and exact commit, not
a host path.

Complete the trusted server-owned repository registry/snapshot path needed by
`RepositorySnapshot`.

Requirements:

- fixed owner-configured allowlist of repository identities;
- registry maps logical identity to a trusted local mirror/checkout controlled
  by deployment, not user API input;
- requested exact commit must exist and be reachable according to policy;
- archive generation refuses symlinks/special files/path traversal as
  appropriate;
- no `.git` credentials, SSH state, `.env`, home or Jarvis secrets enter the
  archive;
- archive is bounded;
- snapshot hash/base SHA is recorded;
- private-agent source and protected Core/persona paths stay outside coding
  snapshots unless a separately reviewed coding policy explicitly permits the
  repository and still protects immutable paths.

No arbitrary "repo path" field may be added to the public coding API.

---

# 9. Add a real Codex run registry and cancellation/status path

The existing raw broker socket intentionally refuses unauthenticated raw
status/cancel. Keep that property.

Add/use a trusted run registry so the authenticated Core/API path can expose:

- queued/preparing/running/completed/failed/timed_out/cancelled;
- coding session ID;
- run ID;
- repository + exact base SHA;
- sanitized checkpoint/progress;
- test/result metadata;
- artifact references;
- no token/credential/raw hidden reasoning.

Cancellation must:

- revoke run capability;
- terminate disposable sandbox;
- stop further broker use;
- release/adjust budget reservation;
- produce one final audited state.

A broker restart revokes outstanding ephemeral capabilities and must reconcile
stale durable runs safely.

---

# 10. Finish subscription-backed Codex integration without exposing auth

Use the already-linked `jarvis-codex` official runtime/auth state.

Do not:

- copy `/var/lib/jarvis-codex` into a sandbox;
- mount the Codex auth store;
- pass access/refresh tokens as env/input/artifact;
- fall back to `OPENAI_API_KEY`;
- execute Codex directly as Core.

The task proxy/broker owns the provider interaction.

If the official Codex runtime/app-server offers a supported local interface,
use its reviewed fixed protocol behind the broker. Verify the exact installed
version/capabilities; do not assume undocumented flags or JSON shapes.

A login failure/plan exhaustion must be reported as a safe structured
subscription state, not paid fallback.

---

# 11. OpenSandbox Home Node activation must become explicit and verifiable

Today `verify-home-node.sh` assumes OpenSandbox must always be disabled.
Replace that unconditional rule with a deliberate state model.

Conceptually:

```text
execution disabled:
  OpenSandbox disabled/inactive is valid

execution explicitly activated:
  OpenSandbox service healthy
  loopback/private control plane only
  authenticated lifecycle API
  approved runtime/isolation present
  Codex broker/task proxy healthy
  egress policy verified
  disposable smoke run verified
```

Do not auto-enable OpenSandbox during ordinary Core updates.

Activation is owner-operated after successful preflight.

Add a safe preflight/verification command suitable for the owner to run on the
UM890 before enabling production execution.

---

# 12. Preserve the Kata / runtime acceptance gate

Do not claim production-grade sandbox isolation merely because Docker/runc works.

The existing Ubuntu runtime verification expects proof of the selected stronger
runtime (for example the currently intended Kata configuration), storage quota
and networking restrictions.

Code/CI should test everything possible without a physical Home Node.

The final physical/runtime proof remains an explicit owner acceptance step:

- selected isolation runtime actually in use;
- no host/private network escape;
- storage quota proven;
- resource limits enforced;
- disposable lifecycle proven.

Do not weaken the requirement because GitHub-hosted CI cannot reproduce it.

---

# 13. Adversarial sandbox tests

Add automated tests/fixtures for everything CI can prove and a deterministic
Home Node acceptance script for the rest.

At minimum prove:

1. no unauthenticated OpenSandbox lifecycle request;
2. OpenSandbox manager remains loopback/private;
3. sandbox cannot reach host loopback;
4. sandbox cannot reach RFC1918;
5. sandbox cannot reach link-local / cloud metadata ranges;
6. DNS rebinding/private resolution is denied;
7. Docker socket is absent;
8. `/etc/jarvis` is absent;
9. owner home is absent;
10. provider auth stores are absent;
11. capability for run A fails for run B;
12. wrong repository fails;
13. wrong base SHA fails;
14. wrong reservation fails;
15. wrong operation fails;
16. expired capability fails;
17. replayed request ID fails;
18. cancelled/completed capability fails;
19. budget overflow fails;
20. broker restart revokes ephemeral capabilities;
21. arbitrary model/URL/header/body cannot be selected;
22. artifact traversal/symlink/oversize fails;
23. timeout always terminates sandbox;
24. cancellation always terminates sandbox;
25. failure leaves no reusable secret in artifact/log/output;
26. no host-process Codex fallback exists.

---

# 14. Complete release/update packaging for the new execution pieces

Any new broker/proxy/runtime binary, systemd unit or protected configuration
contract must participate in the existing checksummed release transaction.

Requirements:

- release manifest capability versioning;
- artifact SHA-256 coverage;
- exact unit/binary presence validation;
- same-version repair;
- upgrade activation;
- rollback restoration;
- legacy release compatibility;
- no automatic activation of optional execution services;
- no symlink/substitution attack on installed binary/unit paths.

Fix the currently failing subscription-worker fixture as part of this work and
ensure the new sandbox components receive equivalent regression coverage.

---

# 15. Context/cost telemetry

For every model/worker/run, preserve the distinction:

```text
local
subscription
metered_api
unknown
```

Subscription workers may report token/call/latency/limit telemetry but must not
increase the EUR API spend ledger unless actual metered API usage occurred.

Unknown billing is never silently treated as free.

For long Codex tasks, keep bounded reservation/checkpoint accounting. A
subscription entitlement can have usage limits even if its marginal API cost is
zero.

---

# 16. Do not finish by merely changing documentation

This goal is implementation work.

Updating a goal/README/status to say "ready" is not completion.

At the final PR head, inspect the code paths and demonstrate:

- Claude route no longer executes the CLI inside Core;
- account lifecycle exists only behind owner-admin tooling;
- Codex broker is no longer unconditionally deny-only when all reviewed
  production gates are configured;
- an approved Codex task can traverse the real sandbox lifecycle using only a
  short-lived capability;
- the sandbox never receives the persistent provider credential;
- long delegated tasks use bounded context/checkpoint + delta;
- services stay disabled until explicit owner activation;
- all CI checks are green.

---

# 17. Final owner acceptance checklist to leave for Gus

Do not perform these actions yourself. Produce exact commands/UI steps for the
owner after merge/release.

The checklist should include:

### Claude
- install/verify reviewed official CLI;
- connect Claude from Core Admin;
- confirm subscription auth;
- disable/cap provider extra usage externally;
- explicitly enable the Claude worker socket;
- run one harmless bounded Jarvis prompt;
- verify subscription compute telemetry and zero metered-API charge in Jarvis.

### Codex
- install/verify reviewed official Codex runtime;
- connect ChatGPT/Codex from Core Admin;
- run OpenSandbox/Kata preflight;
- explicitly activate OpenSandbox/Codex services;
- run one disposable read/test-only coding task against a non-production
  fixture repository;
- verify sandbox termination, artifacts and no host fallback.

### Persona
- explicitly note that `Jarvis.md` management remains owner-run and is not part
  of this goal.

---

# Definition of done

PR #58 may leave only owner-controlled live acceptance steps.

It is **not** done while any of these remain:

- required GitHub CI red;
- Codex broker unconditionally deny-only due missing task-scoped proxy;
- no trusted repository snapshot source;
- no real sandbox run lifecycle;
- no bounded resume/context path;
- release artifacts incomplete;
- OpenSandbox verification cannot distinguish disabled vs explicitly activated;
- a subscription failure can silently create API spend;
- a sandbox can receive persistent provider credentials;
- arbitrary host execution remains as a fallback.

It **is** code-ready when all implementation/security/test gates above are
complete, all CI is green, PR #58 can be marked ready for review, and the only
remaining actions require the owner's real provider accounts or physical Home
Node acceptance.
