#!/usr/bin/env bash
# One-time setup for the "Hey Jarvis" wake word (ADR-026).
#
# Downloads the openWakeWord ONNX models into public/models, where the webview
# detector loads them from (/models). The ORT WASM runtime is bundled by Vite,
# so nothing else to copy. Run once from the client dir:
#   bash scripts/setup-wakeword.sh   (or: npm run setup-wakeword)
set -euo pipefail

cd "$(dirname "$0")/.."
MODELS="public/models"
REL="https://github.com/dscripka/openWakeWord/releases/download/v0.5.1"

echo "→ openWakeWord models into $MODELS"
mkdir -p "$MODELS"
for f in melspectrogram.onnx embedding_model.onnx hey_jarvis_v0.1.onnx; do
  if [ -s "$MODELS/$f" ]; then
    echo "  ✓ $f (already present)"
  else
    echo "  ↓ $f"
    curl -fSL --retry 3 -o "$MODELS/$f" "$REL/$f"
  fi
done

echo "✓ done. Enable “Luister naar ‘Hey Jarvis’” in Settings, then say it."
