# UI en interacties

## Hoofdscherm

- 3D-system view;
- actuele workflowstatus;
- kosten deze run;
- tokens;
- latency;
- errors/warnings;
- actieve approvals.

## Selecteren van een agent

Bij selectie:

- rol;
- status;
- huidige taak;
- parent/children;
- gebruikte tools;
- model;
- tokens en kosten;
- laatste berichten;
- permissions/scopes;
- success/error history.

## Timeline

Onder de 3D-view staat een tijdlijn:

```text
12:00:01 User request
12:00:02 Orchestrator selected Event Alpha
12:00:02 News Agent started
12:00:03 Verification completed
12:00:03 Market lookup
12:00:04 Risk denied
12:00:04 User response
```

## Modi

### Live

Toont huidige events.

### Replay

- kies eerdere agentrun;
- play/pause;
- snelheid 0.25×–8×;
- scrub tijdlijn;
- inspecteer ieder event.

### Explain mode

Jarvis maakt een begrijpelijke samenvatting van waarom agents en tools zijn gekozen.

### Cost mode

Nodegrootte of ring toont:

- tokens;
- eurokosten;
- runtime;
- API-provider.

### Security mode

Toont:

- scopes;
- sensitive data boundaries;
- blocked calls;
- approvals;
- risk decisions.

### Performance mode

Toont:

- latency;
- queue time;
- tool duration;
- bottlenecks;
- retries.

## Filters

- agents;
- tools;
- modelcalls;
- finance;
- memory;
- security;
- errors;
- cost threshold;
- trace/run.
