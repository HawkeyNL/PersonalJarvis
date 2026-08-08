# Agent Observatory

## Doel

Een realtime en replayable overzicht van:

- welke agent actief is;
- wie een taak heeft gedelegeerd;
- welke agents met elkaar communiceren;
- welke tools/MCP-servers worden gebruikt;
- token- en eurokosten;
- latency;
- fouten;
- approvals;
- risk decisions;
- eindresultaten.

## Visueel concept: 3D AI-zonnestelsel

```text
                         Research Agent
                              ○
                              │
         Coding Agent ○ ───── ● ───── ○ Trading Agent
                            Jarvis
                              │
                    ○ Memory Agent
```

### Centrale zon

**Jarvis Orchestrator** staat in het midden.

Visuele status:

- rustig pulserend: idle;
- helderder: actieve workflow;
- oranje ring: wacht op approval;
- rood: error of risk block;
- gedimd: offline/degraded.

### Planeten

Iedere agent is een planeet:

- Investment Analyst
- Trading Analyst
- Risk Manager
- Execution Agent
- Memory Agent
- News Agent
- Event Alpha Agent
- Coding Agent
- Content Agent
- Economics/CFO Agent
- Security Guardian

De baan rond Jarvis representeert een domein of bevoegdheidsniveau.

### Manen

Subagents of tijdelijke workers verschijnen als manen rond hun parent-agent.

### Satellieten en stations

Externe tools en systemen:

- PostgreSQL
- Redis
- pgvector
- IBKR
- MT5 MCP
- Polymarket
- market-data providers
- Ollama/OpenAI/Claude/DeepSeek
- NautilusTrader
- filesystem/repository
- contentplatforms

## Communicatie

Een bericht wordt zichtbaar als een bewegend deeltje over een lijn:

- Jarvis → agent: taakdelegatie;
- agent → tool: toolcall;
- tool → agent: resultaat;
- agent → agent: samenwerking;
- agent → Jarvis: eindrapport;
- Jarvis → gebruiker: antwoord/notificatie.

Lijnstijl:

- solide: request;
- gestippeld: async event;
- dubbele lijn: streaming;
- dikke lijn: grote payload;
- onderbroken rood: failure;
- geblokkeerde lijn: policy/risk denied.

## Waarom dit functioneel is

Het is geen decoratie. De Observatory helpt met:

- debugging;
- kostenanalyse;
- agent-evaluatie;
- securityonderzoek;
- ontdekken van agentloops;
- latencyproblemen;
- onnodige modelcalls;
- uitleg aan de gebruiker;
- audit en replay.
