#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
assets_dir=$(cd -- "$script_dir/.." && pwd -P)

for asset in \
  config.example.json \
  install-app-update-sync.sh \
  jarvis-app-update-sync.service \
  jarvis-app-update-sync.timer; do
  test -s "$assets_dir/$asset"
done

grep -Fq 'ProtectSystem=strict' "$assets_dir/jarvis-app-update-sync.service"
grep -Fq 'ReadWritePaths=/var/lib/jarvis/app-updates' "$assets_dir/jarvis-app-update-sync.service"
grep -Fq 'retention_previous' "$assets_dir/config.example.json"
grep -Fq 'android_signing_certificate_sha256' "$assets_dir/config.example.json"
grep -Fq '"android_apksigner_path": "/usr/bin/apksigner"' "$assets_dir/config.example.json"

if grep -ER 'HOME_NODE_(HOST|IP|DNS|SSH|USER|KEY)' "$assets_dir"; then
  echo "Home Node deployment coordinates must not enter app update assets" >&2
  exit 1
fi
