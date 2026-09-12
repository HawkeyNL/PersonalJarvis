#!/usr/bin/env bash
# Invoke only after reviewed, device-approved owner deployment. Builds run rootless.
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
[[ $EUID -eq 0 && $# -eq 1 ]] || {
  echo 'Usage through trusted owner administration: install-app-downloads.sh /absolute/reviewed/binary' >&2; exit 2;
}
[[ $1 == /* && -f $1 && ! -L $1 && -x $1 ]] || { echo 'A reviewed regular executable is required.' >&2; exit 2; }
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
for directory in /usr /usr/local /etc /etc/jarvis /etc/systemd /etc/systemd/system /var /var/lib; do
  [[ -d $directory && ! -L $directory && $(stat -c %u -- "$directory") == 0 ]] || { echo 'Unsafe or missing managed parent directory.' >&2; exit 2; }
  mode=$(stat -c %a -- "$directory")
  (( (8#$mode & 0022) == 0 )) || { echo 'Writable managed parent refused.' >&2; exit 2; }
done
for directory in /usr/local/libexec /etc/jarvis/app-downloads /var/lib/jarvis-public-downloads /var/lib/jarvis-public-downloads/ios /var/lib/jarvis-app-updates; do
  if [[ -e $directory || -L $directory ]]; then
    [[ -d $directory && ! -L $directory && $(stat -c %u -- "$directory") == 0 ]] || { echo 'Unsafe destination directory.' >&2; exit 2; }
    mode=$(stat -c %a -- "$directory")
    (( (8#$mode & 0022) == 0 )) || { echo 'Writable destination refused.' >&2; exit 2; }
  fi
done
for target in /usr/local/libexec/jarvis-app-downloads /usr/local/libexec/jarvis-app-downloads-set-token /etc/systemd/system/jarvis-app-downloads.service /etc/systemd/system/jarvis-app-downloads.timer /etc/systemd/system/jarvis-app-release-sync.service /etc/systemd/system/jarvis-app-release-sync.timer; do
  if [[ -e $target || -L $target ]]; then
    [[ -f $target && ! -L $target && $(stat -c %u -- "$target") == 0 && $(stat -c %h -- "$target") == 1 ]] || { echo 'Unsafe installed target.' >&2; exit 2; }
  fi
done
install -d -o root -g root -m 0755 /usr/local/libexec /var/lib/jarvis-public-downloads /var/lib/jarvis-public-downloads/ios
install -d -o root -g root -m 0700 /etc/jarvis/app-downloads
if [[ ! -d /var/lib/jarvis-app-updates ]]; then
  install -d -o root -g jarvis -m 0750 /var/lib/jarvis-app-updates
fi
install -o root -g root -m 0755 -- "$1" /usr/local/libexec/jarvis-app-downloads
install -o root -g root -m 0755 "$script_dir/set-download-token.sh" /usr/local/libexec/jarvis-app-downloads-set-token
install -o root -g root -m 0644 "$script_dir/jarvis-app-downloads.service" /etc/systemd/system/jarvis-app-downloads.service
install -o root -g root -m 0644 "$script_dir/jarvis-app-downloads.timer" /etc/systemd/system/jarvis-app-downloads.timer
install -o root -g root -m 0644 "$script_dir/jarvis-app-release-sync.service" /etc/systemd/system/jarvis-app-release-sync.service
install -o root -g root -m 0644 "$script_dir/jarvis-app-release-sync.timer" /etc/systemd/system/jarvis-app-release-sync.timer
# Deliberately no config/token overwrite, download, daemon reload or service start.
echo 'Download importer installed but inactive. Configure a reviewed digest and token before approved activation.'
