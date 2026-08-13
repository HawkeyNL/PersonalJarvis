# ADR-028 — Model-per-taak routing + plan→execute (goedkoopste geschikte intelligentie)

- Status: geaccepteerd (fase 1 gebouwd) — 13 augustus 2026
- Bouwt op ADR-027 (kosten-bewuste brein-router + registry + budget) en op
  `core/Jarvis.md` §6 (Orchestrator First), §7 (Agent Specialization),
  §9 (Cheapest Sufficient Intelligence)

## Context

ADR-027 kiest per taak de goedkoopste geschikte **provider** (plan → API →
lokaal), maar binnen een provider staan de modellen nog vast op drie tiers
(default/hard/cheap uit config). De eigenaar wil verder:

- **Lage modellen vrijwel altijd.** De meeste taken zijn simpel; die horen naar
  een klein/goedkoop model, niet standaard naar Sonnet/Opus.
- **Top-modellen alleen om te denken.** Een duur model (Opus, o-serie,
  deepseek-reasoner) is er om een **plan/architectuur uit te denken** — de
  **uitvoering** (code schrijven, deelstappen) gaat naar goedkopere modellen.
- **Jarvis kent én ontwikkelt zijn eigen ecosysteem.** Hij weet welke modellen er
  zijn, wat ze kosten en waar ze goed in zijn, en kan dat laten groeien.

Dit is precies "Cheapest Sufficient Intelligence" + "Orchestrator First".

## Beslissing

Van "tier → vast model" naar **"taak → gekozen model uit een catalogus"**, met
een expliciete **plan→execute-splitsing** voor zware taken. De keuze verhuist van
de losse provider-adapters naar de router/orchestrator-laag, die de catalogus +
kosten (ADR-027) raadpleegt.

### Fase 1 — Model-catalogus ("ken je ecosysteem") ✅ gebouwd

De registry (`jarvis-registry`) kent nu een **catalogus** van modellen: per
beschikbaar backend een lijst `ModelEntry { id, backend, class, cost, available }`.

- **Cloud-modellen** (Anthropic/OpenAI/DeepSeek): een curated lijst met per model
  een **capability-klasse** (`light` / `mid` / `heavy` / `reasoning`) en een
  **kosten-hint** (`local` / `cheap` / `mid` / `pricey`). Beschikbaar iff de
  provider een key heeft.
- **Lokale modellen** (Ollama): dynamisch uit `ollama list` — wat er echt staat.

Zichtbaar via `GET /v1/system/registry` (`models`) en in Status. Dit is de
kennisbasis; routing gebruikt 'm in fase 2.

### Fase 2 — Capability-routing (nog te bouwen)

De router kiest per **taak-klasse** het **goedkoopste model dat volstaat** uit de
catalogus (i.p.v. het vaste tier-model), begrensd door beschikbaarheid + budget
(ADR-027). Default-beleid: *licht tenzij nodig* — gewone chat → `light`; alleen
expliciet zware taken → `heavy`/`reasoning`. Taak-klasse komt eerst uit een
expliciete hint (client/agent), later uit een goedkope classifier.

### Fase 3 — Plan→execute-orchestrator (nog te bouwen)

Een orchestratiestap (§6) voor zware taken: een **plan-model** (`heavy`/
`reasoning`, bv. Opus) maakt een plan/architectuur; de **uitvoering** van de
deelstappen gaat naar goedkope `light`/`mid`-modellen, met een verificatie-stap
(§10) die het resultaat tegen het plan + `core/Jarvis.md` toetst. Dit is de brug
naar de Engineering Orchestrator (JAR-060) en de agentische laag (ADR-027 stage 4).

### Fase 4 — Zelfontwikkeling (later)

Jarvis breidt de catalogus zelf uit (nieuw Ollama-model gepulld, nieuwe provider-
key gezet → automatisch in beeld) en stelt voor waar een ander model voordeliger/
beter zou zijn — nooit autonoom betaalde acties zonder goedkeuring (§11, §12).

## Gevolgen

- Fase 1 raakt geen routing: puur kennis erbij, dus veilig en groen. De
  provider-adapters blijven werken op hun tiers tot fase 2 de keuze overneemt.
- Prijzen blijven in `jarvis-usage` de enige facturatie-waarheid; de catalogus
  toont alleen een **klasse + kosten-hint**, geen tweede prijs-tabel.
- De catalogus is per-backend afhankelijk van keys/lokale modellen, dus hij
  weerspiegelt automatisch wat de eigenaar echt heeft.
