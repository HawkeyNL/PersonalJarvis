# ADR-021 — Spraak-uitvoer-policy (wanneer Jarvis hardop praat)

- Status: geaccepteerd (scaffold); native route-detectie nog te bouwen
- Context: DEC-001 (LLM-provider) open; voice-stack = Fish Audio (VOICE_STACK)

## Context

Jarvis heeft een gespreksoppervlak op de SYSTEM → Jarvis-view. Invoer kan tekst
of spraak zijn; uitvoer is altijd tekst en soms audio. We moeten deterministisch
bepalen **wanneer** Jarvis hardop mag praten, zodat hij niet in het openbaar via
de luidspreker begint te praten, maar wél door je oortje reageert — ook als je
terugtypt.

## Beslissing

De **audio-route** bepaalt de spraak-uitvoer, niet de invoer-modaliteit.

- **Privé route** (oortje/koptelefoon/bluetooth-headset) ⇒ Jarvis mag praten,
  ongeacht of je typte of sprak.
- **Open route** (ingebouwde luidspreker) ⇒ standaard **tekst-only**; alleen
  hardop als de gebruiker expliciet "luidspreker toestaan" aanzet.
- Een **master voice-toggle** kan alle audio uitzetten.
- De policy (`canSpeak()`) wordt **vóór elke uitspraak** geëvalueerd, want de
  route kan mid-gesprek wijzigen.

Route-detectie is native (Tauri-command `audio_output_route`, iOS
`AVAudioSession` / macOS CoreAudio). Zolang die er niet is, geldt een handmatige
**"oortje in"**-toggle als route-signaal (`unknown` → privé wanneer aan).

Beslisvolgorde: master-uit → stil · privé route → praten · open route + toestaan
→ praten · anders → stil. Zie [CONVERSATION_AND_OUTPUT_POLICY](../docs/blueprint/voice/CONVERSATION_AND_OUTPUT_POLICY.md).

## Alternatieven

- **Invoer-modaliteit bepaalt uitvoer** (praat je, dan praat Jarvis terug):
  verworpen — dan blijft Jarvis stil als je typt met je oortje in, precies het
  scenario dat we willen.
- **Altijd hardop tenzij gedempt**: verworpen — riskant in het openbaar.
- **Altijd tekst, audio als losse "speak"-knop**: blijft als handmatige optie
  bestaan, maar is niet het standaardgedrag.

## Gevolgen

- Voorspelbaar, privacy-vriendelijk gedrag; Jarvis "weet" wanneer hij mag praten
  en toont dat in de UI.
- Vereist native route-detectie voor de volledige ervaring (backlog: JAR-150).
- Nieuwe open beslissing: **DEC-009 STT-provider** (cloud vs lokaal/on-device),
  naast TTS = Fish Audio.
