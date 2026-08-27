# Model routing and credentials

Jarvis treats a provider credential and permission to use a provider's models
as two separate controls.  The Core always determines a task's minimum quality
first; it only then considers exact provider/model pairs the owner enabled.
Credentials, prompts, agents, aliases and fallback paths cannot bypass this
allowlist.

## Home Node operations

On a provisioned Home Node, provider keys live in separate
`/etc/jarvis/secrets/<provider>.env` files.  They are `root:jarvis 0640` in a
`root:jarvis 0750` directory: Core can read the particular EnvironmentFiles it
needs, but cannot change them.  They are not in the unit, releases, logs,
database telemetry, agent bundles, sandbox workloads or Codex worktrees.

Use a controlling terminal; keys are never accepted as normal CLI arguments:

```bash
sudo jarvis-credentials list
sudo jarvis-credentials set openai
sudo jarvis-credentials test openai
sudo jarvis-credentials remove openai
```

`set` installs a temporary root-only file atomically, restarts Core and waits
for `/livez` and `/readyz`.  A failed restart restores the old credential
state. `test` intentionally does not perform a paid generation request.
Local Ollama has no credential. Remote Ollama is a distinct `ollama-cloud`
provider and must use an explicit credential and model allowlist entry.

## Model access

The root-owned `/etc/jarvis/model-policy.json` is the canonical policy.  The
first setup creates it through `jarvis-models refresh`. Configured remote models
are recorded as `discovered` but disabled. Local Ollama is available by default
unless the owner explicitly disables its exact model.

```bash
sudo jarvis-models refresh
sudo jarvis-models list
sudo jarvis-models enable openai-api gpt-4o-mini
sudo jarvis-models disable openai-api gpt-4o
sudo jarvis-models show openai-api gpt-4o-mini
```

The policy matches the literal provider and model ID. A newly listed or renamed
model does not inherit another model's permission. Refresh retains existing
entries if a provider/discovery operation is unavailable.

## Routing, health and spend

Requests use `auto` (default), `fast`, `deep`, or `research`. Older `tier`
hints map to the same quality floor. The selected mode is returned as
non-secret response metadata; provider/model selection is internal and never
reveals keys or hidden reasoning.

Metered execution is accounted per backend/model. Local Ollama and the local
subscription CLI are distinct from paid API backends. Unknown remote prices are
explicitly marked unknown and are conservatively charged for accounting; they
are never treated as free. The monthly hard cap remains a fail-closed stop,
with a soft threshold for cost-aware selection and a per-request hard cap.

The current in-process reservation gate prevents concurrent requests from
oversubscribing the Home Node's configured monthly ceiling and reconciles with
durable SurrealDB usage after replies. Long-task projections are also retained
in the private `llm_budget_reservations` table with an expiry/release lifecycle,
so a restart cannot silently turn a reservation into permanent spend state.
Long-running multi-agent jobs remain owner-approved work: do not treat the
generic chat endpoint as an unrestricted background spend executor.

## Security notes

Provider output is untrusted. Model routing changes no `jarvis-policy`, signed
approval, OpenSandbox, protected persona, agent-bundle or Codex boundary.
Provider keys must never be passed into an agent, sandbox, shell command,
browser context, worktree or prompt.  The public release/update path does not
write `/etc/jarvis/secrets` or `/etc/jarvis/model-policy.json`.
