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

`jarvis-workspace` valideert daarnaast een detached worktree-plan vanaf een
immutable commit. Het kan geen Git-proces starten en heeft geen delete-operatie;
die bevoegdheid wordt pas met signed approval en audit gekoppeld.

## App Server schema-check (lokaal uitgevoerd)

De lokaal geïnstalleerde Codex-versie genereert een versioned JSON-schema voor
App Server. Dat bevestigt `turn/start` met een `cwd`, een sandbox-policy en
server-initiated command/file-change approval callbacks. De schema's bevatten
echter ook expliciete shell/process-capabilities. Jarvis mag die niet als een
generieke doorgeefluik behandelen.

Cruciaal: een `cwd` of modelinstructie maakt `jarvis-core/**` niet
onwijzigbaar. Ook een diffscan na afloop is alleen detectie. Een echte executor
mag daarom pas starten wanneer de dedicated, onprivileged Linux-identity op
OS-niveau uitsluitend in de engineering-worktree kan schrijven en
`jarvis-core/**`, `.git/**` en secretpaden daarbinnen niet kan muteren. De
owner beheert die permissies; Jarvis/Codex krijgt nooit de bevoegdheid ze te
veranderen.

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
2. dedicated Linux-identity + OS-level write bescherming voor een worktree;
3. policy+signed approval, audit, taskopslag en veilige voortgang vóór start;
4. daarna pas read-only scoped MCP;
5. Home-Node-installatie/documentatie pas wanneer de lokale adapter en recovery
   aantoonbaar werken.
