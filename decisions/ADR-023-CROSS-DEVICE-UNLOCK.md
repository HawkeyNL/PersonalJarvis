# ADR-023 — App-vergrendeling met biometrie + ontgrendelen via telefoon

- Status: geaccepteerd — 9 augustus 2026
- Context: device-bound auth (ADR — Ed25519 challenge-response, `jarvis-identity`);
  bouwt voort op de bestaande apparaten/sleutels

## Context

De desktop-app moet achter een slot kunnen: bij het openen verifieer je jezelf
voordat je bij Jarvis (chat, portfolio, straks trading) kunt. We willen **geen**
zelfgebouwd wachtwoordscherm — dat is een aanvalsoppervlak — maar de OS-biometrie
(Touch ID / Face ID). De gebruiker wil bovendien dat, als de biometrie op de
desktop faalt, hij **niet** het desktop-wachtwoord hoeft te typen maar kan
**goedkeuren via zijn telefoon** ("die heb ik altijd bij me"), waarbij de
telefoon terugvalt op zijn eigen biometrie/toegangscode.

## Beslissing

**Twee-traps ontgrendeling, opt-in per Settings-toggle (standaard uit):**

1. **Desktop — lokale biometrie (biometrics-only).** Native command
   `biometric_unlock` via `robius-authentication` (`LAPolicy`
   DeviceOwnerAuthenticationWithBiometrics). Bewust **zonder**
   device-wachtwoord-fallback, zodat een mislukte biometrie naar stap 2 leidt in
   plaats van naar het desktop-wachtwoord.
2. **Ontgrendelen via telefoon (cross-device approval).** De desktop maakt een
   `unlock_request` (random nonce) en pollt op de status. Een ander actief
   apparaat van dezelfde gebruiker (de telefoon) ziet het verzoek, doet een
   **lokale biometrie-check mét wachtwoord-fallback** en **tekent de nonce met
   zijn device-sleutel** — dezelfde Ed25519-sleutel als bij login. De backend
   verifieert de handtekening tegen de publieke sleutel van dat apparaat en zet
   het verzoek op `approved`; de desktop ziet dat bij de volgende poll en
   ontgrendelt.

Eigenschappen:
- Alleen de **telefoon** vergrendelt zichzelf niet in-app (die is de
  goedkeurder en zit al achter het toestelslot); desktops vergrendelen wél.
- Een apparaat kan zijn **eigen** ontgrendeling niet goedkeuren.
- Verzoeken verlopen na 2 minuten; de handtekening bewijst bezit van de
  device-sleutel — er gaat **nooit een wachtwoord over de lijn**.
- Auto-relock na 5 min inactiviteit (reset op interactie).
- De telefoon kan een verzoek ook expliciet **weigeren** (`denied`, geen
  handtekening nodig); de desktop toont dat direct.
- **Near-push via long-polling**: de status- en pending-endpoints accepteren
  `?wait=<secs>` (server houdt de request ~20 s open en antwoordt zodra de
  status wijzigt), dus goedkeuring voelt onmiddellijk zonder strak poll-interval.
- Faalt de desktop-biometrie of ontbreekt de hardware (Mac zonder Touch ID),
  dan start de telefoon-route **automatisch**.

Endpoints (allemaal Bearer-beveiligd): `POST /v1/auth/unlock/request`,
`GET /v1/auth/unlock/pending`, `GET /v1/auth/unlock/{id}`,
`POST /v1/auth/unlock/{id}/approve`. Tabel `unlock_requests` (migratie 0005).

## Alternatieven

- **Desktop-wachtwoord als fallback** (`deviceOwnerAuthentication`): simpelst,
  maar precies wat de gebruiker niet wil; de telefoon-route is prettiger en
  hergebruikt de bestaande device-trust.
- **Push-notificatie i.p.v. polling**: mooier voor achtergrond-wakeup, maar
  vereist een push-kanaal (APNs/websockets). We doen nu **long-polling**
  (`?wait=`), wat voorgrond-latency al wegneemt; échte push blijft een latere
  optimalisatie voor wanneer de app op de achtergrond staat.
- **Nieuw sleutelpaar / apart approval-secret**: overbodig — de device-sleutels
  uit de login bewijzen apparaatbezit al.
- **Altijd vergrendelen**: verworpen als default — opt-in via Settings, zodat
  ontwikkelen niet elke launch een prompt geeft.

## Gevolgen

- Sterke, telefoon-gedragen ontgrendeling zonder zelfgebouwd wachtwoordscherm;
  hergebruikt de Ed25519-device-trust.
- Legt de basis voor **biometrische goedkeuring in de gated trading-keten**
  (order-approval): hetzelfde patroon (verzoek → biometrie → getekende
  goedkeuring) is straks herbruikbaar.
- iOS heeft `NSFaceIDUsageDescription` nodig (toegevoegd aan de Info.plist).
- Nog te doen: échte push (APNs) voor achtergrond-wakeup i.p.v. long-polling;
  meerdere gelijktijdige verzoeken netjes tonen/afhandelen.
