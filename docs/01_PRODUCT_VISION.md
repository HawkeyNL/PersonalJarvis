# Productvisie

## Missie

Jarvis helpt één gebruiker om betere beslissingen te nemen, werk te automatiseren en informatie te ordenen, zonder cruciale controle af te staan aan een taalmodel.

## Productprincipes

1. **Local-first experience, server-backed truth**  
   De client gebruikt lokale cache, maar PostgreSQL is de centrale waarheid.

2. **Explain before act**  
   Jarvis toont data, aannames, onzekerheid en risicocontroles vóór een mutatie.

3. **Human confirmation for consequential actions**  
   Orders, publicaties, betalingen, accountwijzigingen en verwijderingen vereisen bevestiging.

4. **Deterministic core, probabilistic assistant**  
   Bedragen, risk sizing, allocatie, ordervalidatie en backtestmetrics komen uit code.

5. **Provider-independent**  
   OpenAI of Claude als primaire API; Ollama als lokale fallback. Providers zijn adapters.

6. **One source of truth**  
   Brokerdata wordt gereconcilieerd met de eigen database; de broker wint bij verschillen.

## Hoofddomeinen

### Investeren

- Portfolio-overzicht
- Doelallocaties
- Maandelijkse inleg
- Nieuws en filings
- Earningsagenda
- Scenarioanalyse
- Ordervoorstellen en later bevestigde IBKR-orders

### Actief traden

- MT5 marktcontext
- Strategieonderzoek
- Signal intake
- Risicoberekening
- Demo/paper execution
- Backtesting en walk-forward
- Trade journal
- Live execution pas na expliciete promotie

### Content en inkomen

- Trends verzamelen
- Ideeën scoren
- Scripts en shot lists
- Voice-over, captions, renders
- Publicatievoorstellen
- Analyticsfeedback
- Geen blind kopiëren van auteursrechtelijk materiaal

### Persoonlijke assistent

- Chat, notities en geheugen
- Agenda/e-mail later als aparte connector
- Taken en routines
- Bestands- en documentonderzoek
- Development-assistent

## Gebruikerservaring

De gebruiker praat met één Jarvis. Intern kiest de orchestrator een agent en tools. De UI toont altijd:

- welke agent actief is;
- welke bronnen zijn gebruikt;
- welke toolcalls zijn uitgevoerd;
- wat feit, berekening of AI-inferentie is;
- welke actie wordt voorgesteld;
- welke toestemming nodig is.

## Niet-functionele eisen

- API p95 onder 500 ms voor gecachete reads.
- Alle ordermutaties idempotent.
- Audit-events append-only.
- Secrets nooit in clientlogs.
- Encryptie in transit en at rest.
- Dagelijkse databaseback-up.
- Reproduceerbare Docker-images.
- Herstelprocedure getest.
