# JSON, JSONB en archieven

## JSON is een formaat, geen aparte database

Jarvis gebruikt JSON op drie manieren.

## 1. API-berichten

Tauri en backend communiceren via gevalideerde JSON DTO's.

```text
Tauri
→ HTTPS JSON
→ Axum
→ validation
→ domain command
→ PostgreSQL
```

## 2. JSONB in PostgreSQL

Geschikt voor flexibele metadata, niet voor alle kernvelden.

Voorbeeld:

```json
{
  "entities": ["Jarvis", "AI hardware"],
  "tags": ["decision", "budget"],
  "retrieval_count": 3,
  "prompt_version": "memory-v2"
}
```

## 3. Grote gecomprimeerde archieven

Gebruik voor:

- volledige conversaties;
- raw agent traces;
- oude exports;
- grote researchbundels;
- marktdata buiten actieve retentie.

Formaten:

- `.jsonl.zst`
- Parquet
- compressed CSV alleen indien noodzakelijk
- binary market-data format indien performance vereist.

## Plaatsing

Eerste versie:

```text
/var/lib/jarvis/archive/
├── conversations/
├── agent-runs/
├── documents/
└── market-data/
```

Later:

- MinIO;
- S3-compatible storage;
- encrypted bucket;
- lifecycle policies.

## Regels

- raw archive is immutable;
- metadata/index staat in PostgreSQL;
- encrypt at rest;
- retentie per datatype;
- geen secrets in exports;
- archive retrieval wordt gelogd.
