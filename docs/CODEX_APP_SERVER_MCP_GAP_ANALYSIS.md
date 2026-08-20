# Codex App Server — gap-analyse voor fase 1

Dit document beoordeelt het voorstel in GitHub PR #15 tegen `main` van 20
augustus 2026. Het is geen implementatie van MCP-tools of tradingfunctionaliteit.

## Wat al bestaat

- `jarvis-policy` beslist runtime capability/risk; `jarvis-agent` adapteert die
  beslissing en heeft geen eigen allow/deny-pad.
- Mutaties vereisen een device-getekende, eenmalige en vervallende goedkeuring.
- De agentsandbox weigert workspace-escape, secrets, `.git/**` en
  `jarvis-core/**`; de Claude Code-executor is een tweede opt-in.
- `POST /mcp` is eigenaar-gebonden en uitsluitend read-only.
- Jarvis Core draait native onder systemd; SurrealDB is private ondersteuning.

## Ontbrekende grenzen

1. Een typed engineering-tasklifecycle met deadline en veilige annulering.
2. Een beperkte Codex App Server-adapter; protocoldetails zitten nu nergens.
3. Een development-worktree-manager die nooit de actieve release gebruikt.
4. Taakgebonden capability-grants, audit-events en veilige progress-events.

## Fase 1

`jarvis-codex` levert alleen (1) en (2) als pure, niet-gekoppelde fundering.
Het accepteert uitsluitend de App Server-methoden `initialize`, `thread/start`,
`turn/start` en `turn/interrupt`. Het kan dus niet per ongeluk
`thread/shellCommand`, `command/exec` of `process/spawn` doorgeven.

De officiële App Server-documentatie adviseert stdio als standaardtransport;
WebSockets zijn experimenteel en niet productie-ondersteund. Daarom komt er in
deze fase geen listener of service op de Home Node.

## Niet-doelen

- geen Codex-account- of API-credentials in Jarvis-configuratie, prompts of git;
- geen publieke API, WebSocket, systemd-service of permanente App Server;
- geen MCP-grants, shell, filesysteemtoegang, worktree-creatie of deploy;
- geen directe of indirecte live-tradingactie.

## Vervolgvolgorde

1. review/merge van PR #15 als productplan;
2. geïsoleerde worktrees en policy+signed-approval vóór een echte start;
3. audit, taskopslag en veilige voortgang; daarna pas read-only scoped MCP;
4. Home-Node-installatie/documentatie pas wanneer de lokale adapter en recovery
   aantoonbaar werken.
