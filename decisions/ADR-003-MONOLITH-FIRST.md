# ADR-003 — modular monolith first

## Status

Accepted.

## Besluit

Begin als modular monolith/klein aantal services in één Rust workspace. Splits pas op basis van operationele noodzaak.

## Reden

Persoonlijk project, minder deploymentcomplexiteit, eenvoudigere transacties en debugging.
