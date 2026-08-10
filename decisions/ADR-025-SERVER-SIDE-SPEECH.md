# ADR-025 — Server-side spraak: STT + speaker-verificatie met centraal profiel

- Status: geaccepteerd (scaffold) — 9 augustus 2026; echt model nog te bouwen
- Context: vervangt de Picovoice-route (ADR-024) — Picovoice-signup weigert
  persoonlijke e-maildomeinen. Bouwt op de device-auth en ADR-023 (app-lock)

## Context

De gebruiker wil "Hey Jarvis" + herkenning van **alleen zijn stem**, maar zonder
account (Picovoice viel af). Gevraagde richting: doe het zware werk **op de
eigen server** en houd het **stemprofiel centraal**, gesynct naar de devices.

## Beslissing

**Speaker-verificatie en STT draaien server-side, achter een provider-abstractie**
— exact het patroon van het brein-crate (`jarvis-llm`):

- Nieuw crate `jarvis-speech` met een `SpeechEngine`-trait (`transcribe` +
  `embed`) en een deterministische **`StubEngine`** (energie-fingerprint als
  embedding, lege transcriptie). Zo staat de hele keten getest vóór een echt
  model. Cosine-similariteit + een drempel (`JARVIS_SPEECH_VERIFY_THRESHOLD`,
  default 0.5) bepalen "is dit jij".
- Het **stemprofiel is één embedding per gebruiker**, server-side opgeslagen
  (`voice_profiles`, migratie 0006, f32-bytes). Omdat het centraal staat is het
  **inherent gesynct**: elk ingelogd device gebruikt hetzelfde profiel — geen
  per-device kopie of sync-protocol nodig. De server is de bron van waarheid.
- Beveiligde endpoints: `GET /v1/voice/status`, `POST /v1/voice/enroll`
  (embed + opslaan), `POST /v1/voice/verify` (embed + transcribe + cosine).
  Audio gaat als 16-bit mono PCM in de request; het is jóuw server.

**Wake-word blijft lokaal.** "Hey Jarvis" continu naar de server streamen is
geen optie (privacy + 24/7 bandbreedte); een lichte lokale detector triggert, en
het korte fragment gaat daarna naar de server voor verify + STT. (Lokale
wake-word is een latere stap.)

**Security-posture (ongewijzigd t.o.v. ADR-024):** stem is *gemak*, geen slot.
De biometrie/telefoon-goedkeuring blijft de echte grens; stem-verify opent
hoogstens de console of start de biometrie.

## Alternatieven

- **Lokaal (in de webview)**: privé/offline, maar account-loze speaker-
  verificatie in de browser is onbetrouwbaar/zwaar — daarom server-side.
- **Picovoice**: afgewezen — signup weigert persoonlijke e-mail.
- **Per-device profiel + sync-protocol**: onnodig; centraal opslaan is
  eenvoudiger en automatisch consistent.

## Gevolgen

- Geteste, werkende keten (enroll → verify) met een stub; het echte model
  (Whisper voor STT, een speaker-embedding-net) is een afgebakende vervolgstap
  achter dezelfde trait — te valideren op de machine van de gebruiker met echte
  audio.
- **Client-slice klaar** (`voiceCapture.ts` + `voiceServer.ts`): WebAudio-opname
  → resample naar 16 kHz mono i16 → upload naar `/enroll` en `/verify`. Settings
  heeft een STEM-panel (inschrijven + "test verificatie" met transcript + score).
  Werkt mechanisch tegen de stub end-to-end.
- **Echt STT — stage 1 (whisper.cpp)**: `WhisperEngine` achter de trait via
  `whisper-rs`, feature-gated (`--features speech-whisper`, vereist cmake) zodat
  de default-build/CI de stub houden. Provider `whisper` + `JARVIS_SPEECH_WHISPER_*`;
  model via `scripts/fetch-whisper-model.sh`. `transcribe` draait op een
  blocking-thread; misconfig degradeert netjes naar de stub. `embed` is nog de
  placeholder-energie-fingerprint.
- **Echt speaker-embedding — stage 2 (MFCC, pure Rust)**: `embed` gebruikt nu een
  echte stem-timbre-embedding (`speaker.rs`: per-frame MFCC's → mean+std, c0
  weggelaten, L2-genormaliseerd), gedeeld door de default- en whisper-engine.
  Geen model/download/native-dep — puur Rust met `rustfft`, hier compileerbaar +
  unit-getest (deterministisch én onderscheidt verschillende stemprofielen). De
  default-engine heet nu `baseline` (echte speaker-verify, nog geen STT). Client:
  **microfoon-keuze** in Settings (`micDevices.ts`).
- Nog te doen: eventueel een **neuraal speaker-model** (ECAPA/wespeaker-ONNX via
  `ort`) als accuraatheids-upgrade achter dezelfde `embed`; console-STT (mic →
  server-transcript); drempel tunen; profiel eventueel versleuteld opslaan.
