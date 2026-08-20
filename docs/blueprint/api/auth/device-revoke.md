# `auth/device-revoke`

## Doel

Typed endpoint voor `device-revoke` binnen `auth`.

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
