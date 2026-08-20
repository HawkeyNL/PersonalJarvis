# Tabel `cash_balances`

## Doel

Persistentie voor het `cash_balances`-concept.

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
