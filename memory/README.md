# Jarvis Memory

Start met:

1. `MEMORY_ARCHITECTURE.md`
2. `POSTGRES_PGVECTOR.md`
3. `REDIS_POLICY.md`
4. `JSON_AND_ARCHIVES.md`
5. `CLIENT_CACHE.md`
6. `MEMORY_CONSOLIDATION.md`

Jarvis gebruikt een gelaagd geheugensysteem. PostgreSQL is de centrale waarheid. Redis is optioneel en tijdelijk. Grote ruwe bestanden gaan naar object storage of een archiefdirectory. Clients hebben alleen een versleutelde lokale cache.
