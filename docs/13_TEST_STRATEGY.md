# Teststrategie

## Unit tests

- money/decimal calculations
- position sizing
- allocation
- risk rules
- symbol mapping
- timezone handling
- schema validation
- idempotency
- permission checks

## Property-based tests

- order size nooit negatief;
- verliesbudget nooit overschreden;
- herhaalde idempotente command geeft geen tweede order;
- allocaties sommeren correct;
- rounding houdt broker constraints aan.

## Integration tests

- Postgres
- provider mocks
- IBKR paper/simulator
- MT5 demo/MCP staging
- WebSocket reconnect
- approval challenge
- reconciliation

## Contract tests

- OpenAPI
- MCP tool schemas
- broker response mapping
- model structured output
- market-data provider schemas

## Failure injection

- broker timeout na submit
- duplicate fill event
- partial fill
- stale quote
- MT5 disconnect
- expired approval
- model hallucineert onbekend symbol
- malicious instruction in news article
- DB failover
- queue retry

## Security tests

- auth/session
- CSRF/SSRF
- secret leakage
- prompt injection
- MCP tool poisoning
- scope escalation
- replay attacks
- dependency scanning
- container scanning

## Acceptance test voor live trading

Een live flag kan niet worden gezet zonder:

- succesvolle paperperiode;
- getekende checklist;
- restore test;
- kill-switch test;
- max-loss test;
- reconciliation test;
- user approval.
