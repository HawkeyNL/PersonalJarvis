# Goal: Owner-selectable brain, persistent Codex coding sessions, and automatic private-agent deployment

Implement three related operational capabilities for PersonalJarvis:

1. the owner can choose/control Jarvis's brain/model policy from the Jarvis app/API;
2. coding implementation tasks always use the trusted Codex execution path with resumable/summarized coding sessions rather than arbitrary Jarvis model host execution;
3. approved pushes to `PersonalJarvisAgents/main` can be automatically validated, staged and activated on the Home Node without giving Jarvis Core GitHub credentials.

Work against CURRENT main and coordinate cleanly with the intelligent model-router/model-access work. Do not duplicate or weaken that architecture.

## 1. Owner-selectable brain

The owner must be able to configure the default Jarvis brain from the authenticated Jarvis app/API.

Support conceptually:

- `Auto` — intelligent router chooses from owner-enabled models;
- explicit model pin — owner selects one enabled provider/model as the default conversational brain;
- `Deep` / `Research` request overrides remain available where supported;
- optional per-conversation model pin, distinct from global default;
- easy return to `Auto`.

Only models explicitly allowed by the model-access policy may be selected. A client must never be able to select a disabled/unconfigured model by forging an API request.

Persist owner brain preferences as protected application configuration/state, with audit events for changes. Do not put this mutable preference in `Jarvis.md`; `Jarvis.md` defines identity/persona/invariants, not day-to-day provider selection.

Expose authenticated endpoints suitable for the Jarvis app to:

- list available + owner-enabled models;
- show provider health/cost metadata;
- read current global default brain policy;
- set global default to Auto or an allowed model;
- set/clear a conversation-level model override.

Return non-secret routing metadata with responses so the app can show which brain was actually used and whether fallback/escalation occurred.

## 2. Coding policy: Codex is the implementation executor

Add a protected Core policy/invariant that implementation-oriented coding tasks are delegated to the trusted Codex coding runtime.

Examples that should use Codex:

- implement/fix/refactor code in a repository;
- run repo tests and iterate on failures;
- create a branch/commit/PR when authorized;
- long coding tasks;
- repository modifications.

Pure explanation/review questions may be answered by Jarvis/model routing without launching Codex when no repo modification/execution is required.

Do not rely only on wording in a Markdown prompt for this security/behavioral rule. Enforce the routing policy in trusted Core code. Documentation/persona may describe the behavior, but the executable policy is authoritative.

## 3. Codex must execute inside the sandbox boundary

Production coding execution must use the existing OpenSandbox/Codex security architecture.

Conceptual path:

Jarvis Core
-> coding task classifier/policy
-> budget + approval decision
-> Codex broker
-> isolated worktree/task inputs
-> OpenSandbox Codex runtime
-> tests/diff/artifacts
-> trusted validation
-> Jarvis report

Do not fall back to arbitrary host shell execution if the sandbox/Codex runtime is unavailable.

The sandbox image/runtime may contain the Codex executable, but never bake long-lived credentials into the image.

Do not mount the owner's complete `~/.codex`, SSH directory, GitHub credentials, `.env`, `/home`, Docker socket or Jarvis provider credential store into coding sandboxes.

Use narrowly scoped/task-scoped authentication or the existing broker boundary. Preserve approvals and protected Core/worktree paths.

## 4. Persistent logical Codex conversations

Jarvis should maintain a logical coding conversation/session for each coding project/task so follow-up requests do not start from zero.

Persist trusted session metadata in SurrealDB, conceptually:

- session ID
- owner ID/device context where appropriate
- repository/project identity
- base/head revision
- task objective
- created/updated timestamps
- status
- approvals
- sandbox/Codex run IDs
- concise running summary
- important decisions
- unresolved issues/TODOs
- tests/results
- artifact/diff references
- token/cost/budget telemetry

Do NOT persist hidden chain-of-thought. Store concise factual summaries, decisions and outputs only.

A sandbox process itself may remain disposable. Persistence belongs to Jarvis's trusted session state, not to keeping an unsafe container alive forever.

## 5. Context suspension / compaction

Long Codex coding conversations must support context compaction.

When a coding session grows large, Jarvis should create/update a structured checkpoint summary containing at least:

- objective
- current repo/ref
- what has been implemented
- architecture decisions
- files/areas changed
- tests passing/failing
- errors/blockers
- owner decisions/constraints
- pending work
- relevant artifact/diff references

On the next Codex run, reconstruct bounded context from:

1. the original objective;
2. latest trusted checkpoint summary;
3. current repository/worktree state;
4. relevant recent messages/decisions;
5. explicit owner constraints.

Never pretend a summary contains details that were not preserved. Codex can re-read the repo when necessary.

## 6. Resume behavior

The owner should be able to say conceptually:

- `Continue the coding task`;
- `Resume project X`;
- `What did Codex do last time?`;
- `Continue from the failing tests`.

Jarvis resolves the correct logical coding session and starts a fresh isolated Codex sandbox run with reconstructed context.

Do not require the same sandbox/container to survive between sessions.

Support cancellation, pause/suspend and explicit close/archive.

## 7. Coding budget integration

Before a long Codex job, integrate with the model/budget governance layer.

Estimate and expose:

- likely duration range;
- likely API/model cost range;
- reserved budget;
- planned model/Codex usage;
- checkpoint thresholds.

Long tasks should periodically checkpoint progress/cost. If actual spend materially exceeds the estimate or approaches a configured task/month budget, pause/stop and request owner approval according to policy rather than running indefinitely.

## 8. App/API coding session UX

Expose authenticated APIs suitable for the Jarvis app to show:

- active/suspended/completed Codex sessions;
- project/repository;
- objective;
- progress/status;
- current checkpoint summary;
- last test status;
- estimated/actual spend;
- pause/resume/cancel controls;
- relevant PR/commit/artifact references.

Do not expose secrets, raw hidden reasoning or unsafe shell access.

## 9. PersonalJarvisAgents automatic deployment

Implement a safe automatic update path for the private `PersonalJarvisAgents` repository when `main` changes.

Important trust boundary: Jarvis Core itself must NOT receive a GitHub token/SSH key and must NOT `git pull` the private repository.

Use a separate root/trusted updater mechanism, analogous to the existing public release updater where appropriate.

Preferred architecture:

GitHub `PersonalJarvisAgents/main`
-> trusted updater detects new commit
-> fetches with updater-only read credential
-> stages into temporary location
-> validates entire private agent bundle with `jarvis-agent-bundle`
-> verifies schema/policy/protected constraints
-> builds immutable bundle
-> root-owned read-only permissions
-> atomic activation of `current`
-> Core reload/restart only if required
-> `/livez` + `/readyz` health check
-> retain previous known-good bundle for rollback

If validation/health fails, do NOT activate or keep the broken bundle active. Roll back atomically and surface diagnostics.

## 10. Update trigger

Support automatic detection of a push to `PersonalJarvisAgents/main` without exposing a public unauthenticated deployment endpoint.

Evaluate and document the safest practical mechanism for the Home Node, e.g.:

- GitHub webhook through the authenticated public Jarvis gateway with signed webhook validation and a narrow updater trigger; or
- scheduled polling by a separate updater service using conditional requests/commit SHA checks.

Do not require Jarvis Core to hold the repo credential.

For the first Home Node, prefer the simplest robust mechanism with a small attack surface. If polling is chosen, use a reasonable interval and avoid unnecessary API traffic. If webhook is chosen, verify signatures, replay protection and exact repository/ref before triggering.

Never deploy feature branches automatically. Only the explicitly configured repository + `refs/heads/main` is eligible.

## 11. Private repo credential

Provide a root-operated setup path for the private-agent updater credential, preferably integrated with the secure credential-management conventions but isolated from model-provider credentials.

The credential should be read-only and repository-scoped where GitHub supports it.

It must not be readable by:

- ordinary user without sudo;
- Jarvis Core;
- OpenSandbox workloads;
- Codex sessions;
- private agents.

Never log the credential.

## 12. Agent update observability

Jarvis may know/report non-secret deployment status even though Core cannot fetch the repo itself.

Expose conceptually:

- active bundle ID;
- source commit SHA;
- agent count;
- last successful update time;
- last attempted update time;
- update status/failure category;
- previous known-good bundle;
- whether an update is pending.

The Jarvis app can show this in a system/settings page.

## 13. Agent update audit + rollback

Audit:

- update detected;
- source commit;
- validation result;
- activation;
- health result;
- rollback;
- failure reason.

Keep at least a small bounded number of previous immutable bundles or a safe cleanup policy.

Provide an owner-authorized rollback command/API that can atomically switch to a previous validated bundle. Do not permit arbitrary filesystem paths as rollback targets.

## 14. GitHub source integrity

Do not blindly trust arbitrary content merely because it is in the private repository.

Continue to validate the private agent schema and protected runtime rules. The private repository cannot grant itself new Core capabilities, credentials or security exemptions through Markdown/frontmatter.

Consider optionally requiring a known GitHub repository identity and, where practical, commit provenance/signature policy. Document the chosen tradeoff.

## 15. Relationship to Jarvis.md

Update Jarvis.md/documentation only for human-readable behavioral expectations such as:

- Jarvis uses Codex for implementation coding tasks;
- Jarvis can suspend/resume coding work;
- owner brain/model preferences are respected within security/budget policy.

Do NOT make Jarvis.md the sole enforcement mechanism. Core policy and authorization code must enforce these rules.

## 16. Tests

Add tests for at least:

- global Auto brain selection;
- explicit owner model selection;
- disabled model cannot be selected via forged API request;
- per-conversation override and clear;
- coding implementation task routes to Codex;
- pure coding explanation need not start Codex;
- unavailable Codex/OpenSandbox fails closed;
- coding session creation/resume/suspend/cancel;
- context checkpoint/compaction preserves factual state without hidden reasoning;
- resumed Codex run uses current repo state + checkpoint;
- task budget checkpoint pauses/stops correctly;
- private-agent updater only accepts configured repo/main;
- invalid bundle never activates;
- successful bundle activates atomically;
- Core health failure rolls back;
- updater credential is not readable by Core/jarvis/sandbox;
- no updater/model/Codex credentials appear in logs;
- reboot restores updater schedule/state;
- rollback only targets validated historical bundles.

Use mocks/local fixtures for GitHub in normal CI; do not require real private repo credentials.

## 17. Deployment

Extend idempotent Home Node deployment to install/configure:

- Codex sandbox runtime/broker wiring where not already installed;
- coding session storage/schema;
- private-agent updater service/timer or webhook receiver;
- updater-only credential location;
- verification checks;
- relevant CLI management commands.

Upgrade from the current Home Node must preserve SurrealDB, persona, active bundle, owner device state and unrelated credentials.

## Definition of done

Complete when:

- owner can choose Auto or an allowed explicit Jarvis brain through authenticated API/app-ready endpoints;
- explicit model selection cannot bypass owner model allowlist/provider configuration/budgets;
- implementation coding tasks are enforced through Codex rather than arbitrary host execution;
- Codex runs behind the existing sandbox/broker/approval boundary;
- logical coding sessions survive disposable sandbox runs;
- coding context can be safely compacted/suspended/resumed using factual summaries and current repo state;
- long coding jobs integrate budget estimation/reservation/checkpoints;
- app-ready APIs expose coding-session status and controls;
- `PersonalJarvisAgents/main` changes can automatically deploy through a separate trusted updater;
- Jarvis Core has no GitHub credential for the private repo;
- private bundles are validated, immutable and atomically activated;
- broken agent updates fail closed and roll back;
- updater status/commit/bundle are observable without exposing secrets;
- Jarvis.md documents behavior but Core code enforces it;
- security/regression/deployment tests pass;
- documentation matches the production architecture.
