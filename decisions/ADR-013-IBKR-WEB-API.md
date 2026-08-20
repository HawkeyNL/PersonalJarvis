# ADR-013 — IBKR via Client Portal Web API (read-only first)

## Status

Accepted — 8 augustus 2026 (resolves DEC-003).

## Besluit

Koppel IBKR via de **Client Portal Web API** (lokale Client Portal Gateway,
HTTPS op `localhost:5000`), niet via de TWS-socket-API. Start **read-only** in
de **paper**-omgeving.

## Reden

- REST/JSON past direct op de Rust/Axum-backend (`reqwest`), zonder Python/Java
  SDK-sidecar voor read-only.
- `docs/blueprint/integrations/IBKR.md` noemt de Web API "past conceptueel goed bij een backend".
- De TWS-API is uitgebreider maar socket-gebaseerd en operationeel zwaarder; niet
  nodig om posities/cash te lezen.

## Vorm

- Crate `jarvis-ibkr`: `IbkrClient` (reqwest) + getypeerde read-only responses
  (auth-status, accounts, positions), met contracttests op voorbeeld-payloads.
- Gateway-URL uit config (`JARVIS_IBKR_GATEWAY_URL`, default
  `https://localhost:5000/v1/api`).
- **Paper en live nooit verwarren:** bepaald door met welke credentials je in de
  gateway inlogt; de app laat het account kiezen.
- **Interactieve login (browser-SSO + 2FA) doet de gebruiker** in de gateway; de
  backend proxeert alleen read-only. Geen brokercredentials in de backend of
  client (conform ADR-002).

## Later

Writes (orders) blijven achter de eigen broker/risk-gateway (ADR-002), pas na
paper + reconciliation. De TWS-API blijft een optie als de Web API bepaalde data
mist.
