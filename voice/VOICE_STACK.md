# Voice Stack

See [CONVERSATION_AND_OUTPUT_POLICY](CONVERSATION_AND_OUTPUT_POLICY.md) for the
chat surface and the rule that decides **when Jarvis may speak** (the audio-route
/ earbud policy), and [ADR-021](../decisions/ADR-021-VOICE-OUTPUT-POLICY.md).

## Primary (TTS)
- Fish Audio (voice synthesis and voice cloning where appropriate)

## Speech-to-text (STT)
- Provider still open (**DEC-009**): cloud vs local/on-device. The client uses the
  webview Speech API as a stopgap; the production path is native/streaming.

## Audio-route detection
- Native `audio_output_route` (Tauri): iOS `AVAudioSession`, macOS CoreAudio →
  `headset` | `speaker` | `unknown`. Drives the output policy above.

## Realtime reasoning
- Chosen LLM provider (OpenAI/Claude/DeepSeek/local)

## Local fallback
- Local TTS engine when offline

## Design goals
- Natural conversation
- Low latency
- Streaming audio
- Interruptions supported
- Provider abstraction (Fish Audio can be replaced later without changing Jarvis Core)
