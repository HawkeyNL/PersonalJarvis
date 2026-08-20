# MetaTrader 5 native MCP

## Feitelijke basis

MetaTrader 5 Build 6060 van 23 juli 2026 introduceerde native MCP- en agentic-AI-ondersteuning. MetaQuotes beschrijft toegang tot marktinformatie, charts, tradingomgeving en operaties via MCP. MT5 heeft bovendien beveiligingsinstellingen om AI-trading toe te staan, te verbieden of handmatige bevestiging te vereisen.

## Rol in Jarvis

MT5 native MCP is de MT5-adapter. Jarvis gebruikt hem via een eigen `mt5-policy-proxy` of gateway.

```text
Jarvis agent
→ internal tool gateway
→ risk/policy
→ MT5 MCP client/proxy
→ MT5 terminal
→ broker
```

## Eerst inventariseren

Na installatie moeten de daadwerkelijk aangeboden:

- tools;
- resources;
- prompts;
- schemas;
- mutatierechten;
- confirmation behavior;

worden geëxporteerd en vastgezet in een compatibility test. Vertrouw niet op aannames over toolnamen.

## Aanbevolen rechtenfasen

### Read-only

- symbols/market watch
- quotes/candles
- account
- positions
- orders/history
- charts/indicators
- tester reports

### Development

- MQL5-code genereren/analyseren;
- compileren;
- Strategy Tester uitvoeren;
- rapporten lezen.

Alleen in een sandbox/repository met review.

### Demo write

- orderproposal valideren;
- demo-order;
- modify/cancel;
- trade management.

### Live

- standaard uit;
- MT5 manual confirmation aan;
- eigen approval + risk engine verplicht;
- allowlist;
- max notional;
- kill switch.

## MQL5 EA nog nodig?

Niet als algemene bridge. Wel nuttig als:

- deterministic strategy executor;
- local risk sentinel;
- heartbeat/failsafe;
- trailing/time-stop logic;
- Strategy Tester target;
- bescherming wanneer Jarvis/backend offline is.

## Windows VPS

- MT5 en copier in aparte terminal instances/profiles;
- unieke magic numbers;
- unieke directories;
- niet dezelfde orders dubbel beheren;
- tunnel naar backend;
- auto-start gecontroleerd;
- watchdog;
- logs en terminal build monitoren;
- updates eerst op staging/demo testen.

## Telegram copier

Laat de bestaande copier voorlopig apart. Voeg alleen read-only observatie toe. Migreer pas wanneer:

- signal parsing in tests gelijkwaardig is;
- duplicate prevention werkt;
- position sizing klopt;
- reconnectcases getest zijn;
- paperperiode is doorlopen.

## Belangrijk

MCP “begrijpt” de markt niet. Het levert context en tools. Strategie, risk, data-integriteit en backtests blijven aparte componenten.
