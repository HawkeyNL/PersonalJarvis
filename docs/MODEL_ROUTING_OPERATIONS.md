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
sudo jarvis credentials list
sudo jarvis credentials set openai
sudo jarvis credentials test openai
sudo jarvis credentials remove openai
```

`set` installs a temporary root-only file atomically, restarts Core and waits
for `/livez` and `/readyz`.  A failed restart restores the old credential
state. `test` performs a bounded authenticated metadata probe (`/models` where
the provider supports it; Anthropic's model-list endpoint otherwise) using an
ephemeral root-only curl config, then checks Core health. It intentionally does
not perform a paid generation request or print a provider response.
Local Ollama has no credential. Remote Ollama is a distinct `ollama-cloud`
provider and must use an explicit credential and model allowlist entry.

## Model access

The root-owned `/etc/jarvis/model-policy.json` is the canonical policy.  The
first setup creates it through `jarvis-models refresh`. Configured remote models
are recorded as `configured` or `provider_api` but disabled. Local Ollama is available by default
unless the owner explicitly disables its exact model.

```bash
sudo jarvis models refresh
sudo jarvis models list
sudo jarvis models enable openai-api gpt-4o-mini
sudo jarvis models disable openai-api gpt-4o
sudo jarvis models show openai-api gpt-4o-mini
```

The policy matches the literal provider and model ID. A newly listed or renamed
model does not inherit another model's permission. Refresh retains existing
entries if a provider/discovery operation is unavailable.

### App-mediated owner changes

The Home Node contains a minimal local root broker for app model toggles. It is
a Unix-socket service only: no HTTP listener, shell, arbitrary path,
environment or command operation. Its sole allowlisted operation changes the
enabled bit of an already discovered exact provider/model pair. A Bearer session
is never sufficient. The owner device signs a domain-separated canonical
payload containing action, payload hash, request ID, nonce, owner/device IDs,
issue/expiry times and the current policy SHA-256. The broker independently
checks the active device key, signature, TTL and one-time replay marker before
atomically replacing the policy; a changed policy requires a fresh signature.

Credentials remain root-TTY-only through `jarvis credentials`. They are not
sent through the app or broker until a separately reviewed sealed secret-transfer
protocol exists; there is deliberately no unsafe fallback.

## Routing, health and spend

Requests use `auto` (default), `fast`, `deep`, or `research`. Older `tier`
hints map to the same quality floor. A deterministic classifier establishes a
minimum quality floor from the original request before cost is considered; it
does not replace the message with a summary. In particular, Fast cannot reduce
deterministically safety-sensitive, research, coding or complex work below its
required floor. The selected mode is returned as non-secret response metadata;
provider/model selection is internal and never reveals keys or hidden reasoning.

Provider faults are classified without logging response bodies. Authentication,
rate-limit, transport and temporary availability failures receive a bounded
in-process cooldown, so Core falls back only to another enabled provider that
still meets the task floor instead of retrying a known-bad credential on every
request.

Metered execution is accounted per backend/model. Local Ollama and the local
subscription CLI are distinct from paid API backends. Unknown remote prices are
explicitly marked unknown and are conservatively charged for accounting; they
are never treated as free. The monthly hard cap remains a fail-closed stop,
with a soft threshold for cost-aware selection and a per-request hard cap.

`/etc/jarvis/pricing-registry.json` is a root-owned, Core-readable (`0640`)
versioned registry with a source note and update date. Entries are exact
provider/model pairs. It is initialized once during setup and an explicit owner
entry is never overwritten by a release. Verified releases may add reviewed
exact-model coverage for pairs that are absent from the owner registry; the
effective source/date reports both layers. An owner can stage a reviewed
replacement atomically, retain the ownership/mode, then restart Core. Malformed
input falls back to the built-in conservative registry and is logged without
affecting availability. Unknown remote models remain explicitly unknown and
use conservative accounting rather than a fabricated zero price.

Core persists bounded monthly aggregates for requests, input/output/cache
tokens and estimated spend. The aggregate contains no prompts, replies,
credentials or request identifiers. Core Admin exposes it in **Usage & Costs**;
the regular Jarvis app shows a compact summary on Status. Provider invoices
remain authoritative because pricing and provider token reporting can change.

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
