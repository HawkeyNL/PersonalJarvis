# ADR-029 — Agentische uitvoering (Jarvis krijgt handen), veilig achter policy + goedkeuring

- Status: **VOORGESTELD** (ontwerp — nog niets gebouwd, wacht op akkoord eigenaar) — 13 augustus 2026
- Bouwt op ADR-027 (kosten-router + registry + budget), ADR-028 (model-per-taak +
  plan→execute), de bestaande unlock/approval-flow (device-gebonden, cryptografisch),
  en `core/Jarvis.md` §11 (Risk-Based Autonomy), §12 (Financial Safety), §13
  (Security by Default), §28 (Reversibility), §29 (Auditability)

## Context

Fase 1–3 gaven Jarvis een **plan-brein zonder handen** (pure redenering). De
eigenaar wil dat Jarvis ook kan **doen**: terminal-commando's draaien, Claude Code
aansturen, bestanden bewerken — en zijn eigen ecosysteem uitbreiden. Dit is de
**gevaarlijkste laag**: een AI met uitvoerrechten op de machine van de eigenaar.

Daarom is dit een ontwerp-ADR: het legt de **veiligheidsgrenzen** vast vóór er
één commando kan draaien. Leidend principe (Jarvis.md §11): autonomie schaalt met
**omkeerbaarheid × impact**, nooit met gemak.

## Beslissing (voorgesteld)

Eén **policy-gated action executor** — alle uitvoering loopt door vier lagen; er
is geen andere weg naar de shell.

### Laag 1 — Capability-allowlist (statisch, geen vrije shell)

- Acties zijn **getypeerd en geparametriseerd**, geen vrije strings. Géén
  `sh -c "<willekeurig>"`. Elk actietype is een bekende vorm met bekende argumenten
  (bv. `Git{sub: Status|Diff|Log}`, `ReadFile{path}`, `CargoTest`, `ClaudeCode{prompt}`).
- Alles buiten de allowlist → **geweigerd**. Blocklists zijn te lekken; alleen een
  expliciete allowlist telt.
- Geen shell-metacharacters, geen pipes/redirects die de executor niet zelf zet.

### Laag 2 — Risico-classificatie → Auto | NeedsApproval | Denied

| Klasse | Voorbeelden | Gate |
|---|---|---|
| **Auto** (read-only, omkeerbaar) | `ls`, `cat`, `git status/diff/log`, `grep`, `cargo test/check` | uitvoeren + auditen |
| **NeedsApproval** (mutatie, omkeerbaar) | bestand schrijven, `git commit`, `git push`, Claude Code die schrijft, `cargo build` | phone/biometrie-goedkeuring per actie |
| **Denied** (gevaarlijk/onomkeerbaar) | `sudo`, `rm -rf`, netwerk-exfiltratie, secrets lezen (`.env`, keychain, `~/.ssh`), pakket-install, **live-trading**, **schrijven naar de Core** | nooit — hard geweigerd |

**De Core is een beschermd write-path (Jarvis.md §30).** Elke schrijf-/bewerk-/
verwijder-actie op `core/**` (incl. `core/Jarvis.md`), de policy/permissions-config,
de allowlist zelf, en secrets (`.env`, keychain) is **onvoorwaardelijk Denied** —
niet eens goedkeurbaar via de gate. Alleen de eigenaar wijzigt de Core, handmatig,
buiten de agent om. Zo kan geen enkel model (hoe slim ook, §31) of prompt-injectie
(§16) zijn eigen regels of veiligheidsgrenzen herschrijven.

### Laag 3 — Goedkeuringsgate (hergebruik de bestaande flow)

NeedsApproval-acties gebruiken de **bestaande device-gebonden unlock/approval-flow**
(`/v1/auth/unlock/*`): een vertrouwd toestel **tekent cryptografisch** een nonce
die de exacte actie vastlegt. Zo blijft **biometrie/telefoon het slot** en is een
goedkeuring niet vervalsbaar of herbruikbaar voor een andere actie. Goedkeuringen
zijn **tijdgebonden** en **actie-specifiek** (geen blanco cheque).

### Laag 4 — Sandbox, audit, kill switch

- **Sandbox**: acties alléén binnen een geconfigureerde `workspace-root` (bv. één
  projectmap). Nooit systeembreed, geen `sudo`, geen toegang tot secrets. Paden
  worden gecanonicaliseerd en gecontroleerd (geen `..`-escape).
- **Audit** (§29): elke actie → append-only Postgres-log (migratie `0008`):
  actor-device, actietype, argumenten, classificatie, goedkeuring-id, exit/resultaat,
  tijd. Onvervalsbaar en volledig terugleesbaar.
- **Kill switch**: één schakelaar (`JARVIS_AGENT_ENABLED=false`, default **false**)
  zet álle uitvoering uit. Uit staat is de veilige default; de eigenaar zet het
  bewust aan.
- **Timeout + limieten**: elke actie heeft een harde timeout en `kill_on_drop`.

## Fasering (elk zelfstandig, na akkoord)

- **4a — Read-only shell**: alleen Auto-acties binnen de sandbox + audit. Geen
  mutaties. Bewijst de policy-engine + audit end-to-end, met nul mutatierisico.
- **4b — Mutaties achter goedkeuring**: bestand schrijven / `git commit` via de
  approval-gate. Preview (dry-run/diff) vóór goedkeuring; voorkeur voor omkeerbaar (§28).
- **4c — Claude Code aansturen**: een plan→execute-stap (ADR-028) mag een headless
  CC-run zijn, in de sandbox, achter goedkeuring — Jarvis als orchestrator, CC als
  uitvoerder.
- **4d — Zelfontwikkeling**: nieuwe modellen/keys/tools detecteren (registry) →
  Jarvis **stelt voor**, activeert nooit autonoom iets betaalds (§12). Budget uit
  ADR-027 blijft de rem.
- **MCP (los spoor)**: Jarvis-eigen tools als MCP-server (read-only portfolio/
  geheugen) die Claude Code mag gebruiken; en Jarvis als MCP-host.

## Niet-doelen / harde grenzen

- **Geen live-trading, ooit, vanuit deze laag.** IBKR blijft read-only (ADR-013).
- Geen secrets in acties/logs/goedkeuringen (ADR-022). Sleutels blijven backend-only.
- Geen autonome onomkeerbare acties. Geen netwerk buiten expliciet toegestane hosts.
- De executor is single-user (de eigenaar), geen publieke multi-user route.

## Vastgestelde beleidskeuzes (eigenaar, 13 augustus 2026)

1. **Read-only acties**: **autonoom + alles auditen**. Jarvis draait veilige
   read-only commando's (ls, git status/diff, cargo test/check, grep) zelf binnen
   de sandbox; elke actie in het audit-log.
2. **Mutaties**: **per actie tekenen** — elke schrijf-/commit-actie apart
   goedgekeurd via de device-signed gate; een goedkeuring geldt voor precies die
   ene actie, niet herbruikbaar.
3. **Zelf-verbetering (sandbox = de PersonalJarvis-codebase zelf)**:
   - **alleen op verzoek** van de eigenaar — Jarvis begint hier nooit uit zichzelf;
   - hij **stelt eerst een plan op** (de plan→execute-orchestrator, ADR-028 fase 3),
     toont dat, en voert het pas uit **na goedkeuring per stap**;
   - hij mag aan de **codebase** werken (crates, services, apps, niet-Core docs),
     maar niet aan de Core.

### Onaantastbaar — nooit via Jarvis, ook niet met goedkeuring

- **`core/Jarvis.md`**: Jarvis wijzigt zijn eigen grondwet **nooit**. Punt.
- **De sloten zelf**: policy, permissions, de allowlist, de veiligheidsconfig en
  secrets (`.env`, keychain). Als de agent zijn eigen hek (met één getekende
  goedkeuring) kon verzetten, ondermijnt dat de hele gate (§31 — Policy Is Not
  replaceable, §16 — prompt-injectie is onvertrouwd). Deze blijven **puur
  handmatig door de eigenaar**, buiten elke agentische route om.

Dit is strenger dan "de Core niet zonder toestemming": de Core-sloten zijn niet
eens goedkeurbaar. Zo blijft "alleen de eigenaar verandert de Core" een garantie,
niet een gunst — en kan Jarvis tóch aan de rest van zichzelf werken.

## Gevolgen

- Veiligheid is structureel, niet per-prompt: de allowlist + sandbox + gate zitten
  in de executor, niet in de systeemprompt (die is te omzeilen — §16 Prompt
  Injection Is Untrusted Input).
- Herbruikt de bestaande crypto-goedkeuring → geen nieuw vertrouwensmechanisme.
- Start dicht (kill switch default uit, allowlist leeg-op-veilig); de eigenaar
  opent bewust, stap voor stap.
