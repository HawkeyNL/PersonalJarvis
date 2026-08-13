# ADR-027 — Kosten-bewuste brein-router (plan vóór API vóór lokaal)

- Status: geaccepteerd (stage 1 gebouwd) — 13 augustus 2026
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

- **Stage 2 — usage/threshold-routing**: eigen token/verzoek-teller per backend,
  waarschuwen + vervroegd omschakelen rond een drempel; kosten/limieten in beeld.
- **Stage 3 — resource-/agent-registry ("instant memory")**: Jarvis kent de
  beschikbare breinen/agents (CLI, API, Ollama-modellen, MCP-servers), hun
  status/kosten, én de host (CPU/RAM/GPU, Apple Silicon; geïnstalleerde software)
  — zodat hij weet wat lokaal kan draaien en waar het voordeligst is. Router
  kiest per taak op **capability × kosten × beschikbaarheid**.
- **Stage 4 — MCP-laag**: MCP is de **tool/context-laag**, geen facturatieroute.
  Jarvis als MCP-host (tools consumeren) én Jarvis-eigen tools (portfolio, IBKR,
  geheugen) als MCP-server die Claude Code kan gebruiken. Agentische shell/taken
  ("open terminal, draai dit") achter een allowlist + goedkeuring — biometrie
  blijft het slot.

## Gevolgen

- Directe besparing mogelijk: zet `JARVIS_LLM_PROVIDER=claude-cli` en het gesprek
  loopt op je abonnement, met de API als automatisch vangnet.
- Vereist een ingelogde `claude` CLI op de machine waar de backend draait.
  Headless `-p` is een officiële feature; voor persoonlijk gebruik prima (geen
  publieke multi-user API-backend). Subprocess = iets meer latency dan HTTP.
- De router blijft dezelfde trait, dus stages 2–4 pluggen erin zonder de
  call-sites te raken. Sleutels blijven backend-only (ADR-022).
