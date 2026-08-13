# ADR-030 — Persistente, auto-gecategoriseerde gesprekken

- Status: **geaccepteerd — gebouwd** — 13 augustus 2026
- Bouwt op ADR-022 (LLM-brein), ADR-027/028 (kosten-router + tiers), en de
  device-login (elk gesprek is van de ingelogde eigenaar)

## Context

De chat leefde alleen in het geheugen van de client (`messages` ref). Bij een
app-herstart was alles weg, en er was geen manier om over meerdere onderwerpen
tegelijk te praten. De eigenaar wil dat Jarvis **de gesprekken bewaart** (server-
side, want de server is de altijd-aanwezige kern; de app is puur frontend) en dat
Jarvis **zelf een nieuw tabje** begint als het onderwerp verandert.

## Beslissing

**Gesprekken staan server-side** in Postgres (migratie `0010`):

- `conversations` (id, user_id, title, created_at, updated_at)
- `chat_messages` (id, conversation_id → cascade, user_id, role, content, model, created_at)

**Endpoints** (allen beveiligd, eigenaar-gebonden):

- `GET /v1/conversations` — de tabjes, nieuwste-actief eerst.
- `GET /v1/conversations/{id}` — de berichten van één gesprek, op volgorde.
- `DELETE /v1/conversations/{id}` — gesprek + berichten weg (cascade).
- `POST /v1/assistant/chat` — nu met `conversation_id`; persisteert de beurt en
  geeft `conversation_id` + `conversation_title` + `new_topic` terug.

**Auto-categorisatie.** Bij elk bericht draait een **goedkoop model** (tier Cheap,
router-gekozen) een mini-classifier: hoort dit bij het lopende onderwerp, en wat is
een korte titel? → JSON `{same_topic, title}`.

- Geen huidig gesprek, of `same_topic=false` → **nieuw gesprek** met die titel; de
  client springt naar dat tabje.
- `same_topic=true` → **aangevuld** bij het huidige gesprek.
- Een nieuw onderwerp start met een **schone context** (alleen het nieuwe bericht),
  een voortzetting houdt zijn geschiedenis.
- **Robuust boven alles**: mislukt de classifier (of geeft het brein geen bruikbare
  JSON), dan wordt het huidige gesprek behouden (of één gestart met een afgeleide
  titel) — chat breekt nooit. Het bericht van de eigenaar wordt **vóór** de brein-
  aanroep opgeslagen, dus het overleeft zelfs een brein-storing.

**Client.** Een `conversations`-store (tablijst + huidige id, onthouden in
`localStorage`) en een herschreven `assistant.ts` die de huidige-gesprek-berichten
beheert, topic-splits volgt, en bij het opstarten de tablijst + laatste gesprek
herlaadt — zo staat de chat er meteen weer na een herstart. De HUD toont een
**tab-strook** boven de transcript (klikken = wisselen, `+` = nieuw, `×` = verwijderen).

## Kosten

Eén goedkope classifier-call per bericht (kleine tokens, tier Cheap) bovenop de
brein-call. Beide worden tegen het €50-budget geboekt (ADR-027); "lage modellen
vrijwel altijd" (ADR-028) houdt dit klein.

## Niet-doelen / grenzen

- Geen gedeelde/multi-user gesprekken — alles is eigenaar-gebonden (single-user).
- Geen server-side samenvatting/geheugen over gesprekken heen (kan later).
- De classifier bepaalt alleen de *indeling*; hij ziet de secrets/Core niet en
  voert niets uit.

## Gevolg

De chat is persistent en volgt je over herstarts (en later over devices). Jarvis
splitst onderwerpen zelf in tabjes; de eigenaar kan wisselen, nieuw beginnen en
verwijderen. api 7 tests (+1 end-to-end persist/append/list/fetch/delete), client
typecheck + build groen.
