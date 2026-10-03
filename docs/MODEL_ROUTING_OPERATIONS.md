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
environment or command operation. It has two allowlisted operations:
`model.set_enabled` changes the enabled bit of an already discovered exact
provider/model pair, and `model.routing_set` replaces the owner routing
document (see [Owner routing](#owner-routing-routingjson)). A Bearer session
is never sufficient. The owner device signs a domain-separated canonical
payload containing action, payload hash, request ID, nonce, owner/device IDs,
issue/expiry times and the current policy SHA-256. The broker independently
checks the active device key, signature, TTL and one-time replay marker before
atomically replacing the policy; a changed policy requires a fresh signature.

Credentials remain root-TTY-only through `jarvis credentials`. They are not
sent through the app or broker until a separately reviewed sealed secret-transfer
protocol exists; there is deliberately no unsafe fallback.

## Owner routing (`routing.json`)

`/etc/jarvis/model-policy/routing.json` (`root:jarvis 0640`, next to
`policy.json`, same directory lock; Core setting `llm_model_routing_path`)
lets the owner choose the exact provider/model order per tier and switch paid
APIs off. It only orders candidates. It never enables a model: every attempt
still has to pass the allowlist (`enabled`), the monthly cap, availability and
health.

```json
{"version": 1,
 "paid_api": "allowed",
 "tiers": {
   "cheap": {"chain": [{"provider": "zai-api", "model": "glm-5.3-flash"},
                       {"provider": "claude-cli", "model": "claude-haiku-4-5"}],
             "metered_after_subscription": false},
   "hard":  {"chain": [{"provider": "claude-cli", "model": "claude-opus-5"}]}}}
```

Validation (Core, broker and CLI apply the same rules): only these fields;
`version` is 1; at most 64 KiB; tiers are `cheap`, `default` and `hard`; a
missing tier keeps the built-in order for that tier; a chain has 1 to 9
entries; providers are the routable provider IDs (`anthropic-api`,
`openai-api`, `deepseek-api`, `xai-api`, `zai-api`, `ollama`, `ollama-cloud`,
`huggingface`, `claude-cli`); a model has 1 to 256 characters and no control
characters; no duplicate pairs. A metered entry after a subscription entry
(`claude-cli`) is refused unless the tier sets
`"metered_after_subscription": true`, so a full plan never silently becomes a
paid call. The CLI and the broker also require every routed pair to be
discovered in `policy.json`.

`paid_api` is the "subscriptions and local only" switch. `"allowed"` is the
default when the field or the file is absent. `"off"` removes every metered
backend (everything except local `ollama` and subscription `claude-cli`) from
every tier, routed chains and the built-in order alike. The switch is
re-checked before every attempt, so turning it off also stops a request that
is already falling back. A brain pin or explicit provider that selects a
metered backend is then refused with a bounded `409 paid API is off` error
instead of being rerouted. A brain pin names its provider: it is tried on that
provider only and never falls back to another one. When the monthly cap is
reached and nothing is available, Core never tries a metered backend anyway.

Failure behaviour:

| State | Behaviour |
| --- | --- |
| File absent | Built-in order, paid APIs allowed. |
| Valid | Routed chains and switch as written. |
| Invalid, oversized, unsafe owner/mode, symlink or unreadable | Built-in order **without** metered backends; logged; `routing_unavailable_reason` reports it. |
| Signed change could not be verified | Same fail-closed state (`routing_activation_unverified`) until the owner verifies routing and restarts Core. |

Core reads routing only at startup and through the signed broker path; there
is no file watcher. `GET /v1/system/models` reports `routing`,
`routing_sha256`, `routing_unavailable_reason` and `routing_mutation`
(`device-signed-model-route-v1` when the broker is available and the file is
readable, otherwise `unavailable`).

### CLI

```bash
sudo jarvis models route list
sudo jarvis --json models route list   # stored document and reason code
sudo jarvis models route show cheap
sudo jarvis models route set cheap zai-api glm-5.3-flash claude-cli claude-haiku-4-5
sudo jarvis models route set default claude-cli claude-opus-5 anthropic-api claude-opus-5 --metered-after-subscription
sudo jarvis models route reset cheap
sudo jarvis models route paid-api off
```

Every command takes the policy directory lock. Changes validate the complete
result, require discovered pairs, write atomically as `root:jarvis 0640` and
restart Core. A failed restart stops Core rather than leaving an old route
live. The CLI refuses to edit an existing invalid or unsafe file; remove it as
root or replace it through a signed app change.

### Signed app change (`model.routing_set`)

The app replaces the whole document. The approval uses the same
`jarvis-privileged-config-v1` message as `model.set_enabled`, with action
`model.routing_set`, the SHA-256 of the canonical payload, and as state hash
the `routing_sha256` from `GET /v1/system/models`: SHA-256 of the exact current
file bytes, of empty bytes when the file is absent, and also of invalid bytes,
so a broken file can be repaired. It is `null` only when the file cannot be
read safely.

The canonical payload is compact JSON without whitespace, in this field order:
`action`, `routing` (`version`, `paid_api`, `tiers` with `cheap`, `default`,
`hard`; each tier `chain` of `provider`, `model`, then
`metered_after_subscription`), `expected_routing_sha256`. An absent tier is
omitted; `paid_api` and `metered_after_subscription` are always written. Only
`"` and `\` are escaped; `/` and non-ASCII characters are written as UTF-8.
Fixed vector (in `crates/client-core/src/model_control.rs`):

```text
{"action":"model_routing_set","routing":{"version":1,"paid_api":"off","tiers":{"cheap":{"chain":[{"provider":"huggingface","model":"org/modèl"},{"provider":"claude-cli","model":"claude-haiku-4-5"}],"metered_after_subscription":false},"hard":{"chain":[{"provider":"claude-cli","model":"claude-opus-5"},{"provider":"anthropic-api","model":"claude-opus-5"}],"metered_after_subscription":true}}},"expected_routing_sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}
SHA-256: a9c73476991bb884fbaf43378882c25f4403a8ce3daa44d1012221ce15e30a59
```

The broker checks signature, device, expiry and replay marker, takes the
directory lock, compares the state hash, validates the document, requires
every pair to be discovered and writes atomically (`0640`, the policy's
group). Core forwards the request, reads the file back and activates it only
if it is exactly the signed document and the live routing did not change
meanwhile. A lost reply, a different read-back or a concurrent change turns
paid APIs off (`routing_activation_unverified`). The signed request must fit
the broker's 16 KiB frame.

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
write `/etc/jarvis/secrets`, `/etc/jarvis/model-policy.json` or
`/etc/jarvis/model-policy/routing.json`.
