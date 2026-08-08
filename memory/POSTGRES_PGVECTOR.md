# PostgreSQL + pgvector

## Plaatsing

PostgreSQL draait centraal op de VPS in Docker of als managed service.

```text
VPS
├── api
├── workers
├── postgres + pgvector
├── reverse proxy
└── backup job
```

PostgreSQL is niet publiek bereikbaar. Alleen interne services mogen verbinden.

## Verantwoordelijkheden

- users en devices;
- gesprekken en summaries;
- memories;
- goals en tasks;
- portfolio en tradingdata;
- agentruns;
- costs;
- audit;
- embeddings via pgvector.

## Voorbeeldtabellen

```sql
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE memories (
    id uuid PRIMARY KEY,
    memory_type text NOT NULL,
    scope text NOT NULL,
    content text NOT NULL,
    summary text,
    importance numeric(5,4) NOT NULL DEFAULT 0.5,
    confidence numeric(5,4) NOT NULL DEFAULT 1.0,
    source_type text NOT NULL,
    source_id uuid,
    valid_from timestamptz,
    valid_until timestamptz,
    supersedes_id uuid REFERENCES memories(id),
    sensitivity text NOT NULL DEFAULT 'normal',
    metadata jsonb NOT NULL DEFAULT '{}',
    embedding vector(1024),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
```

## Gebruik gewone kolommen voor

- bedragen;
- datums;
- statussen;
- scopes;
- rechten;
- confidence;
- importance;
- validiteit;
- foreign keys.

## Gebruik JSONB voor

- flexibele metadata;
- provider-specifieke velden;
- debug/contextinformatie;
- niet-kritieke uitbreidbare attributen.

Stop geen belangrijke businessregels uitsluitend in JSONB.

## Vectorgebruik

Embeddings worden gebruikt voor:

- semantische retrieval;
- gelijkaardige memories;
- duplicate detection;
- document chunks;
- conversation recall.

Vectors zijn niet de autoriteit voor exacte feiten.

## Indexering

Start klein:

- normale B-tree indexes;
- pgvector exact search of eenvoudige index;
- pas HNSW/IVFFlat toevoegen wanneer dataset en metingen dat rechtvaardigen.

## Backup

- dagelijkse encrypted backup;
- point-in-time recovery indien haalbaar;
- periodieke restoretest;
- embeddings mogen opnieuw gegenereerd kunnen worden;
- relationele feiten en raw sources zijn belangrijker dan vectorindexen.
