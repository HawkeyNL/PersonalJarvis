@AGENTS.md

# Claude Code — werkwijze PersonalJarvis

De regels in AGENTS.md (hierboven ingeladen) blijven leidend. Dit bestand voegt
alleen toe hoe Claude Code werkt.

## Rolverdeling
- **Jij (hoofdsessie, Sonnet) bent de orchestrator.** Je deelt de taak op,
  voert uit en controleert het resultaat.
- **Code lezen en zoeken:** start een subagent met model `sonnet`. Hij rapporteert
  bevindingen; jij houdt de conclusie, niet de bestandsdumps.
- **Plannen en moeilijk denkwerk:** start een subagent met model `opus` vóór je
  bouwt. Dat geldt voor elke niet-triviale taak en zeker voor architectuur,
  security, concurrency, performance en wijzigingen over meerdere crates.
- **Review:** laat `opus` de diff nalopen van alles wat security, auth,
  approvals, policy, de sandbox of de broker raakt, vóór je het af noemt.

## Eerst vragen, dan doen
- Doe precies wat gevraagd is. Geen ongevraagde features, refactors,
  dependencies, bestanden of config.
- Ideeën zijn welkom: zet ze kort onder **Voorstellen** en bouw ze pas na een
  expliciet "ja".
- Vraag altijd eerst bij:
  - een nieuwe dependency of crate;
  - een schema- of migratiewijziging;
  - wijzigingen aan policy, approvals, auth, sandbox of broker;
  - iets verwijderen;
  - commit, push of PR;
  - alles op de host buiten de repo (systemd, firewall, Docker, packages).

## Dubbel checken
Voor je zegt dat iets klaar is:
1. Lees je eigen diff (`git diff`) helemaal na. Alleen wat nodig is, zonder
   debugresten of uitgecommentarieerde code.
2. Draai de checks uit AGENTS.md voor wat je hebt aangeraakt.
3. Bij security- of concurrency-code: laat `opus` reviewen en verwerk wat hij
   vindt.
4. Meld wat je hebt gedraaid en wat eruit kwam. Kon iets niet geverifieerd
   worden? Zeg dat eerlijk in plaats van het af te noemen.

## Security
- Bij twijfel: fail closed en vraag.
- Lees, log, print of commit nooit secrets, tokens, keys of `.env`-inhoud.
- Valideer alle input aan de grenzen: API, IPC en sandbox-artifacts. Gebruik
  geen `unwrap`/`expect` op externe input.
- Voeg geen open poorten, publieke endpoints of shell-uitvoering toe.
- Geef nieuwe code de kleinste rechten die werken.

## Performance
- Let op hot paths: geen onnodige clones of allocaties, geen blocking I/O in
  async code, en zet limieten en timeouts op queues, retries en I/O.
- Meet voor je optimaliseert. Maak code niet complexer zonder bewijs dat het
  sneller wordt.

## Code
- Kies simpel en leesbaar boven slim. Volg de bestaande stijl en patronen van
  de crate.
- Houd wijzigingen klein en gericht: één onderwerp per commit of PR.
- Gebruik typed errors en duidelijke namen. Schrijf alleen comments waar het
  "waarom" niet vanzelf spreekt.
- Schrijf tests bij nieuw gedrag en bij elke bugfix.

## Lokale instructies
Machine-specifieke afspraken (welke host, wat er nog meer draait) staan in een
lokaal, niet-gecommit `CLAUDE.local.md`.

## Communicatie
- Antwoord in het Nederlands, kort en concreet.
- Begin een taak met een plan van een paar regels.
- Sluit af met wat er is veranderd, wat er is geverifieerd en wat er nog open
  staat.
