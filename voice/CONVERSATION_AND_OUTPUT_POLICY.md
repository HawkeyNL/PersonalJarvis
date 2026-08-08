# Gesprek & spraak-uitvoer-policy

Het gesprek met Jarvis staat links op de **SYSTEM → Jarvis**-view (de HUD).
Je praat of typt; Jarvis antwoordt altijd in tekst en — als het mag — ook
hardop. Deze doc beschrijft de in-/uitvoer-modaliteiten en, als kern, **wanneer
Jarvis hardop mag praten**.

## Modaliteiten

**Invoer**
- **Tekst** — altijd beschikbaar (toetsenbord).
- **Spraak (STT)** — microfoonknop; transcript verschijnt als jouw bericht.

**Uitvoer**
- **Tekst** — altijd getoond in de chat.
- **Audio (TTS)** — alleen als de *uitvoer-policy* het toestaat (zie onder).

De invoer-modaliteit bepaalt **niet** of Jarvis hardop praat. Ook als je terug
*typt*, mag Jarvis hardop antwoorden zolang de route dat toelaat. Andersom: praat
je hardop maar zit je zonder oortje op een luidspreker in het openbaar, dan blijft
Jarvis stil (tekst).

## Audio-route

De route is waar het geluid *uit* komt:

| Route | Voorbeeld | Klasse |
|-------|-----------|--------|
| `headset` | oortje, koptelefoon, bluetooth-headset (AirPods) | **privé** |
| `speaker` | ingebouwde luidspreker | **open** |
| `unknown` | route niet detecteerbaar | valt terug op handmatige toggle |

### Detectie
- **iOS** — `AVAudioSession.currentRoute.outputs`; `headphones` / `bluetoothA2DP`
  / `bluetoothHFP` ⇒ `headset`, `builtInSpeaker` ⇒ `speaker`.
- **macOS** — CoreAudio default-output + transport type; `headphones` / `bluetooth`
  / `usb`-headset ⇒ `headset`, `built-in` ⇒ `speaker`.
- Geleverd aan de webview via Tauri-command **`audio_output_route`** (retourneert
  `headset` | `speaker` | `unknown`) en herbekeken bij route-wijziging.
- **Fallback zolang native detectie er niet is**: een handmatige **"oortje in"**-
  toggle in de chat, die de route als `headset` markeert.

## De policy — mag Jarvis nu praten?

Beslissing `canSpeak()` → `{ allowed, reason }`:

1. **Master spraak uit?** → `allowed = false` (`"spraak uit"`). Jarvis blijft stil.
2. **Privé route** (`headset`, of `unknown` met "oortje in" aan)? →
   `allowed = true` (`"oortje verbonden"`). Jarvis praat, ook als je typt.
3. **Open route** (`speaker`):
   - gebruiker heeft **"luidspreker toestaan"** aan? → `allowed = true`.
   - anders → `allowed = false` (`"geen oortje — stil"`). Alleen tekst.

Jarvis kent zijn eigen toestand: de UI toont continu of hij mag praten en
waarom, en de policy wordt vóór elke TTS opnieuw geëvalueerd (route kan mid-
gesprek wijzigen — oortje eruit → volgende antwoord is stil).

### Later (uitbreidingen)
- **Do-not-disturb / stille uren** — forceert tekst-only in bepaalde vensters.
- **Gevoeligheid** — bedragen/orders niet hardop op een open route, ook niet met
  "luidspreker toestaan" (privacy in het openbaar).
- **Barge-in / interruptions** — praten onderbreekt lopende TTS (zie
  [VOICE_STACK](VOICE_STACK.md)).

## Gesprekstoestand (state machine)

```
idle → listening (mic aan) → thinking → replying (tekst [+ audio]) → idle
                     ↑______________ barge-in / stop ______________|
```

- `replying` spreekt alleen als `canSpeak().allowed`.
- Elke TTS is onderbreekbaar (`stop`), en een nieuwe invoer stopt de vorige.

## Alleen jouw stem (speaker verification)

Jarvis reageert alleen op **jouw** stem. Dit is stembiometrie bovenop STT:

- **Enrollment** — je neemt een korte set zinnen op; hieruit komt een
  *voiceprint* (speaker-embedding), device-privé opgeslagen (keychain / app-privé),
  net als de device-sleutel. Nooit ruwe audio bewaren, alleen de embedding.
- **Verificatie** — elke spraakinvoer krijgt een gelijkeniscore t.o.v. de
  voiceprint. Onder de drempel ⇒ **genegeerd** (of, in strikte modus, expliciet
  geweigerd: "stem niet herkend"). Boven de drempel ⇒ transcript wordt jouw
  bericht.
- **Wake word** (optioneel) — "Jarvis" activeert luisteren; alleen daarna wordt
  geverifieerd, zodat er geen always-on transcriptie is.
- **Anti-spoofing** — liveness/replay-detectie tegen opnames; drempel instelbaar
  (strenger in gevoelige context, bv. vóór een order-bevestiging).
- **Koppeling met identiteit** — de stem is een *tweede* factor naast de
  device-bound login; hij vervangt die niet. Mutaties/orders blijven een
  expliciete (biometrische) bevestiging vereisen.
- **Provider** — onderdeel van de STT/voice-keuze (DEC-009); kan on-device
  (privacy) of via een speaker-ID-model. Backlog: **JAR-151**
  (speaker-verification), na **JAR-150** (native audio-route-detectie).

## Privacy & veiligheid
- Microfoon vraagt expliciete toestemming; geen always-on opname zonder
  duidelijke indicator.
- Transcript blijft lokaal tot een bericht bewust verstuurd wordt.
- Geen gevoelige financiële data hardop op een open route.
- De reasoning/brain draait via de gekozen LLM-provider (DEC-001); TTS via de
  voice-stack (Fish Audio, met lokale fallback). Zie [VOICE_STACK](VOICE_STACK.md)
  en [ADR-021](../decisions/ADR-021-VOICE-OUTPUT-POLICY.md).
