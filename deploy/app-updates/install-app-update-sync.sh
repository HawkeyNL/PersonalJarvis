#!/usr/bin/env bash
set -euo pipefail

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  echo "install-app-update-sync.sh must run as root" >&2
  exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(cd -- "$script_dir/../.." && pwd -P)

getent group jarvis >/dev/null || { echo "Install the Home Node jarvis service group first" >&2; exit 2; }

install -d -o root -g root -m 0755 /usr/lib/jarvis/app-updates/update-mirror
install -d -o root -g root -m 0755 /usr/lib/jarvis/app-updates/update-release
install -d -o root -g root -m 0750 /etc/jarvis/app-updates
install -d -o root -g jarvis -m 0750 /var/lib/jarvis/app-updates

install -o root -g root -m 0755 \
  "$repository_root/tools/app-updates/update-mirror/sync.py" \
  /usr/lib/jarvis/app-updates/update-mirror/sync.py
install -o root -g root -m 0644 \
  "$repository_root/tools/app-updates/update-release/manifest.py" \
  /usr/lib/jarvis/app-updates/update-release/manifest.py
install -o root -g root -m 0644 \
  "$repository_root/tools/app-updates/update-release/signatures.py" \
  /usr/lib/jarvis/app-updates/update-release/signatures.py
install -o root -g root -m 0644 \
  "$script_dir/jarvis-app-update-sync.service" \
  /etc/systemd/system/jarvis-app-update-sync.service
install -o root -g root -m 0644 \
  "$script_dir/jarvis-app-update-sync.timer" \
  /etc/systemd/system/jarvis-app-update-sync.timer

if [[ ! -e /etc/jarvis/app-updates/config.json.example ]]; then
  install -o root -g root -m 0640 \
    "$script_dir/config.example.json" \
    /etc/jarvis/app-updates/config.json.example
fi

systemctl daemon-reload
echo "Application update sync installed but not enabled."
echo "Review /etc/jarvis/app-updates/config.json.example and pin the desktop signing public key; sync once before enabling the timer."
