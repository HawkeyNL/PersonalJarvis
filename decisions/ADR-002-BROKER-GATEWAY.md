# ADR-002 — centrale broker- en riskgateway

## Status

Accepted.

## Besluit

Alle brokerwrites lopen door één eigen gateway met policy, risk, approval, idempotency en audit.

## Consequentie

MT5 native MCP en IBKR API worden adapters; geen agent krijgt rechtstreeks brokercredentials.
