# ADR-027 — Kosten-bewuste brein-router (plan vóór API vóór lokaal)

- Status: geaccepteerd (stages 1 + 2 + 3 + router-koppeling + multi-provider gebouwd) — 13 augustus 2026
- Bouwt op ADR-022 (Claude als brein, provider-abstractie) en de bestaande
  `LlmProvider`-trait + `FallbackProvider`

## Context

Jarvis draait op de eigen machine van de gebruiker, die een Claude-abonnement
heeft. Elk gesprek via de **API** kost per-token-geld, terwijl het **abonnement**
al betaald is. Wens: Jarvis gebruikt het **goedkoopste geschikte brein** per taak
en verdeelt het werk logisch — het plan zolang het kan, de API als vangnet, en
lokale modellen (Ollama, gratis) waar dat volstaat. Jarvis moet daarvoor
"weten" welke agents/modellen er zijn, hun status/kosten, en op welke
hardware/software hij draait.

## Beslissing

**Een router achter de bestaande `LlmProvider`-trait, kosten-eerst.** Providers
zijn `Arc<dyn LlmProvider>`; de `FallbackProvider` (probeer primair, val bij fout
terug — behalve bij een echte refusal) is de bouwsteen.

**Stage 1 (gebouwd): `claude-cli`-provider + reactief API-vangnet.**
- Nieuwe `ClaudeCliProvider` roept de lokale **`claude` CLI** headless aan
  (`claude -p … --output-format json --model … --append-system-prompt …`), in een
  neutrale werkmap en zonder tools, zodat het een puur antwoord teruggeeft dat op
  het **abonnement** loopt i.p.v. de API. `JARVIS_LLM_PROVIDER=claude-cli`.
- **Elke** CLI-fout (non-zero exit, onparseerbare output, of een `is_error`-result
  — zo verschijnt een plan/rate-limit) wordt een `LlmError`, waardoor de
  `FallbackProvider` doorschakelt naar de API (of Ollama). Dat is precies "API als
  vangnet als de CLI vol is", **reactief**.

**Waarom reactief, niet "tot 98%":** het plan-verbruik is niet schoon
programmatisch uit te lezen (rollende vensters, geen simpele "resterend %"-API).
Reactief terugvallen op de limiet-fout is robuust en vereist geen schatting.
Een **proactieve** drempel (zelf usage tellen, bij ~X% vast naar de API) is een
latere stage bovenop deze basis.

## Vervolg (nog te bouwen)

- **Stage 2 — usage/budget-routing ✅ gebouwd**: crate `jarvis-usage` schat de
  kosten per call (prijs-per-model × tokens, USD→EUR) en logt élke *metered* call
  in Postgres (`llm_usage`, migratie 0007). Alleen de betaalde API's tellen; het
  plan (claude-cli) en Ollama zijn gratis. Een **harde maand-cap** (default
  `JARVIS_LLM_MONTHLY_BUDGET_EUR=50`) wordt afgedwongen door `BrainAvailability`:
  zodra de maand-som ≥ budget, markeert de availability-brug de metered breinen
  als "niet beschikbaar" → de router valt terug op plan/Ollama. De teller is een
  `AtomicU64` (cent) die de DB spiegelt: geseed bij startup, herladen na elke call
  (dekt de maandwissel, want de som filtert op `date_trunc('month', now())`).
  Endpoint `GET /v1/system/usage` (budget/verbruik/rest + per-backend); client
  toont een budgetbalk in Status. **Multi-provider**: naast Anthropic nu ook
  OpenAI + DeepSeek (OpenAI-compatibele adapter), elk met eigen key in de backend.
  Nog open: een proactieve waarschuwing rond ~80–90% en per-provider sub-budgetten.
- **Stage 3 — resource-/agent-registry ("instant memory") ✅ gebouwd**: crate
  `jarvis-registry` detecteert de **host** (CPU/RAM/GPU via `sysinfo`, arch, OS) en
  probeert de **breinen/tools** (claude CLI, Ollama + lokale modellen, cmake,
  whisper-model), met per brein een **kostentier** (plan/metered/local) +
  beschikbaarheid. Verzameld bij startup, endpoints `GET /v1/system/registry` +
  `POST /v1/system/registry/refresh`; client toont het in Status ("AI-RESOURCES").
- **Router ↔ registry-koppeling ✅ gebouwd**: `JARVIS_LLM_PROVIDER=router`
  (of `auto`) bouwt een `RouterProvider` (`crates/llm/src/router.rs`) die **per
  verzoek** kiest op **capability × kosten × beschikbaarheid** i.p.v. een vaste
  keten. Per tier een **logische voorkeursvolgorde**:
  - `Cheap` → `ollama` → `claude-cli` → `anthropic-api` (goedkoopste eerst;
    simpel werk mag lokaal/gratis).
  - `Default` → `claude-cli` → `anthropic-api` → `ollama` (plan eerst voor
    kwaliteit, lokaal alleen als laatste redmiddel).
  - `Hard` → alleen sterke breinen (`claude-cli` → `anthropic-api`).

  De router filtert die volgorde op **live beschikbaarheid** uit de registry via
  de trait `jarvis_llm::Availability`, in de api geïmplementeerd door
  `RegistryAvailability` (leest `registry.brains[].available`, achter een
  `std::sync::RwLock` zodat de check synchroon is). **Vangnet**: zegt de registry
  dat *niets* beschikbaar is, dan probeert de router de volledige geordende lijst
  alsnog — een verkeerde registry mag het brein nooit lamleggen. Binnen de
  gekozen lijst blijft het **reactief**: probeer op volgorde, val bij elke fout
  (behalve een echte refusal) door naar de volgende. `jarvis-llm` hangt níet af
  van `jarvis-registry` (geen cyclus) — alleen de api overbrugt de twee.
- **Stage 4 — MCP-laag**: MCP is de **tool/context-laag**, geen facturatieroute.
  Jarvis als MCP-host (tools consumeren) én Jarvis-eigen tools (portfolio, IBKR,
  geheugen) als MCP-server die Claude Code kan gebruiken. Agentische shell/taken
  ("open terminal, draai dit") achter een allowlist + goedkeuring — biometrie
  blijft het slot.

## Gevolgen

- Directe besparing mogelijk: zet `JARVIS_LLM_PROVIDER=router` (aanbevolen) en
  Jarvis verdeelt het werk zelf — gratis lokaal waar het kan, je abonnement voor
  echt werk, de API als vangnet. `claude-cli` blijft beschikbaar als vaste keten
  (alleen plan + vangnet, zonder de per-taak-routing).
- Vereist een ingelogde `claude` CLI op de machine waar de backend draait.
  Headless `-p` is een officiële feature; voor persoonlijk gebruik prima (geen
  publieke multi-user API-backend). Subprocess = iets meer latency dan HTTP.
- De router blijft dezelfde trait, dus stages 2–4 pluggen erin zonder de
  call-sites te raken. Sleutels blijven backend-only (ADR-022).
