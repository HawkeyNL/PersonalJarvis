# Goal: Intelligent Model Router v1 + Secure Credential Manager

Implement an intelligent, cost-aware multi-provider model router for PersonalJarvis and a root-operated credential manager for the Ubuntu Home Node.

Work against the current architecture. Preserve all existing Home Node, private AgentRegistry, immutable release, SurrealDB, approval, sandbox and security boundaries.

## Core principle

Do not choose the cheapest model first. Determine the minimum quality/capability tier required for a task, then choose the best available model inside that tier using capability, health, latency and cost. When uncertain, escalate upward rather than silently returning a low-quality answer.

Agents and models remain separate concepts. An agent requests a model policy/capabilities; it must not hard-code one provider/model as its privilege or identity.

## Providers

Design provider adapters/registry support for at least:

- OpenAI
- Anthropic
- xAI / Grok
- DeepSeek
- Z.ai / GLM
- Ollama local
- Ollama Cloud/API where supported

Do not assume all credentials are configured. Missing providers must be reported as unavailable, not crash Core.

Provider/model metadata must be data-driven where practical and include capabilities such as reasoning, tool use, structured output, context size, research/search capability where applicable, latency class and pricing metadata. Do not scatter model names throughout Core.

## Routing modes

Support an explicit request mode:

- `auto` (default): Jarvis decides depth/provider/model
- `fast`: prioritize latency/cost while maintaining a minimum acceptable quality
- `deep`: force high-quality/frontier reasoning and allow escalation/review
- `research`: allow research-oriented orchestration and source gathering before synthesis

The external API should expose the selected mode without exposing provider credentials.

## Task analysis

Before model execution, classify enough of the task to determine at least:

- complexity/depth
- current-information/research requirement
- tool requirement
- agent/specialist requirement
- safety/risk/approval relevance
- context requirements
- expected output type
- minimum model capability/quality tier

The router/classifier must not replace the user's original request with a lossy summary. The selected answering model/agent receives the original request plus the relevant trusted context.

Keep classification cheap, bounded and observable. Avoid an expensive extra LLM call when deterministic routing is sufficient.

## Quality tiers and escalation

Introduce semantic tiers rather than provider-specific `cheap`, `normal`, `hard` fields. A reasonable conceptual structure is:

- fast/utility
- standard
- strong
- frontier
- research-specialist

Exact naming may differ if the codebase has a better abstraction.

Support escalation when:

- provider/model fails
- timeout occurs
- response is structurally invalid
- model reports insufficient confidence/capability through a trusted mechanism
- required tool/capability is unavailable
- task policy requires a stronger tier

Do not create unbounded retry loops. Bound attempts, total time and estimated cost.

## Provider health and fallback

Track provider/model health separately from task quality.

Handle:

- timeout
- HTTP 429/rate limiting
- provider 5xx
- auth failure
- malformed response
- unavailable model
- context overflow

Fallback only to a model that still satisfies the minimum task capability/quality requirements.

An auth failure should mark that credential/provider unhealthy and produce useful diagnostics without logging the secret.

## Cost accounting

Persist useful model execution telemetry in SurrealDB, including where available:

- request/run ID
- provider
- model
- routing mode
- quality tier
- agent ID if applicable
- input tokens
- cached input tokens
- output/reasoning tokens where exposed
- latency
- estimated/actual cost
- status/failure category
- fallback/escalation chain

Never store prompts/responses merely for billing telemetry unless an existing explicit privacy/memory policy permits it.

Keep the existing monthly budget concept, but make it multi-provider and routing-aware. Budget pressure may prefer cheaper qualifying models; it must not silently downgrade below the minimum required quality/security tier.

## Pricing metadata

Pricing changes frequently. Do not bake provider prices irreversibly into routing logic.

Use a versioned/configurable pricing registry with timestamps/source notes and safe defaults. Unknown pricing must be represented as unknown, not zero.

Routing should still function when cost metadata is stale/unavailable, using quality/capability/health first.

## Secure Home Node credential manager

The owner should NOT need direct interactive access to `/etc/jarvis`.

Install a root-operated command available as:

```bash
sudo jarvis-credentials set <provider>
sudo jarvis-credentials list
sudo jarvis-credentials test <provider>
sudo jarvis-credentials remove <provider>
```

Support at least provider IDs:

- openai
- anthropic
- xai
- deepseek
- zai
- ollama

If local Ollama requires no credential, `ollama` should clearly distinguish local Ollama from Ollama Cloud/API credentials rather than forcing a fake key.

### Secret input

`set` must prompt interactively with hidden input from a TTY. Do not accept the secret as a normal command-line argument by default.

The secret must not appear in:

- shell history
- argv/process listings
- stdout/stderr
- journal logs
- audit messages
- Git
- Jarvis.md
- private agent definitions
- SurrealDB telemetry

Do not echo even partial secret values.

Fail safely when no controlling TTY is available unless an explicitly designed secure provisioning mechanism is used.

### Storage

Use a dedicated root-managed secrets location, conceptually:

`/etc/jarvis/secrets/`

The directory remains inaccessible to ordinary users. Individual provider credential files should be root-owned and readable only by the minimum service identity that requires them, e.g. `root:jarvis 0640` where appropriate.

Do not weaken `/etc/jarvis` globally beyond the existing production permission model.

Core may read provider credentials required for model calls. OpenSandbox workloads, Codex jobs and arbitrary agents must NOT automatically inherit all provider credentials.

Prefer separate provider files or another structure that allows least-privilege loading and rotation. Never place all secrets into a world-readable/general environment dump.

### Systemd integration

Integrate credentials into `jarvis-core.service` without placing plaintext secrets directly in the unit file or repository.

Evaluate systemd credential facilities (`LoadCredential=` / encrypted credentials where suitable) versus root-managed `EnvironmentFile=` provider files. Choose the design that fits the current Ubuntu Home Node and explain the threat model/tradeoff in documentation.

Do not weaken:

- `User=jarvis`
- `NoNewPrivileges=true`
- `ProtectSystem=strict`
- existing protected persona/agent paths
- root-only SurrealDB/update credentials

### Credential operations

`list` displays configuration state only, for example:

```text
PROVIDER     CONFIGURED   STATUS
OpenAI       yes          healthy
Anthropic    no           not configured
xAI          yes          unknown
DeepSeek     yes          healthy
Z.ai         no           not configured
Ollama local yes          healthy
```

Never display credential contents.

`test <provider>` performs the smallest safe provider-specific validation possible. Avoid costly generation where a lightweight authenticated endpoint is available. Clearly report when validation necessarily incurs a model call.

`remove` must require explicit confirmation when interactive, remove only that provider's credential, reload/restart Core safely if needed, and verify the provider becomes unavailable.

Credential updates should be atomic: write to a root-only temporary file, fsync/validate as appropriate, set ownership/mode, atomic rename, then reload/restart Core. A failed update must not destroy the previous working credential.

## Core reload/restart behavior

Credential changes must not leave Core down unexpectedly.

Implement a bounded health-checked restart/reload flow:

1. validate credential file structure/permissions
2. restart/reload Core if required
3. wait bounded time for `/livez` and `/readyz`
4. on failure, show concise diagnostics
5. where practical, roll back the credential change and restore the previous healthy state

Never print secrets during diagnostics.

## Provider adapters

Normalize provider responses behind a common interface. Avoid leaking provider-specific JSON throughout orchestration.

Conceptually expose:

- request/messages
- model selection
- tool/structured-output capabilities
- token usage
- response
- provider error category
- latency
- cost metadata

Use official provider APIs/protocols. Do not implement custom cryptography or unofficial authentication workarounds.

## Ollama

Keep local Ollama as a first-class offline/local provider.

Local Ollama should work without an API key when bound locally as designed.

Ollama Cloud/API, if used, must be modeled as a credentialed remote endpoint separately enough that Core cannot confuse a remote authenticated service with trusted localhost Ollama.

Expose provider health so Jarvis knows when the local Ollama service is absent.

## Agent integration

Private agent definitions already contain `model_policy`. Connect this to the new router.

Examples conceptually:

- research agent -> research-capable policy
- coding agent -> strong coding/agentic policy
- trading/risk agent -> high reasoning/reliability policy
- utility tasks -> fast/utility policy

Agent definitions may request a policy but cannot choose arbitrary credentials, bypass budgets, lower security requirements or force an unavailable/inappropriate provider.

## Research mode

Design `research` so a research specialist can gather current information through approved tools/providers and a strong synthesis model can produce the final response when useful.

Do not equate one provider's built-in web search with the entire research architecture. Keep search/retrieval and synthesis composable.

## API/UI observability

Expose non-secret routing metadata to authenticated clients so the Jarvis app can eventually show, for example:

- mode: Auto/Deep/Research
- selected provider/model
- agent(s) used
- whether escalation/fallback occurred
- latency
- estimated cost

Do not expose internal secrets or sensitive reasoning traces.

Provide a concise explanation field such as `routing_reason` based on routing policy facts, not hidden chain-of-thought.

Example: `Deep reasoning requested; selected frontier model with tool support.`

## Security boundaries

Treat all model output as untrusted data until interpreted through existing policy/tool boundaries.

A model or agent must never gain more tool capability merely because it asks for it.

Do not pass provider credentials into:

- OpenSandbox by default
- arbitrary shell commands
- Codex worktrees
- downloaded repositories
- browser content
- prompts

Prompt injection must not be able to request credential contents.

## Logging

Continue redacting secrets in startup/config logs. Improve the current startup config logging if necessary so adding six providers cannot accidentally dump API keys.

Add regression tests that seed recognizable fake credentials and assert they do not occur in logs, errors, telemetry or command argv.

## Home Node deployment

Extend the existing idempotent deployment scripts to create the credential-manager binary/script and protected secrets directories with correct permissions.

A fresh Home Node should start without any cloud credentials and remain healthy using local/offline capabilities where configured.

An upgrade from v0.0.8 must preserve existing Core/SurrealDB/persona/agent state and must not rotate unrelated secrets.

`verify-home-node.sh` should verify credential storage permissions without reading/displaying credential values.

## Tests

Add comprehensive tests for:

- routing modes
- deterministic tier selection
- capability constraints
- escalation/fallback bounds
- provider health
- rate-limit handling
- auth failure handling
- budget-aware selection
- unknown/stale pricing
- agent `model_policy` integration
- missing credentials
- local Ollama without credentials
- remote Ollama credential handling
- credential set/list/test/remove
- hidden/TTY-only secret entry behavior
- atomic credential replacement
- permissions/ownership
- no secret leakage in argv/logs/telemetry
- Core restart/health handling
- fresh Home Node with zero cloud credentials

Use mocks/fakes for provider unit tests. Real-provider integration tests must be opt-in and must never run against real paid credentials in normal CI.

## Migration

Migrate the current static fields such as provider/model/cheap/hard carefully. Preserve compatibility where useful, but the final routing path should use the new registry/policies rather than hard-coded `cheap`/`hard` decisions.

Document deprecated environment variables and provide a safe migration path. Do not silently reinterpret existing credentials.

## Documentation

Document:

- router architecture
- model/provider registry
- routing modes
- quality tiers
- escalation/fallback
- cost/budget accounting
- credential manager commands
- credential storage/security model
- adding/updating providers/models/pricing
- local vs remote Ollama
- Home Node operational workflow

The owner should be able to configure a new provider without entering `/etc/jarvis` manually.

## Definition of done

Complete when:

- Jarvis supports Auto/Fast/Deep/Research routing modes
- minimum quality/capability is determined before cost optimization
- provider/model registry replaces hard-coded routing assumptions
- OpenAI, Anthropic, xAI, DeepSeek, Z.ai and Ollama adapters/configuration are represented cleanly
- local Ollama works without a credential
- credentialed remote Ollama is supported distinctly where applicable
- private agents' `model_policy` is wired into routing
- fallback/escalation is bounded and capability-safe
- cost/latency/token telemetry is persisted without leaking prompt/secrets by default
- monthly budget is multi-provider aware
- `sudo jarvis-credentials set/list/test/remove` works
- keys are entered hidden and never passed in normal CLI argv
- credentials are stored root-managed with least privilege
- ordinary users do not need direct `/etc/jarvis` access
- sandboxes/Codex do not inherit provider keys by default
- credential changes are atomic and health checked
- startup/config logs never reveal keys
- fresh Home Node works with no cloud keys
- upgrade from the current v0.0.8 Home Node preserves existing state
- deployment/security/regression tests pass
- documentation matches the implementation

Do not create a release merely because the specification file exists. Implement and test the complete milestone before marking the PR merge-ready.