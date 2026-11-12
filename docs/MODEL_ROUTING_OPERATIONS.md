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
sudo jarvis models register codex-cli gpt-6-luna
```

Subscription providers (`claude-cli`, `codex-cli`) have no model catalog.
`refresh` and the hourly refresh record the Claude tier models configured for
Core (`JARVIS_LLM_MODEL`, `_HARD`, `_CHEAP`) as `claude-cli` pairs, because the
Claude worker runs those models. Any other subscription model is added with
`register <claude-cli|codex-cli> <model>`: one exact pair, recorded as
discovered and disabled with source `owner_registered`. The model id must be 1
to 80 of `A-Z a-z 0-9 . _ -` and must not start with `-` (the subscription
worker rule). Registering never enables a model or restarts Core; `enable`
does. Core Admin offers the same in the Models view.

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
The device key must also be pinned in the root-owned
`/etc/jarvis/trusted-device-keys`; see
[Trusted device keys](JARVIS_ADMIN_CLI.md#trusted-device-keys-for-privileged-changes).

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

Validation (Core, broker and CLI apply the same rules): only these fields
(plus the optional `research_web_search`, see
[Research web search](#research-web-search-off-by-default));
`version` is 1; at most 64 KiB; tiers are `cheap`, `default` and `hard`; a
missing tier keeps the built-in order for that tier; a chain has 1 to 9
entries; providers are the routable provider IDs (`anthropic-api`,
`openai-api`, `deepseek-api`, `xai-api`, `zai-api`, `ollama`, `ollama-cloud`,
`huggingface`, `claude-cli`, `codex-cli`); a model has 1 to 256 characters
and no control characters; no duplicate pairs. A metered entry after a
subscription entry (`claude-cli`, `codex-cli`) is refused unless the tier sets
`"metered_after_subscription": true`, so a full plan never silently becomes a
paid call. The CLI and the broker also require every routed pair to be
discovered in `policy.json`.

`paid_api` is the "subscriptions and local only" switch. `"allowed"` is the
default when the field or the file is absent. `"off"` removes every metered
backend (everything except local `ollama` and subscriptions `claude-cli` and
`codex-cli`) from
every tier, routed chains and the built-in order alike. `ollama` counts as
local only when `llm_ollama_url` points at a loopback host (127.0.0.0/8, `::1`
or `localhost`); otherwise Core treats it as metered at runtime. Validation
classifies by provider ID only, so Core, broker and CLI agree. A routed chain
may therefore list `ollama` after a subscription entry; with a non-loopback
URL Core skips that entry at runtime unless the tier sets
`metered_after_subscription`, and `paid_api: "off"` refuses it. Known gap:
usage accounting still counts every `ollama` call as local and free, so a
remote Ollama's spend does not count toward the monthly cap. Use
`ollama-cloud` for a billed remote Ollama, or keep `llm_ollama_url` on
loopback. The switch is
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
`routing_sha256`, `routing_unavailable_reason`, `research_web_search`
(`off`, `on` or `auto`, the effective state) and `routing_mutation`
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
sudo jarvis models route research-web-search on   # or off, auto
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
`metered_after_subscription`; then `research_web_search`),
`expected_routing_sha256`. An absent tier is omitted; `paid_api` and
`metered_after_subscription` are always written; `research_web_search` is
written only when `"on"` or `"auto"`, so the fixed vector below is unchanged.
An app that does not know the field drops it from a full replacement, which
turns research off (fail closed). Only
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

### Research web search (off by default)

`"research_web_search": "on"` is the owner's opt-in for web search (the apps
call it "Ask"); `"auto"` also lets Core search by itself (see
[Automatic web search](#automatic-web-search)). Absent or `"off"` is the
default; an unusable routing file keeps it off. Set it with
`sudo jarvis models route research-web-search off|on|auto`, in the Core Admin
model routing card or with a signed app change. Like every routing
change it restarts Core. Off -> on, off -> auto and on -> auto loosen the
switch, and clients confirm them first: native clients through
`ResearchWebSearch` in `jarvis-client-core` (ordered strictest first), Core
Admin through its own `RESEARCH_ORDER` in
`jarvis-core-admin/src/components/models/routing.ts`. Every routing change,
tighter or looser, needs the signed approval or the local root CLI.

When it is on, only a request with the explicit mode `research` searches. A
classifier that guesses "research" for an Auto request does not: that request
stays an ordinary hard-tier answer without tools. Its `assistant.completed`
event then carries a `research_offer` with the owner's question (only when a
research run could start now, see
[REALTIME_PROTOCOL.md](REALTIME_PROTOCOL.md)); the app shows a button, and
only the owner's click sends that question as a new `research` request. A
research request:

- sends **only the latest user question** (trimmed, at most 2000 characters;
  a longer question is refused, never cut) with a fixed research instruction
  that asks for an answer and a `Sources:` list of https URLs. No persona,
  memory, conversation history, device or location data goes with it. Core
  builds it through a separate code path (`ResearchRequest`) that has no field
  for anything else;
- runs only on a subscription worker that supports research (`claude-cli`,
  `codex-cli`), taken from the owner's `hard` chain in order (or the built-in
  hard order, which reaches `claude-cli`), with an owner-enabled model;
- never uses a paid API: metered providers have no web search here at all,
  and there is no paid fallback, `paid_api` and `metered_after_subscription`
  notwithstanding;
- answers with a fixed notice when research is off, the question is too long,
  or no capable subscription answered ("Research is off" / "Research is
  unavailable right now"), never with a silent non-search or paid answer.

**What leaves the Home Node.** The question and the fixed instruction go to
the subscription provider (Anthropic for `claude-cli`, OpenAI for
`codex-cli`). The provider runs the search on its own servers and may pass the
search queries, which can contain words from the question, to its search
partners. The Home Node itself fetches nothing: no browser, no local fetch.
The answer, with the source URLs the model reports, is stored in the
conversation like any other answer. Source URLs come from the web and are
untrusted: clients must render them only as plain text or as https links the
owner clicks (the safe Markdown renderer), never load them automatically (no
link previews, images, prefetch or embeds), so viewing an answer contacts no
third party. Turning the switch off also stops a research request that is
about to try its next subscription. A run that a worker stopped because it
used another tool or searched for a flagged query is never retried on another
subscription. Web content is untrusted: the model is told to
ignore instructions in it, and it cannot trigger any other tool. Core stores
the answer as `web:<model>` and later chat turns see it only as labelled,
capped untrusted data (see `SUBSCRIPTION_WORKERS.md`, "Research runs").

### Automatic web search

With `"auto"`, explicit Research works as with `"on"`, and Core may also start
a Research run by itself after an ordinary answer, so the latest question
leaves the Home Node without the owner's click. It does so only for a runs-API
answer (never `POST /v1/assistant/chat`) when all of these hold; otherwise the
answer carries the usual `research_offer`:

- research was advised and a research run can start now (a capable,
  owner-enabled subscription in the hard chain; the question is at most 2000
  characters). Research is advised when the intent classifier says so, or when
  the chat model ends its answer with a final line that is exactly
  `[[web_search]]` (below);
- the privacy gate passes (below);
- no other automatic run is in flight, and fewer than 10 automatic runs
  started in the last hour and 40 in the last day. Explicit Research runs do
  not count. The counters live in memory and reset when Core restarts.

**Model-requested search.** With `"auto"` and a research run possible, the
runtime facts of a runs-API answer tell the chat model that web search is
automatic: when a good answer needs current or external facts it does not
have, it answers briefly and ends its reply with a final line that is exactly
`[[web_search]]`. The model can only ask; the search still sends the owner's
latest question, never the model's text, and every check above still applies.
The same runtime facts give the current time to the hour and say that current facts
(prices, news, weather, schedules, status) in messages or web results over an
hour old are stale: the model asks for a new search instead of pointing to the
earlier answer.
This works without an intent classifier and with a pinned brain. Core removes
such a trailing marker line (surrounding whitespace allowed) from every chat
answer, whatever the routing, so it is never stored, streamed or shown; a reply that
was only the marker is stored as a fixed sentence. A marker anywhere else is
ordinary text and asks for nothing. An explicit Research answer never asks
for another search. With `"on"` or `"off"` the runtime facts do not mention
the marker.

The automatic run sends exactly what an explicit Research request sends (the
question and the fixed instruction), stores the answer as `web:<model>`,
records usage with `routing_mode` `research_auto`, and adds no user message.
How clients see it is in [REALTIME_PROTOCOL.md](REALTIME_PROTOCOL.md#automatic-web-search),
including the busy case: when the subscription worker's background slot is
taken, the run fails and nothing is stored. The web answer is written only
while the conversation still exists, in one transaction with the
conversation row; when the owner deleted it during the search, the run fails
and nothing is stored.

**Privacy gate.** A deterministic check keeps the question on the offer button
when it contains:

- an email address;
- a long number: 6 or more contiguous digits, or digit groups joined by up
  to three separators of which at most one is not whitespace (whitespace,
  including no-break spaces and tabs, `-`, `.`, `/`, `_`, and their Unicode
  and fullwidth look-alikes such as `–`, `—` and the minus sign) with 8 or
  more digits or 6 or more groups in total (`4111 1111 1111 1111`,
  `06 1234 5678`, `06 - 1234 - 5678`, `+31 6 12 34 56 78`, `1 2 3 4 5 6`, and
  the digits of `NL91 ABNA 0417 1643 00`). Any numeric character counts,
  fullwidth and superscript digits included. Invisible characters (control,
  zero-width and bidi characters, blank fillers, combining marks, variation
  selectors) are ignored, so they never split a number. A letter, a visible
  symbol (`€`, `×`, `→`) or other punctuation such as `,` ends a number:
  this is a heuristic, not a complete detector. Dates such as `2024-11-08`
  and year ranges such as `2024 - 2025` count too;
- a currency amount (any digit counts, fullwidth included, but not a
  superscript, fraction or Roman numeral, so `m²` is no amount): `€`, `$`,
  `£`, or the whole word `EUR`, `euro`, `euros`,
  `USD`, `dollar`, `dollars` or `GBP`, with a number in the question; or a
  number with a `k` suffix (`80k`);
- a key-like token: a word starting with `sk-`, `ghp_` or `AKIA`, `-----BEGIN`,
  or 24 or more consecutive ASCII letters and digits mixing both;
- "my", "our", "mijn", "m'n", "ons" or "onze" followed within three words by
  a word starting with portfolio, positie, position, holding, account,
  rekening, IBKR, broker, salaris, salary, wachtwoord, password, aandeel,
  aandelen, belegging, hypotheek, pensioen, saldo, spaar, schuld, lening,
  share, stock, invest, mortgage, pension, balance, debt or loan;
- a symbol the owner holds per the last IBKR sync (the snapshot the runtime
  facts read), as a whole word in any case. When IBKR is linked but that
  snapshot cannot be read, or does not list every held symbol, the gate fails
  closed.

Names, addresses and other free-text personal details are not detectable; a
question that only carries those still searches automatically. Choose `on`
when that is not acceptable.

**Identifier check.** Every Research question, explicit (with `on` and with
`auto`) or automatic, must also pass a narrower check, and so must every search
query the subscription workers see (see `SUBSCRIPTION_WORKERS.md`, "Research
runs"). It flags only personal identifiers: an email address, a key-like token,
the possessive rule above, and the long-number rule above, except that a run
that is exactly one date (`YYYY-MM-DD`, `YYYY/MM/DD`, `DD-MM-YYYY`,
`DD/MM/YYYY` or `DD.MM.YYYY`, each part joined by exactly one of those ASCII
separators, with a 1-2 digit day from 1 to 31, a month from 1 to 12 and a year
from 1900 to 2099) is not a number, and neither is a run of exactly two years
from 1900 to 2099 joined by a dash or `/` (`2024-2025`, `1990 – 2000`; not
`1999 2045` or `2012.2034`). A date with another digit group joined to it by a
separator still counts. Amounts, the `k` suffix and held symbols are not
checked here: an explicit search on a price or a held symbol is the owner's own
choice, and an automatic question has already passed the gate above. The search
queries the model writes in an automatic run get only this narrower check, so
an amount the model adds to a query does not stop the run. A flagged explicit
question is not searched at all: Core answers with a fixed notice asking the
owner to rephrase it without personal details, and there is no override.

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

**Photos.** A request whose input carries chat photos goes only to the
owner's subscription workers, `claude-cli` and `codex-cli`, in the owner's
chain order (`codex-cli` still only through a routed chain or a pin). The
router skips every other backend for it: paid APIs, Ollama and anything
else never see an image. A run (streamed, one attempt) uses the first of
them that is usable; a failed attempt ends the run and is never retried
elsewhere. A brain pinned to another backend, or no usable subscription
worker, ends the request with "no vision-capable subscription worker"; Core
then stores a fixed answer and keeps the photo. A failed photo attempt puts
the worker in cooldown only for a revoked login or a full plan, so text chat
is not moved to another backend because of a photo. The same chat without
photos routes as before.

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
