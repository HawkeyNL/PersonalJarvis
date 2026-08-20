# ADR-031 — Jarvis als read-only MCP-server

- Status: **geaccepteerd — gebouwd (read-only spoor)** — 13 augustus 2026
- Bouwt op ADR-029 (agentische laag, m.n. 4c Claude Code-executor), ADR-013
  (IBKR read-only), ADR-022 (secrets alleen backend), en de device-login
  (elke tool-call is eigenaar-gebonden)

## Context

De agentische laag (ADR-029 4c) laat Jarvis **Claude Code** aansturen als
uitvoerder. Om nuttig te zijn moet die uitvoerder Jarvis' eigen kennis kunnen
**raadplegen** — portfolio, status, geheugen — zonder de backend-secrets of
mutatie-rechten. MCP (Model Context Protocol) is dé standaard om zulke tools/data
aan een LLM-tool als Claude Code te koppelen.

Dit ADR dekt het **eerste, veilige spoor**: Jarvis als **read-only MCP-server**.
"Jarvis als MCP-host" (externe MCP-servers consumeren) is een later spoor.

## Beslissing

Jarvis biedt een kleine set **uitsluitend-lezende** MCP-tools aan, eigenaar-
gebonden, zonder secrets/mutaties/trading.

### Tools (read-only)

| Tool | Wat | Bron |
|---|---|---|
| `portfolio_summary` | Posities + kostenbasis + allocatie | `portfolio::list_holdings` |
| `jarvis_status` | Ecosysteem: host, breinen, model-catalogus, budget | registry + budget |
| `recent_conversations` | Recente gesprekstitels (het "geheugen") | `conversations` |

Elke `tools/call` levert `{content: [{type: "text", text: ...}]}`.

### Harde grenzen (waarom dit veilig is)

- **Alleen lezen.** Geen tool schrijft, muteert, of plaatst orders. Holdings
  wijzigen, agent-acties, live-trading: **niet** via MCP (blijven achter de
  bestaande auth/goedkeuring). IBKR blijft read-only (ADR-013).
- **Geen secrets, geen Core.** De tools raken nooit `.env`/keys/keychain of
  `jarvis-core/**`; ze lezen alleen afgeleide, niet-gevoelige data.
- **Eigenaar-gebonden.** Elke call draait onder de ingelogde eigenaar; geen
  multi-user, geen publieke route (single-user, ADR-029 §niet-doelen).
- **Read-only sluit aan op 4c.** Dit is precies de kant die de confined Claude
  Code-executor veilig mag raadplegen; schrijven blijft de getekende gate.

### Transport — Streamable HTTP in de api

Eén endpoint **`POST /mcp`** in de bestaande Axum-service (geen apart binary):

- **Hergebruikt de bestaande auth**: de `Authed`-extractor eist een geldig
  session-token, dus elke MCP-call is de ingelogde eigenaar. Claude Code stuurt de
  `Authorization: Bearer …`-header op elke request (uit `.mcp.json`).
- **Plain JSON-antwoorden** (`application/json`) — geen SSE nodig voor
  request/response; simpelste spec-conforme vorm.
- **Defensief protocol** (MCP-vormen zijn over versies verschoven, dus geen aanname
  op één versie): het endpoint beantwoordt **zowel** de klassieke `initialize` als
  de nieuwere `server/discover`, **echoot de door de client gevraagde
  `protocolVersion`** terug, en geeft **union-resultaten** die beide vormen dekken.
  De stabiele kern — `tools/call` → `{content:[{type:"text"}], isError}` — is overal
  gelijk. Notificaties (geen `id`) → `202` zonder body; onbekende methode/tool →
  JSON-RPC-fout `-32601`.
- **DNS-rebinding-guard**: een browser-`Origin` die niet lokaal is → `403`. Niet-
  browser-clients (Claude Code) sturen geen `Origin` en mogen door.

Koppelen (lokaal, single-user) via `.mcp.json` of `claude mcp add`:

```json
{
  "mcpServers": {
    "jarvis": {
      "type": "http",
      "url": "http://localhost:8080/mcp",
      "headers": { "Authorization": "Bearer <JOUW_SESSION_TOKEN>" }
    }
  }
}
```

Het token is een gewoon Jarvis session-token (device-login). Read-only, dus een
verlopen/ingetrokken token faalt netjes met `401`.

## Niet-doelen (dit spoor)

- Geen schrijvende/mutatie-tools via MCP.
- Geen "Jarvis als MCP-host" (externe servers consumeren) — later spoor.
- Geen publieke/multi-user MCP-endpoint.

## Gevolg

Jarvis' portfolio, status en geheugen zijn nu **veilig, read-only** te raadplegen
door Claude Code (de 4c-executor) en de eigenaar' eigen Claude-tooling, via
`POST /mcp` achter de bestaande auth. Schrijven/muteren blijft buiten MCP, achter
de getekende goedkeuring (ADR-029 4b/4c). api 9 tests (+1 MCP: auth, initialize,
tools/list, tools/call, onbekende tool, origin-guard), clippy schoon. Later spoor:
Jarvis als MCP-**host** (externe read-only MCP-servers consumeren).
