# Observability en operations

## Metrics

- API latency/error rate
- queue depth
- agent latency/tokens/cost
- model failure/fallback rate
- MCP tool latency/errors
- broker connectivity
- stale market data
- orders proposed/submitted/filled/rejected
- reconciliation mismatches
- risk denials
- backtest durations
- content render failures
- database/storage usage

## Logging

- structured JSON;
- correlation/request/agent/order IDs;
- secrets redaction;
- geen volledige prompts standaard in productie;
- aparte auditlog;
- logretentiebeleid.

## Tracing

OpenTelemetry over:

- client request
- API
- agentrun
- toolcalls
- brokeradapter
- database
- notification

## Alerts

- broker disconnected;
- MT5 MCP unavailable;
- IBKR session expired;
- risk limit breach;
- order status unknown;
- reconciliation mismatch;
- database backup failed;
- disk <20%;
- excessive AI spend;
- unusual number of toolcalls.

## Back-ups

- dagelijkse encrypted Postgres backup;
- point-in-time recovery waar haalbaar;
- object storage versioning;
- kwartaalgewijze restore test;
- secrets apart back-uppen;
- Windows VPS-config en EA-code in Git, niet alleen image backup.

## Incident runbooks

Zie `runbooks/`.
