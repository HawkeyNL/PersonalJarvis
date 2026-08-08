# Memory Architecture

## Doel

Veel data bewaren met laag tokengebruik, laag energieverbruik en voorspelbare retrieval.

## Hoofdopzet

```text
Tauri clients
├── encrypted SQLite cache
├── secure session storage
└── geen centrale waarheid
        │
        ▼
Rust/Axum backend op VPS
├── Context Builder
├── Memory Agent
├── Consolidation Worker
├── Retrieval Service
└── Cost Tracker
        │
        ├── PostgreSQL + pgvector
        ├── Redis (optioneel, tijdelijk)
        └── Object storage / compressed archives
```

## Geheugenlagen

### Working memory

- actieve taak;
- laatste relevante berichten;
- tijdelijke toolresultaten;
- workflowstate.

Opslag:
- procesgeheugen;
- eventueel Redis;
- korte TTL.

### Episodic memory

- beslissingen;
- gebeurtenissen;
- gesprekken;
- belangrijke resultaten;
- fouten en lessen.

Opslag:
- PostgreSQL;
- compacte samenvatting;
- embedding via pgvector.

### Semantic memory

- duurzame feiten;
- voorkeuren;
- relaties;
- projecten;
- doelen;
- architectuurkeuzes.

Opslag:
- relationele PostgreSQL-tabellen;
- pgvector alleen voor retrieval.

### Operational memory

- taken;
- agentruns;
- orderstatussen;
- costs;
- alerts;
- systeemstatus.

Opslag:
- normale PostgreSQL-tabellen;
- nooit als losse vrije tekst wanneer typed data mogelijk is.

### Document memory

- PDF's;
- code;
- research;
- rapporten;
- transcripts;
- handleidingen.

Opslag:
- metadata en chunks in PostgreSQL;
- bestanden in object storage of archive;
- embeddings via pgvector.

### Archive

- volledige oude chats;
- ruwe agentlogs;
- grote exports;
- oude marktdata.

Opslag:
- gecomprimeerde JSONL/Parquet/Zstandard;
- object storage of beveiligde filesystemdirectory;
- zelden direct naar een LLM.

## Tokenbesparing

Jarvis stuurt niet standaard volledige historie mee.

Context Builder selecteert:

- huidige vraag;
- korte conversationsummary;
- relevante memories;
- relevante documenten;
- verse toolresultaten.

## Belangrijk principe

Opslagcapaciteit is goedkoop. Promptcontext is duur.

Bewaar dus veel, maar haal per taak weinig en relevant op.
