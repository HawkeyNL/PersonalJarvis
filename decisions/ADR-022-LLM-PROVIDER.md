# ADR-022 — Primaire LLM-provider: Anthropic (Claude), met lokale fallback

- Status: geaccepteerd — 9 augustus 2026 (sluit DEC-001)
- Context: DEC-001 (OpenAI vs Anthropic); voice-stack = Fish Audio; ADR-001
  (API/MCP), ADR-021 (spraak-uitvoer-policy)

## Context

Het Jarvis-gesprek (SYSTEM → Jarvis-view) had tot nu toe een placeholder-antwoord
(`draftReply`). Om het brein echt te maken moesten we DEC-001 sluiten: welke
cloud-LLM wordt de standaard, en hoe houden we providers wisselbaar zonder de
rest van de app eraan vast te lijmen. Randvoorwaarden uit de blueprint: de
API-sleutel mag **alleen** in de backend leven (nooit in client/webview/logs), en
we willen kunnen degraderen naar lokaal draaien als de cloud niet bereikbaar is.

## Beslissing

**Anthropic (Claude)** wordt de primaire cloudprovider, met een **tier-model**:

- **Standaard-brein**: `claude-sonnet-5` — de meeste gesprekken.
- **Zwaar redeneren**: `claude-opus-5` — complexe/meerstaps-taken.
- **Snel & goedkoop**: `claude-haiku-4-5` — korte/goedkope taken.

**Ollama** (`llama3.2`) is de **lokale fallback**: staan zowel Anthropic als
Ollama geconfigureerd, dan valt de backend automatisch terug op Ollama bij een
transport-/API-fout — **maar niet** bij een echte veiligheids-`refusal` (die
wordt niet weggewassen door een tweede provider).

Providers zitten achter één trait (`LlmProvider::chat`) in de nieuwe crate
`crates/llm`, opgeslagen als `Arc<dyn LlmProvider>` in `AppState`. Concrete
providers: `AnthropicProvider` (raw HTTP naar `POST /v1/messages`, headers
`x-api-key` + `anthropic-version: 2023-06-01`), `OllamaProvider`
(`POST /api/chat`), `FallbackProvider` (primary→fallback), plus `Unconfigured`
en een `Echo`-stub voor deterministische tests.

Het gesprek loopt via een beveiligd (Bearer/device-bound) endpoint
`POST /v1/assistant/chat`. De persona/system-prompt (`JARVIS_SYSTEM`, Nederlands)
wordt **server-side** toegevoegd; de client stuurt alleen de ruwe beurten. De
sleutel komt uit `JARVIS_LLM_API_KEY` (config, geredigeerd in `Debug`).

Waarom Claude boven OpenAI: sterke instructievolging en veiligheids-defaults die
passen bij een assistent met toegang tot financiële acties, een duidelijk
tier-aanbod (Sonnet/Opus/Haiku) voor de kosten/kwaliteit-afweging, en een simpele
Messages-API die zonder officiële Rust-SDK via een dunne reqwest-client te
bedienen is. Een tweede cloudprovider komt er **alleen op gemeten noodzaak**.

## Alternatieven

- **OpenAI als primair**: prima model-aanbod, maar geen doorslaggevend voordeel
  hier; we vermijden liever twee cloud-afhankelijkheden tot er een gemeten reden
  is. De provider-abstractie houdt de deur open.
- **Alleen lokaal (Ollama)**: gratis en privé, maar (nog) niet sterk genoeg als
  standaard-brein; blijft wél als fallback en offline-optie bestaan.
- **Provider-enum in plaats van trait-object**: minder uitbreidbaar; het
  `Arc<dyn LlmProvider>`-ontwerp maakt swappen/fallback triviaal.
- **Streaming (SSE) nu al**: uitgesteld — eerst een correcte non-streaming
  chat-lus; streaming is een latere iteratie op hetzelfde endpoint.

## Gevolgen

- Het Jarvis-gesprek heeft een echt brein zodra `JARVIS_LLM_API_KEY` in de
  backend-`.env` staat; zonder sleutel degradeert het naar Ollama of een nette
  "brein niet bereikbaar"-melding.
- Providers zijn wisselbaar achter `LlmProvider`; een tweede cloudprovider is
  additief, niet herstructurerend.
- Nieuwe config: `JARVIS_LLM_*` (provider, api_key, base-url, drie modellen,
  max_tokens, ollama-url/model) — zie `.env.example`.
- Nog te doen (backlog): SSE-streaming voor het chat-endpoint; tier-routing
  slimmer maken (nu expliciet meegegeven); cost/usage-tracking (Fase 1).
