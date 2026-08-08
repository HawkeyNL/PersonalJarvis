# MCP versus API — beslisdocument

## Kernregel

**API = productcontract. MCP = agentgereedschap.**

Een API wordt door jouw clients en services deterministisch aangeroepen. MCP maakt tools discoverable voor AI-clients. Bouw daarom nooit je volledige interne architectuur uitsluitend als MCP.

## Wanneer een gewone API gebruiken

Gebruik REST/WebSocket/interne Rust interfaces voor:

- authenticatie;
- portfolioqueries;
- gebruikersinstellingen;
- orderworkflow en approvals;
- idempotency;
- auditlogging;
- schedulerbeheer;
- synchronisatie;
- grote bulkdatatransfers;
- stabiele businesslogica;
- mobiele/desktopclients;
- brokeradapters;
- iedere geldbewegende mutatie.

## Wanneer MCP gebruiken

Gebruik MCP voor kleine, semantisch duidelijke tools die een agent selecteert:

- `get_market_snapshot`
- `get_open_positions`
- `get_trade_history`
- `search_research_documents`
- `run_saved_backtest`
- `read_backtest_result`
- `draft_order_proposal`
- `find_content_trends`
- `create_short_script_draft`

## Welke MCP's bouwen

### 1. MT5 native MCP — gebruiken

MT5 Build 6060 heeft native MCP en kan marktdata, tradingomgeving, analyse, ontwikkeling en tests beschikbaar maken. Gebruik dit als adapter naar MT5, maar plaats er een eigen gateway/policylaag voor.

### 2. Jarvis Research MCP — bouwen

Read-only tools voor:

- opgeslagen filings;
- earnings transcripts;
- nieuws;
- eigen notities;
- backtestrapporten;
- strategieversies.

### 3. Jarvis Market MCP — optioneel

Handig wanneer externe AI-clients dezelfde read-only markttools moeten gebruiken. Voor de eigen backend volstaat aanvankelijk een interne typed tool registry.

### 4. Jarvis Trading MCP — zeer beperkt

Bouw niet één brede `execute_any_trade`-tool. Expose hoogstens:

- `create_order_proposal`
- `validate_order_proposal`
- `request_order_approval`
- `get_order_status`
- `cancel_pending_order` met policy

De daadwerkelijke brokeradapter blijft interne code.

### 5. Jarvis Content MCP — later

Tools voor trendonderzoek, scripts, assets en renders. Publicatie blijft een aparte approved API-command.

## Toolontwerp

Slecht:

```json
{"tool":"trade","instruction":"Doe wat slim is met EURUSD"}
```

Goed:

```json
{
  "tool": "create_order_proposal",
  "symbol": "EURUSD",
  "side": "buy",
  "order_type": "limit",
  "entry": 1.08420,
  "stop_loss": 1.08180,
  "take_profit": 1.08900,
  "risk_budget_eur": 5.00,
  "thesis_id": "th_...",
  "market_snapshot_id": "ms_..."
}
```

## MCP-beveiliging

- deny-by-default tool allowlist;
- aparte read- en write-servers;
- exact scopes per tool;
- geen token passthrough;
- geen breed shell/filesystem-access;
- pinned server identity waar mogelijk;
- input/output schema validation;
- toolresultaten als onbetrouwbare data behandelen;
- prompt injection detecteren en isoleren;
- netwerk-egress allowlist;
- auditlog vóór en na toolcall;
- mutaties vereisen short-lived approval token;
- kill switch buiten het agentproces.

## Conclusie

- **MT5:** native MCP gebruiken.
- **IBKR:** officiële API-adapter bouwen.
- **Eigen app:** normale API gebruiken.
- **AI tooling:** interne tools eerst; MCP toevoegen waar interoperabiliteit echt waarde heeft.
- **Trading:** MCP mag voorstellen en data lezen; execution blijft achter eigen gateway.
