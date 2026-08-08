# Runbook — trading kill switch

## Activeren bij

- onverwachte order;
- risk breach;
- reconciliation mismatch;
- broker/account mismatch;
- MCP toolset onverwacht veranderd;
- mogelijk gecompromitteerde credentials;
- abnormale agentactiviteit.

## Effect

- nieuwe proposals blokkeren;
- approvals ongeldig maken;
- submits/cancels/modifies blokkeren behalve expliciete emergency path;
- agents naar read-only;
- notificatie;
- audit event.

## Herstel

Handmatige root/admin review, credentials/sessioncontrole, reconciliation, incidentrapport en expliciete reset.
