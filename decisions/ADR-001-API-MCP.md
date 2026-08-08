# ADR-001 — API voor product, MCP voor agenttools

## Status

Accepted.

## Besluit

Tauri en interne domeinworkflows gebruiken een eigen API. MCP wordt gebruikt voor agenttooling en externe interoperabiliteit.

## Reden

- voorspelbare contracts;
- betere auth/idempotency;
- kleinere attack surface;
- MCP kan veranderen zonder clientcontract te breken;
- agents krijgen minimale tools.
