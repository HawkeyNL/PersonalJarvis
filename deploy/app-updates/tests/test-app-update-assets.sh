#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
assets_dir=$(cd -- "$script_dir/.." && pwd -P)
trap 'echo "App-update asset assertion failed at line $LINENO" >&2' ERR

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
grep -Fqx 'User=root' "$assets_dir/jarvis-app-update-sync.service"
grep -Fqx 'Group=jarvis' "$assets_dir/jarvis-app-update-sync.service"
grep -Fq 'install -d -o root -g jarvis -m 0750 /var/lib/jarvis/app-updates' "$assets_dir/install-app-update-sync.sh"
python3 - "$assets_dir/config.example.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as source:
    config = json.load(source)
assert config['source'] == {'kind': 'github-releases', 'repository': 'HawkeyNL/PersonalJarvisApp'}, 'client source must be public and token-free'
assert config['tauri_signing_public_key'], 'client mirror requires a pinned Tauri key'
assert config['android_signing_certificate_sha256'], 'client mirror requires a pinned Android signer'
assert config['android_apksigner_path'] == '/usr/bin/apksigner', 'client mirror requires an explicit APK verifier'
PY

test ! -e "$assets_dir/../../jarvis-android"
test ! -e "$assets_dir/../../jarvis-ios"
test ! -e "$assets_dir/../../.github/workflows/mobile-release.yml"

if grep -ER 'TAURI_SIGNING_PRIVATE_KEY|ANDROID_RELEASE_KEYSTORE|APPLE_DISTRIBUTION_CERTIFICATE|APPLE_APP_STORE_PROVISIONING_PROFILE|APPLE_TEAM_ID|APP_STORE_CONNECT_API_' \
  "$assets_dir/../../.github/workflows"; then
  echo "Core workflows must not reference client production signing secrets" >&2
  exit 1
fi

if grep -ER 'HOME_NODE_(HOST|IP|DNS|SSH|USER|KEY)' "$assets_dir"; then
  echo "Home Node deployment coordinates must not enter app update assets" >&2
  exit 1
fi
