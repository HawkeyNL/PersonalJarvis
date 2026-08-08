# Security- en risicomodel

## Dreigingsmodel

Bescherm tegen:

- gestolen API-keys;
- prompt injection in nieuws, documenten of MCP-resultaten;
- kwaadaardige of gewijzigde MCP-server;
- hallucinerende orderparameters;
- dubbele orders;
- stale quotes;
- te grote positie;
- account mismatch;
- broker reconnect en gedeeltelijke fills;
- gecompromitteerde client;
- supply-chain aanval;
- Windows VPS compromise;
- operatorfout.

## Trust boundaries

1. Tauri-client is nooit volledig vertrouwd.
2. LLM-output is onbetrouwbare input.
3. Externe content is onbetrouwbare input.
4. MCP tool metadata is niet automatisch vertrouwd.
5. Brokerresponse is gezaghebbend voor uitvoeringsstatus.
6. Risk engine en approval service zijn autoritatief.

## Orderveiligheid

Elke order vereist:

- toegestane broker/account;
- toegestane asset/symbol;
- verse quote;
- gevalideerd ordertype;
- maximale notional;
- maximale risicoblootstelling;
- stop-loss indien policy dit vereist;
- max daily loss check;
- max drawdown check;
- max concurrent positions;
- trading hours policy;
- spread/slippage policy;
- margin/available funds check;
- duplicate fingerprint check;
- expliciete user approval;
- idempotency key;
- reconciliation na submit.

## Modussen

| Modus | Gedrag |
|---|---|
| Research | Alleen lezen/analyseren |
| Shadow | Maakt voorstellen, verzendt niets |
| Paper/demo | Orders alleen naar paper/demo |
| Assisted live | Iedere order handmatig bevestigen |
| Rule-automated | Alleen vooraf goedgekeurde strategie en grenzen |
| Emergency stop | Alle mutaties geblokkeerd |

Begin uitsluitend in Research en Shadow.

## Approval flow

1. Backend maakt proposal.
2. Proposal wordt immutable opgeslagen.
3. Client toont instrument, richting, hoeveelheid, worst-case verlies en aannames.
4. Backend stuurt nonce/challenge.
5. Gebruiker bevestigt met biometrie/pincode.
6. Client tekent challenge via device-bound key.
7. Approval service controleert TTL en exact proposal hash.
8. Risk engine herberekent met verse data.
9. Broker gateway submit.
10. Reconciliation en audit.

## Secrets

- Gebruik Vault/SOPS/Docker secrets of minimaal root-only env files.
- Geen secrets in Git, logs, crash dumps of prompts.
- OpenAI/Claude-keys alleen server-side.
- IBKR/MT5 credentials gescheiden van appdatabase.
- Per integratie eigen key en budgetlimiet.
- Regelmatige rotatie.
- Device tokens revocable.

## Netwerk

- TLS overal.
- Windows MT5-VPS niet publiek exposen.
- WireGuard/Tailscale tussen backend en Windows-VPS.
- Firewall allowlist.
- MCP alleen via beveiligde tunnel of localhost proxy.
- Database niet publiek.
- Egress allowlist voor gevoelige services.

## Audit

Append-only events bevatten:

- actor/user/device/agent;
- model + versie;
- prompttemplateversie;
- gebruikte tools;
- input hashes;
- policybeslissing;
- approval;
- brokerrequest/response IDs;
- timestamps;
- redacted errors.

Bewaar nooit volledige secrets of onnodige persoonlijke inhoud.

## Prompt injection

- Scheid system instructions, user intent en retrieved content.
- Markeer externe tekst expliciet als data.
- Laat retrieved content geen tools autoriseren.
- Gebruik tool allowlists per workflow.
- Geen toolcalls op basis van één documentregel.
- Mutaties vereisen deterministische policy en user approval.
- Strip/escape actieve markup waar nodig.
