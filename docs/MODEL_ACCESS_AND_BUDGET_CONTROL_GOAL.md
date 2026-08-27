# Goal extension: Model access control, provider discovery, and budget-aware long-task planning

This document is REQUIRED scope for PR #26 together with `docs/INTELLIGENT_MODEL_ROUTER_GOAL.md`.

The owner must be able to decide exactly which provider models Jarvis is allowed to use after provider credentials are configured. Jarvis must also reason about projected cost before committing to long-running or high-volume API work.

## Owner-controlled model access

Credentials grant Jarvis permission to authenticate to a provider; they do NOT automatically grant permission to use every model exposed by that provider.

Introduce a persistent owner-controlled model policy with three states where useful:

- discovered: provider reports or registry knows the model exists
- enabled: owner explicitly allows Jarvis to use it
- disabled: Jarvis must never route to it

Default remote-provider behavior should be fail-closed for newly discovered paid models: new models are visible to the owner but are NOT automatically enabled merely because the provider added them.

Local Ollama models may use a separate configurable default policy, but the owner must still be able to enable/disable individual local models.

The router may only choose models that are BOTH:

1. available/healthy enough for the requested operation; and
2. explicitly enabled by owner policy.

No agent, prompt, provider response, fallback path or model alias may bypass this allowlist.

## Model discovery

After credentials are configured, support provider model discovery through official model-list endpoints where providers expose a suitable endpoint.

When a provider does not expose reliable model discovery, merge a maintained versioned model registry with configured/discovered data and clearly label the source.

Never silently invent capabilities or prices for a newly discovered model.

Store enough metadata to present:

- provider
- model ID
- display name if known
- discovery source
- discovered timestamp
- enabled/disabled state
- context window if known
- supported modalities/capabilities if known
- quality tier/policy tags
- price metadata status
- health/availability state

Refresh discovery manually and periodically with a conservative cadence. A provider listing outage must not erase the owner's existing allowlist.

## Owner management interface

Add a safe management interface. CLI support is required; authenticated app/API support should be designed so the Jarvis UI can later render checkboxes/toggles.

Conceptual CLI:

```bash
sudo jarvis-models refresh [provider]
sudo jarvis-models list [provider]
sudo jarvis-models enable <provider> <model>
sudo jarvis-models disable <provider> <model>
sudo jarvis-models show <provider> <model>
```

Example output:

```text
PROVIDER   MODEL                 ENABLED   HEALTH     PRICE
OpenAI     model-a               yes       healthy    known
OpenAI     model-b               no        healthy    known
Anthropic  model-c               yes       healthy    known
xAI        model-d               no        unknown    unknown
```

Never print credentials.

The authenticated owner API should expose the same non-secret state and permit owner-authorized enable/disable updates, protected by the existing trusted-device/authentication model. Do not expose model-policy mutation publicly without owner authentication.

The UI should eventually be able to show provider sections with checkboxes/toggles for available models. The backend must be authoritative; UI state alone is never authorization.

## Aliases and model replacement

Do not let mutable provider aliases bypass owner intent.

If a configured alias can move to a materially different model, either:

- resolve and persist the concrete model identity where the provider makes this possible; or
- treat the alias explicitly as an owner-enabled alias and surface that it may change.

A newly appearing replacement model must not inherit allowlist permission simply because its name is similar.

## Budget configuration

Add owner-configurable spending controls that are independent of provider billing limits.

At minimum support:

- global monthly soft budget
- global monthly hard budget
- optional provider monthly caps
- optional model monthly caps
- per-request/task hard cost cap
- optional long-running job cost cap

Prefer storing budget/policy values separately from credentials.

The existing monthly budget concept should migrate into this policy cleanly.

Soft budget means Jarvis should become increasingly cost-conscious while still preserving required quality/safety.

Hard budget means Jarvis MUST stop creating new paid model work that would exceed the configured hard cap, except an explicitly owner-approved override if the product supports one.

Local no-cost models must be represented distinctly from unknown-cost remote models. Unknown cost is NOT zero.

## Spend accounting

Maintain spend state using provider usage/token responses and the versioned pricing registry.

Track at least:

- spent this month globally
- spent per provider
- spent per model
- reserved/projected cost for active long-running jobs
- actual cost after completion
- remaining soft/hard budget

Avoid double-counting retries/fallbacks. Each provider call should have a unique accounting record.

If provider billing reports differ from local estimates, support reconciliation without destroying the audit trail.

## Preflight planning for expensive or long tasks

Before Jarvis begins a task likely to generate substantial paid API usage, perform a bounded cost/benefit preflight.

Examples:

- "work on this API for three hours"
- large repository analysis
- long multi-agent research
- repeated backtesting analysis
- many parallel model calls
- high-context code migration

The preflight should estimate, as reasonably possible:

- expected duration
- likely number of model calls
- likely input/output token ranges
- expected provider/model mix
- estimated low/likely/high cost
- current monthly spend
- remaining budget
- percentage of monthly budget the task may consume
- cheaper qualifying alternatives
- whether local execution can replace some remote calls
- whether the expected value/importance justifies the spend

Do not pretend the estimate is exact. Represent uncertainty/range explicitly.

## Jarvis decision policy for long tasks

Jarvis should answer a question such as "is it sensible to work on this for three hours?" using budget and task value, not merely whether sufficient balance remains.

Conceptually consider:

- importance/owner intent
- urgency
- quality requirement
- estimated API spend
- remaining monthly budget
- availability of cheaper qualifying models
- expected improvement from using a more expensive model
- ability to split the work into checkpoints
- ability to use local Ollama/OpenSandbox/Codex compute instead of paid tokens

The output should be a concise policy decision, not hidden chain-of-thought. Example:

```text
Projected cost: €3.20–€6.80
Monthly remaining hard budget: €42.10
Recommendation: proceed
Plan: GLM for implementation loops, frontier model only for architecture review, checkpoint after 45 minutes.
```

or:

```text
Projected cost: €28–€55
Monthly remaining hard budget: €31
Recommendation: do not start full run
Alternative: run local analysis first and request owner approval before the paid synthesis stage.
```

## Reservations and race conditions

Long-running tasks must reserve projected budget so parallel agents cannot all independently assume the same remaining money is available.

Use an atomic budget reservation mechanism in SurrealDB or an equivalent transaction-safe layer.

Lifecycle conceptually:

1. preflight estimates cost
2. reserve a bounded amount
3. execute calls while tracking actual spend
4. adjust reservation as bounded checkpoints occur
5. release unused reservation on completion/cancel/failure

A process crash must not leave permanent stale reservations. Add expiry/reconciliation.

## Checkpoints for long tasks

Long-running paid jobs should work in stages rather than committing the entire budget blindly.

Support configurable checkpoints based on one or more of:

- elapsed time
- spend
- token count
- task milestones

At a checkpoint Jarvis should evaluate:

- progress achieved
- estimated remaining work
- actual spend versus estimate
- whether the current model mix remains sensible
- whether to continue, downgrade within quality constraints, escalate, pause, or ask owner approval

Do not automatically continue past the task hard cap.

## Approval thresholds

Support policy-driven owner approval for unusually expensive tasks.

Examples (configurable, not hard-coded product constants):

- projected task spend exceeds €X
- projected task spend exceeds Y% of monthly remaining budget
- high estimate crosses hard budget
- task asks for a disabled model

A disabled model should normally remain unavailable rather than being enabled by a one-off prompt. If one-off model approval is later supported, it must be an explicit authenticated owner action with expiry/scope.

## Router integration

Cost controls happen AFTER minimum capability/quality determination.

Correct order:

1. determine required capabilities/quality
2. filter to owner-enabled models
3. filter by provider/model health and task constraints
4. evaluate budget/cost
5. choose best qualifying model/mix
6. reserve budget if required
7. execute with bounded fallback/escalation

Never solve a budget shortage by silently using a model below the task's minimum quality/safety requirement.

If no enabled model satisfies the task inside budget, report that clearly and suggest options:

- enable another model
- increase task/month budget
- use local model where adequate
- reduce task scope
- defer the task

## Agent integration

An agent's `model_policy` may request capabilities/quality but does not grant access to disabled models.

Example:

```text
trading-risk agent -> requires strong reasoning
owner-enabled set -> {model X, model Y}
router -> chooses among X/Y only
```

If all eligible models are disabled/unavailable, the agent fails closed with a useful reason.

## API/UI observability

Expose authenticated non-secret state so the app can show:

- configured providers
- discovered models
- enabled models
- provider/model health
- current monthly spend
- soft/hard budget
- remaining budget
- active task reservations
- current task projected/actual spend

For each response/run, include concise fields where useful:

- selected model
- why that model was eligible
- projected cost
- actual estimated cost
- budget remaining
- whether a cheaper qualifying option existed

Do not expose hidden reasoning traces.

## Safe defaults

On initial deployment/upgrade:

- existing working local Ollama behavior should remain available where appropriate
- remote provider credentials alone must not enable every remote model
- no paid task should be allowed to interpret an unknown price as free
- monthly hard budget must not default to unlimited without an explicit owner policy decision

Migration may preserve the existing configured monthly budget as an initial cap where safe and documented.

## Tests

Add tests for at least:

- model discovery without credentials
- model discovery with fake provider responses
- newly discovered paid model defaults disabled
- enabled model becomes router-eligible
- disabled model is never selected, including fallback/escalation
- provider refresh does not erase existing policy on transient failure
- model alias/replacement cannot silently inherit unintended access
- global/provider/model caps
- unknown pricing is not treated as zero
- projected long task over budget is rejected or requires owner approval
- projected long task within budget reserves funds
- parallel reservations cannot overspend the same budget
- cancellation releases reservation
- stale reservations reconcile/expire
- checkpoint stops task at hard cap
- router quality floor remains enforced under budget pressure
- agent model policy cannot bypass allowlist
- owner API/CLI can enable/disable models without exposing credentials

## Definition of done extension

PR #26 is not complete until:

- credentials and model authorization are separate concerns
- provider models can be discovered/refreshed where supported
- owner can explicitly enable/disable models
- newly discovered paid models do not auto-enable
- router can only use owner-enabled models
- UI/API can represent model toggles safely
- monthly soft/hard budgets are configurable
- provider/model/task caps are supported
- Jarvis estimates costly long-running tasks before execution
- Jarvis can recommend whether a multi-hour API task is sensible given remaining budget and expected value
- long jobs reserve budget atomically
- long jobs have spend/progress checkpoints
- hard caps cannot be crossed silently
- disabled models cannot be reached through fallback, aliases, agents or prompt injection
- tests cover allowlist and budget-race security properties
