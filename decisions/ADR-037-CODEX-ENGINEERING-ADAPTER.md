# ADR-037 — Begrensde Codex engineering-adapter

## Status

Accepted — 20 augustus 2026.

## Context

PR #15 beschrijft Codex als een vervangbare, begrensde engineering-runtime voor
Jarvis. De bestaande agentlaag heeft al een getypeerde allowlist, policy-gate,
device-getekende goedkeuring, sandbox en audit-trail. Er is echter nog geen
taakmodel voor langdurig engineeringwerk of adapter voor de Codex App Server.

De officiële Codex-documentatie ondersteunt App Server als bidirectionele
JSON-RPC-integratie voor threads, turns, approvals en events. De WebSocket
transportmodus is daarbij experimenteel en niet productie-ondersteund. Een
publieke of permanente listener past daarom niet in de eerste Home-Node-slice.

## Besluit

Fase 1 voegt een zelfstandige `jarvis-codex`-crate toe met uitsluitend:

- een getypeerde engineering-taakstate-machine;
- begrensde tekst- en deadlinevelden;
- een kleine, allowlisted JSON-RPC-vocabulary voor `initialize`, thread/turn
  starten en een turn onderbreken.

De adapter is lokaal bedoeld voor stdio/JSONL. Hij exposeert geen HTTP-,
WebSocket- of MCP-listener en kent geen generieke command/process/shell-methoden.
Er is nog geen API-route, worker, systemd-unit, credential, worktree-creatie,
toolgrant of uitvoerder gekoppeld.

Een volgende slice mag Codex alleen starten in een expliciet aangemaakte,
geïsoleerde development-worktree. Zij moet `jarvis-policy` gebruiken voor de
`ExecuteCode`-beslissing, de bestaande getekende approvalflow onmiddellijk vóór
start verifiëren en audit/events toevoegen. `thread/shellCommand` en
`process/spawn` uit het App Server-protocol zijn geen toegestane Jarvis-calls.

## Gevolgen

- Core blijft de autoriteit; Codex kan in deze fase niets uitvoeren.
- WebSocket, openbare toegang, persistent accountmateriaal en live trading
  blijven buiten scope.
- De typed lifecycle maakt timeout, annulering en veilige voortgang later
  testbaar zonder protocoldetails door de rest van de codebase te verspreiden.
