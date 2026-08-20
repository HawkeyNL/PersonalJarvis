# Memory Consolidation

## Doel

Veel ruwe data reduceren naar weinig relevante context.

## Pipeline

```text
raw messages/events
→ candidate extraction
→ deduplication
→ importance scoring
→ contradiction detection
→ compact memory
→ embedding
→ archive raw data
```

## Regels

- niet ieder bericht wordt memory;
- bron en timestamp bewaren;
- gebruiker-origin facts hebben hogere betrouwbaarheid;
- afgeleide facts krijgen confidence;
- gewijzigde voorkeuren superseden oude memories;
- oude memory blijft voor audit/history bestaan;
- financiële rules horen in versioned policy, niet vrije memory.

## Consolidation tiers

### Immediate

Na belangrijke expliciete beslissing.

### Session summary

Aan einde van gesprek of taak.

### Daily consolidation

Deduplicatie en conflicts.

### Monthly compaction

Zelden gebruikte episodes samenvoegen en archiveren.

## Goedkoop modelgebruik

Gebruik een klein model voor:

- classificatie;
- samenvatting;
- duplicate candidates;
- taggen.

Gebruik sterk model alleen voor:

- conflicten;
- complexe besluiten;
- onduidelijke entiteiten.

## Retrieval scoring

Combineer:

- semantische similarity;
- recency;
- importance;
- source confidence;
- scope match;
- entity match;
- validity;
- prior retrieval usefulness.

## Contextbudget

Iedere workflow krijgt een maximaal memory-tokenbudget. De Context Builder stopt wanneer het budget is gevuld.
