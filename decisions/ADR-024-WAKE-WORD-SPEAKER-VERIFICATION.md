# ADR-024 — "Hey Jarvis" wake-word + stem-verificatie (alleen jouw stem)

- Status: geaccepteerd — 9 augustus 2026
- Context: sluit deels JAR-151 (alleen jouw stem) af; raakt DEC-009 (STT) niet;
  bouwt op ADR-023 (app-lock) en de voice-output-policy (ADR-021)

## Context

De gebruiker wil Jarvis activeren met **"Hey Jarvis"** en dat **alleen zijn eigen
stem** dat doet. Dit moet privacy-vriendelijk: audio mag het toestel niet
verlaten. En het mag de zojuist gebouwde biometrische beveiliging niet
ondermijnen.

## Beslissing

**On-device met Picovoice**, volledig lokaal (WASM in de webview):

- **Porcupine** voor het wake-word — **"Jarvis" is een ingebouwd keyword**, dus
  geen custom training nodig.
- **Eagle** voor **speaker verification** — je neemt je stem één keer op
  (enrollment → profiel), daarna geeft Eagle per frame een score [0,1]; boven een
  drempel (0.5) geldt het als "jij".
- Beide draaien tegelijk op de mic-stream via `WebVoiceProcessor`. Bij een
  wake-detectie checkt de app de laatste Eagle-score; alleen bij een match volgt
  actie.

**Security-posture (belangrijk): stem is een *gemaks*-laag, geen beveiliging.**
Stem-herkenning is spoofbaar (opnames, gelijkende stemmen), dus:
- **App ontgrendeld** → "Hey Jarvis" (jouw stem) onthult de console en start de
  mic (activatie).
- **App vergrendeld** → "Hey Jarvis" (jouw stem) **start alleen de Touch
  ID-prompt**; de biometrie/telefoon-goedkeuring uit ADR-023 blijft vereist.
  Stem ontgrendelt het slot dus nooit alleen.

Opt-in per Settings-toggle (standaard uit). De Picovoice **AccessKey** en het
**stemprofiel** blijven lokaal (nu localStorage; keychain is een latere
verbetering). Alles is dynamisch geïmporteerd en feature-flagged, zodat de app
zonder de feature volledig onaangetast is. Modelbestanden (`*.pv`) staan niet in
git; ze worden gehaald met `npm run fetch-models`.

## Alternatieven

- **Browser `SpeechRecognition`**: werkt niet betrouwbaar in WKWebView en kan
  geen sprekers onderscheiden — afgewezen.
- **Open-source (openWakeWord + open embeddings, ONNX)**: geen account nodig,
  maar zwaarder te integreren en minder betrouwbaar — later een optie als we van
  Picovoice af willen (net als de LLM-abstractie blijft dit vervangbaar).
- **Stem ontgrendelt alles**: afgewezen — te zwak voor een app die trading raakt.
- **Native Rust-engine (cpal + Porcupine/Eagle Rust)**: robuuster voor
  achtergrond-luisteren, maar veel meer plumbing; de web-SDK hergebruikt de
  bestaande mic-pipeline. Kan later voor achtergrond-wakeup.

## Gevolgen

- Handsfree, privé activatie die de biometrische grens respecteert.
- Externe afhankelijkheid (Picovoice AccessKey, gratis persoonlijke tier),
  client-side — geen backend-secret.
- Nog te doen: stemprofiel/sleutel naar de keychain; drempel/gevoeligheid tunen
  op toestel; eventueel native achtergrond-luisteren; live verificatie met echte
  AccessKey op Mac + iPhone.
