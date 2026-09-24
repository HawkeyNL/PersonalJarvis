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

`set` first probes the candidate credential using metadata only. A rejected or
unverifiable candidate leaves the existing credential untouched. It then
installs a temporary root-only file atomically, restarts Core and waits
for `/livez` and `/readyz`.  A failed restart restores the old credential
state. `test` performs a bounded authenticated metadata probe (`/models` where
the provider supports it; Anthropic's model-list endpoint otherwise) using an
ephemeral root-only curl config, then checks Core health. It intentionally does
not perform a paid generation request or print a provider response. After a
successful `set`, the CLI refreshes that provider's model catalog; discovered
models remain disabled. If this refresh fails, the validated credential remains
saved and the command reports the partial failure with a retry command.
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

### Hourly metadata refresh

Releases declaring `tooling.model_catalog: 1` include verified
`jarvis-model-catalog.service` and `.timer` units. Core starts the timer at boot
and restart. Its first run is after five minutes, then one hour after each
completed run, with up to 30 seconds of jitter. A single oneshot service and
the existing policy lock prevent overlapping writes. This does not restart
Core, generate completions, enable models or change owner-selected HF routes.

Only safely configured credential files are used. The seven cloud providers
are checked independently; a failed request retains the previous model policy,
does not prevent the remaining providers being checked, and fails the service
visibly. Requests have bounded time/size. Anthropic catalogs exceeding the
supported 1,000-record page are refused rather than silently truncated.

After installing such a release:

```bash
systemctl status jarvis-model-catalog.timer
systemctl list-timers jarvis-model-catalog.timer
sudo journalctl -u jarvis-model-catalog.service -n 50 --no-pager
sudo jarvis models refresh-configured
```

The last command runs the same configured-provider refresh manually. Existing
models absent from a later catalog are retained, not silently deauthorized.
HF route prices come from metadata; other provider prices remain reviewed
release snapshots, not hourly scraped billing pages. Rollback to a release
without this capability removes the new canonical timer/service files through
the verified unit manager; failed activation restores the backed-up unit set.

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
provider/model pairs. Fresh setup initializes an empty owner override registry;
reviewed defaults remain in the immutable release. Verified releases update
default rates while explicit owner entries continue to take precedence; the
effective source/date reports both layers. An owner can stage a reviewed
replacement atomically, retain the ownership/mode, then restart Core. Malformed
input falls back to the built-in conservative registry and is logged without
affecting availability. Unknown remote models remain explicitly unknown and
use conservative accounting rather than a fabricated zero price.

The packaged `deploy/systemd/pricing-registry.json` is also embedded in Core;
there is no separate Rust baseline to maintain. Per-entry `pricing_source`,
`pricing_updated_at` and `pricing_notes` identify the actual provider source
and conditions. Legacy entries inherit their original registry provenance before
layers are merged, so adding a new release does not make an old owner price
appear freshly reviewed. For the two historical setup catalogs dated August 27
and September 1, 2026, exact unmodified shipped entries are treated as defaults
only if their registry source/date also match. Changed entries are retained.
Set `owner_override: true` to deliberately pin even an unchanged historical
rate. Migration happens only when reading: no owner file is rewritten, so an
older binary can still read its previous configuration after rollback.

The September 24 catalog covers reviewed text models from OpenAI, Anthropic,
DeepSeek, xAI, Z.ai and Ollama Cloud. HF prices remain discovered per route;
there is intentionally no fabricated universal HF model price. Discovery and
pricing are separate: a newly discovered model is disabled and may have unknown
pricing until its exact ID is reviewed. No model-name prefix matching is used.

Shown rates are standard global USD/million text tokens, not a provider invoice.
Optional `long_context` rates include an inclusive `from_input_tokens`
threshold; accounting selects them using fresh plus cached input tokens, and
multi-call estimates apply the threshold per call, not to the sum of calls.
DeepSeek/Ollama time-dependent rates use the peak rate, classified conservative.
Missing cache discounts remain null in the UI and use the full input rate for
accounting, never an invented 90% discount. Unknown models have no unrelated
source/date label. Core Admin exposes per-model pricing details.

This is a release-reviewed snapshot, not a live price scraper. Changing a
credential or refreshing model IDs does not scrape billing pages or rewrite
owner prices. Cache-write fees, nonstandard service tiers, region premiums,
tools and taxes are not included in the displayed input/cache-read/output
columns; their treatment requires additional accounting before these estimates
can be considered invoice-equivalent. Configure appropriate owner rates for
nonstandard endpoints. Never interpret an unknown price as authorization or free
usage.

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
