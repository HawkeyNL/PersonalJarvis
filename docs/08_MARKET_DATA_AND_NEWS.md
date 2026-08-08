# Marktdata en nieuws

## Scheid databronnen van brokers

Brokerdata is goed voor account- en executionstate. Voor research kan een aparte dataprovider praktischer zijn.

## Datatypes

- realtime/delayed quotes
- historische candles/ticks
- fundamentals
- corporate actions
- earnings
- filings
- macro-economische kalender
- nieuws
- social/trend data

## Ingestionprincipes

- provider timestamp bewaren;
- ontvangsttijd bewaren;
- symbol mapping centraal;
- timezone normaliseren;
- adjusted en unadjusted prijzen scheiden;
- gaps en duplicates detecteren;
- bronlicentie vastleggen;
- geen commerciële herdistributie aannemen;
- rate limits centraal beheren.

## Nieuwsverwerking

1. Ophalen.
2. Dedupliceren.
3. Entiteiten/tickers koppelen.
4. Bronkwaliteit classificeren.
5. Publicatie- en eventtijd onderscheiden.
6. Relevantie voor holdings bepalen.
7. Feiten extraheren.
8. Samenvatten.
9. Impact als hypothese labelen, niet als feit.
10. Alerts alleen bij ingestelde drempel.

## Marktbegrip

Een MCP geeft toegang, geen begrip. Bouw marktcontext uit:

- actuele prijsstructuur;
- meerdere timeframes;
- volatiliteit;
- volume/liquiditeit;
- spreads;
- kalender/events;
- nieuws;
- portfolio exposure;
- strategiecondities;
- historische regimes.

Sla een immutable `market_snapshot` op dat bij ieder voorstel hoort.
