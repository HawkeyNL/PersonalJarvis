# Open beslissingen

- DEC-002 Eerste marktdata-provider
- DEC-003 NautilusTrader na PoC: kern, optioneel of afwijzen
- DEC-004 Prop firm: TopstepX, MFFU of voorlopig geen
- DEC-005 Eerst crypto of prediction markets
- DEC-006 Riskdefault: voorstel 0,5%, normale bovengrens 1%
- DEC-007 Bestaande pc, nieuwe GPU of vooral API's
- DEC-008 Eigen VPS of dedicated backend-VPS
- DEC-009 STT-provider (spraak-naar-tekst): cloud versus lokaal/on-device (TTS = Fish Audio)

## Gesloten

- DEC-001 Primaire cloudprovider → **Anthropic (Claude)**, 9 aug 2026. Sonnet als
  standaard-brein, Opus voor zwaar redeneren, Haiku snel/goedkoop; Ollama als
  lokale fallback. Provider-abstractie (`LlmProvider`) houdt providers
  wisselbaar. Zie [ADR-022](decisions/ADR-022-LLM-PROVIDER.md).
