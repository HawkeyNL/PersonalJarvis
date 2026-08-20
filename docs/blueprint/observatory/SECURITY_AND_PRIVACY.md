# Security en privacy

## Niet tonen

- API-keys;
- access/refresh tokens;
- brokercredentials;
- wallet private keys;
- volledige gevoelige prompts;
- volledige retrieved documents;
- persoonsgegevens zonder expliciet doel.

## Backend-filter

Observatory-events worden server-side geredacteerd. De client mag niet zelf bepalen wat gevoelig is.

## Autorisatie

Scopes:

- `observatory.read.summary`
- `observatory.read.financial`
- `observatory.read.security`
- `observatory.read.payload`
- `observatory.replay`

Payloadinspectie vereist hogere rechten en wordt geaudit.

## Retentie

- summaries en metrics: langer bewaren;
- volledige debugpayloads: korte retentie of uitgeschakeld;
- trading/audit-events: volgens auditpolicy;
- modelstreamchunks: standaard niet opslaan.

## Prompt injection

Externe tooldata mag de Observatory niet manipuleren. Labels en summaries worden escaped en als data behandeld.
