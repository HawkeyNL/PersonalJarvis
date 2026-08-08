# Tabel `trend_items`

## Doel

Persistentie voor het `trend_items`-concept.

## Ontwerpeisen

- primaire ULID/UUID;
- `created_at`/`updated_at` waar passend;
- UTC timestamps;
- numeric/decimal voor geld;
- foreign keys;
- minimale gevoelige data;
- relevante unique/index constraints;
- audit/eventkoppeling;
- retentiebeleid.

## Migratie

Definieer exacte kolommen tijdens implementatie vanuit het domeincontract en voeg een rollback-/backfillplan toe.
