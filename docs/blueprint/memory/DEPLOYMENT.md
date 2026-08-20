# Memory Deployment

## Minimale eerste versie

```text
VPS
├── jarvis-api
├── jarvis-worker
├── postgres + pgvector
├── caddy
└── encrypted backup job
```

Nog geen Redis en nog geen MinIO verplicht.

## Uitgebreide versie

```text
VPS / private infrastructure
├── API
├── workers
├── PostgreSQL + pgvector
├── Redis
├── object storage
├── backup service
├── metrics
└── alerting
```

## Capaciteit

Tekstmemories en embeddings gebruiken relatief weinig opslag. Grote volumegroei komt vooral uit:

- raw market depth;
- ticks;
- audio;
- video;
- document originals;
- generated assets.

Daarom krijgen marktdata en media afzonderlijke retention policies.

## Energie

PostgreSQL en Redis zijn bij lage persoonlijke belasting doorgaans niet de grootste energieverbruikers. LLM-inference, video en continu market-data processing zijn zwaarder.
