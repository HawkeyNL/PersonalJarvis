#!/usr/bin/env bash
# Trusted owner-side hidden input only. Never run via an agent with a token.
set -euo pipefail
set +a
unset token
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
[[ $EUID -eq 0 ]] || { echo 'Run through the trusted root administration path.' >&2; exit 2; }
readonly token_dir=/etc/jarvis/app-downloads
readonly token_path=/etc/jarvis/app-downloads/ghcr.token
for directory in /etc /etc/jarvis "$token_dir"; do
  [[ -d $directory && ! -L $directory && $(stat -c %u -- "$directory") == 0 ]] || {
    echo 'Missing or unsafe managed configuration directory.' >&2; exit 2;
  }
  mode=$(stat -c %a -- "$directory")
  (( (8#$mode & 0022) == 0 )) || { echo 'Writable managed configuration directory refused.' >&2; exit 2; }
done
if [[ -e $token_path || -L $token_path ]]; then
  [[ -f $token_path && ! -L $token_path && $(stat -c %u -- "$token_path") == 0 && $(stat -c %h -- "$token_path") == 1 ]] || {
    echo 'Unsafe existing token file.' >&2; exit 2;
  }
fi
umask 077
token_tmp=''
cleanup() {
  unset token
  if [[ -n $token_tmp ]]; then rm -f -- "$token_tmp"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP
# Bash read -s restores its terminal settings when interrupted. Input is bounded;
# secrets never enter argv, external printf, environment, or shell history.
IFS= read -r -s -n 1025 -p 'GHCR read:packages token (hidden): ' token </dev/tty || {
  printf '\nToken entry cancelled.\n' >/dev/tty; exit 2;
}
export -n token
printf '\n' >/dev/tty
[[ -n $token && ${#token} -le 1024 && $token != *[[:space:][:cntrl:]]* ]] || {
  echo 'Invalid token input; existing token unchanged.' >&2; exit 2;
}
token_tmp=$(mktemp "$token_dir/.ghcr-token.XXXXXXXX")
printf '%s' "$token" > "$token_tmp"
unset token
chmod 0600 "$token_tmp"
chown root:root "$token_tmp"
mv -T -- "$token_tmp" "$token_path"
token_tmp=''
echo 'GHCR credential stored privately. No network request made.'
