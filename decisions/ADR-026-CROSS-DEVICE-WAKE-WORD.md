# ADR-026 — "Hey Jarvis" wake-word op elk device

- Status: geaccepteerd (scaffold) — 9 augustus 2026; auto-detector nog te bouwen
- Bouwt op ADR-025 (server-side spraak) en ADR-023 (app-lock)

## Context

De gebruiker wil "Hey Jarvis" op **elk toestel** (macOS-desktop én iPhone), en
alleen zíjn stem mag Jarvis wekken. Picovoice viel af (ADR-024: signup weigert
persoonlijke e-mail). Vraag: kan een wake-word op elk device?

## Beslissing

**Ja — mits de wake-word _lokaal per device_ draait.** "Hey Jarvis" continu naar
de server streamen kan niet (privacy + 24/7 bandbreedte). Elk toestel luistert
zelf en stuurt pas ná een treffer een kort fragment naar de server voor
speaker-verify + STT (de keten uit ADR-025).

**"Elk device" lossen we op door de detector in de gedeelde webview te draaien.**
De hele UI is dezelfde Tauri-webview op macOS en iOS, dus één implementatie dekt
alle toestellen — geen aparte native build per platform. Concreet:

- **Detector**: [openWakeWord](https://github.com/dscripka/openWakeWord) —
  open-source, permissieve licentie, met een kant-en-klaar **`hey_jarvis`**-model
  (ONNX). Draait via **onnxruntime-web** (WASM) in de webview. Klein model,
  offline, geen account. (Native `ort`-crate in de Tauri-Rust-laag blijft een
  optie voor lager verbruik/achtergrond, maar is niet nodig voor "elk device".)
- **Controller** (`voicewake.ts`): platform-agnostisch. Beheert aan/uit
  (persistent), start/stopt de detector, en heeft één `triggerWake()`-pad dat
  élke detectie doorloopt. Zo is de downstream-keten exact wat er live gaat.
- **Speaker-gate**: bij een treffer → `POST /v1/voice/verify` (ADR-025). Alleen
  bij `is_you` wekt Jarvis (reveal console + luisteren). Niet ingeschreven →
  wekken als gemak, zonder gate.
- **Security-posture (ongewijzigd)**: stem/wake = *gemak*, geen slot. Is de app
  vergrendeld, dan start een wake hooguit Touch ID / telefoon-goedkeuring —
  biometrie blijft de grens.

**Tussenoplossing tot de auto-detector er is:** een handmatige trigger
(`⌘/Ctrl + ⇧ + J` in de webview, en een native `wake-detected`-event-hook) loopt
exact hetzelfde pad, zodat reveal → verify → luisteren nu al werkt en getest is.

## Alternatieven

- **Web Speech API** (`webkitSpeechRecognition`) continu als keyword-spotter:
  niet beschikbaar/onbetrouwbaar in WKWebView (macOS/iOS) — afgevallen.
- **Native detector per platform** (Rust `ort` + cpal): lager verbruik en
  achtergrond-luisteren, maar dubbele build (macOS + iOS-toolchain). Bewaard als
  latere optimalisatie; niet nodig om "elk device" te halen.
- **Server-side always-on**: privacy + bandbreedte onacceptabel.

## Gevolgen

- Controller + speaker-gate + aan/uit staan (cross-device by construction, want
  in de gedeelde webview). Handmatige trigger werkt nu end-to-end tegen de
  spraak-stub.
- Nog te doen: de openWakeWord-`hey_jarvis`-detector via onnxruntime-web
  inpluggen (model-assets bundelen, audio-framing, CSP) — te valideren met echte
  audio op de machine van de gebruiker; dan is "Hey Jarvis" volledig hands-free
  op elk toestel. Drempel/false-accept tunen. Eventueel native `ort` voor
  achtergrond-luisteren.
