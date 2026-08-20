# Event Alpha Engine

## Doel

Onderzoeken of nieuwe, betrouwbaar bevestigde informatie sneller kan worden verwerkt dan een doelmarkt haar prijs aanpast.

## Kernflow

```text
Nieuws / officiële feeds / blockchain-events
→ verificatie
→ eventclassificatie
→ koppeling aan relevante markt
→ marktprijs en orderboek vergelijken
→ netto expected value na alle kosten
→ shadow/paper trade
→ performance-evaluatie
```

## Bronnen

- officiële persberichten;
- economische publicaties;
- bedrijfsfilings;
- officiële sport- en verkiezingsfeeds;
- blockchain-indexers en RPC's;
- betrouwbare nieuwsfeeds;
- goedgekeurde sociale bronnen alleen als secundair signaal.

## Doelmarkten

- Polymarket en andere prediction markets;
- gecentraliseerde crypto-exchanges;
- decentralized exchanges;
- futures;
- later eventueel andere liquide markten.

Polymarket is dus meestal de markt waarop wordt gemeten of gehandeld. De primaire trigger kan uit nieuws, een officiële feed of een blockchain-event komen.

## Componenten

- Breaking News Agent
- Event Verification Agent
- Event Classifier
- Entity & Market Mapper
- Market Reaction Monitor
- Opportunity/EV Engine
- Shadow Fill Simulator
- Performance & Cost Analyzer

## Belangrijk ontwerpprincipe

Gebruik geen groot taalmodel in een latency-kritische execution-loop.

- Rust verwerkt feeds, timestamps, pricing, orderboeken en execution.
- Kleine classifiers kunnen events triëren.
- Een LLM helpt alleen bij semantische interpretatie van complexe tekst.
- Risk, sizing en expected-value-berekeningen zijn deterministische code.

## Polymarket-route

```text
Event bevestigd
→ relevant contract vinden
→ resolution rules controleren
→ huidige odds en orderboek ophalen
→ fair-probability hypothese
→ verschil met marktprijs
→ fees/spread/slippage
→ shadow/paper proposal
```

## DEX/on-chain-route

```text
On-chain event
→ finality bevestigen
→ relevante pools/exchanges ophalen
→ prijsverschil meten
→ gas/slippage/priority fees/MEV simuleren
→ shadow result
```

## Kostenmodel

```text
bruto verwachte winst
- fees
- spread
- slippage
- gas en priority fees
- MEV-verlies
- API/modelkosten
- infrastructuurkosten
= netto verwachte edge
```

## Verplichte metrics

- detectielatency;
- verificatielatency;
- mapping accuracy;
- false-positive rate;
- beschikbare edge;
- fill probability;
- netto expectancy;
- edge decay;
- maximum drawdown;
- kosten per opportunity;
- resultaten per eventtype en markt.

## Fasen

1. Historical study
2. Live shadow
3. Paper/sandbox
4. Kleine assisted-live pilot, uitsluitend na gates

## Live gates

- minimaal 100 geëvalueerde opportunities;
- positieve out-of-sample netto expectancy;
- juridische en platformtoegang bevestigd;
- realistische fills en gefaalde transacties meegerekend;
- Risk Engine en kill switch getest;
- iedere live order handmatig bevestigd.

## Niet doen

- virale winstclaims als bewijs gebruiken;
- onbevestigde geruchten automatisch traden;
- private keys aan een LLM geven;
- onbeperkte wallet signing;
- gas, MEV of failed transactions negeren;
- een winstdoel gebruiken om risico te verhogen.

## MVP Definition of Done

- één primaire nieuws/eventfeed;
- Polymarket read-only market data;
- event-to-market mapping;
- immutable timestamps;
- shadow proposals;
- fill simulator;
- volledige kostenberekening;
- rapport over minimaal 100 opportunities;
- geen live wallet- of orderrechten.
