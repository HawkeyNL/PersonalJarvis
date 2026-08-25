#!/usr/bin/env bash
# Fetch a Whisper GGML model for server-side STT (ADR-025 stage 1).
#
# Usage:  bash scripts/fetch-whisper-model.sh [size]
#   size = base | small | medium | large-v3-turbo   (default: base)
# Use the MULTILINGUAL models (no ".en" suffix) — you speak Dutch.
# Then set in .env:  JARVIS_SPEECH_PROVIDER=whisper
#                    JARVIS_SPEECH_WHISPER_MODEL=models/ggml-<size>.bin
#                    JARVIS_SPEECH_WHISPER_LANGUAGE=nl
# And run the backend with:  cargo run -p jarvis-api --features speech-whisper
# (real STT needs cmake: `brew install cmake`).
set -euo pipefail

cd "$(dirname "$0")/.."
SIZE="${1:-base}"
DEST="models"
FILE="ggml-${SIZE}.bin"
URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/${FILE}"

mkdir -p "$DEST"
if [ -s "$DEST/$FILE" ]; then
  echo "✓ $DEST/$FILE (already present)"
else
  echo "↓ $FILE"
  curl -fSL --retry 3 -o "$DEST/$FILE" "$URL"
fi
echo "→ set JARVIS_SPEECH_WHISPER_MODEL=$DEST/$FILE (and JARVIS_SPEECH_PROVIDER=whisper)"
