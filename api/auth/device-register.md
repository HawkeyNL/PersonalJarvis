# `auth/device-register`

## Doel

Typed endpoint voor `device-register` binnen `auth`.

## Eisen

- authenticatie en scope;
- request schema;
- validation;
- idempotency bij mutaties;
- audit;
- RFC 7807 errors;
- contract tests.

## OpenAPI

Maak het exacte schema tijdens implementatie; domeintypes zijn niet rechtstreeks publieke DTO's.
