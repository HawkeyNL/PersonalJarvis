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

Fase 2 voegt `jarvis-workspace` toe. Die crate maakt nog geen worktree, maar
valideert een plan voor precies één `git worktree add --detach` vanaf een
immutable commit-id. De doelmap moet buiten de live repository liggen en de
caller krijgt een vaste argv-lijst, geen shell-string. Uitvoeren, opruimen en
een API-route wachten op de approval- en auditkoppeling.

De fase-1-crate vraagt nu al uitsluitend `jarvis-policy` om de
`ExecuteCode`/`Mutating`-beslissing: een vertrouwd toestel krijgt
`RequireApproval`, een onvertrouwde caller `Deny`. Dit is nog geen approval;
alleen de bestaande device-handtekening kan die later werkelijk bewijzen.

Voordat fase 3 een `codex app-server`-proces mag starten, is een dedicated
onprivileged Linux-identity met OS-level schrijfgrenzen een harde
afhankelijkheid. App Server's `cwd`, sandboxinstellingen, tool-approvals en een
diffscan zijn aanvullende lagen, maar bewijzen niet dat `jarvis-core/**` in een
worktree niet te wijzigen is. Een proces dat die grens niet aantoonbaar
handhaaft, wordt niet geïntroduceerd.

## Gevolgen

- Core blijft de autoriteit; Codex kan in deze fase niets uitvoeren.
- WebSocket, openbare toegang, persistent accountmateriaal en live trading
  blijven buiten scope.
- De typed lifecycle maakt timeout, annulering en veilige voortgang later
  testbaar zonder protocoldetails door de rest van de codebase te verspreiden.

## Home Node-delegatie

Voor de eerste optionele Home Node-Codex-inzet is beperkte Unix-socket
groepsdelegatie gekozen. `jarvis-codex` bezit de App Server en diens
credential-state. Het Core-account `jarvis` wordt uitsluitend lid van de
groep `jarvis-codex` om de lokale runtime-socket te bereiken; het krijgt geen
toegang tot de Codex-state-directory met modus `0700`.

Engineering-worktrees worden voorbereid door een root-only operator-helper,
niet door de API of een agent-tool. Deze maakt `jarvis-core` en `.git`
root-owned en filesystem-immutable alvorens de rest van de worktree aan
`jarvis-codex` te geven. De helper faalt gesloten wanneer immutable
attributes niet kunnen worden toegepast. Dit is een begrensde
besturingssysteemgrens, geen generieke root-broker.
