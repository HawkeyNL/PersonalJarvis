#!/usr/bin/env bash
# Static safety checks for the Caddyfile shipped to the Home Node. Runtime
# syntax validation remains `caddy validate` on the target package version.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
template="$repo_dir/deploy/caddy/Caddyfile"

grep -Fqx $'\tauto_https disable_redirects' "$template"
# Admin API only on the Unix socket in the drop-in's runtime directory, never TCP.
grep -Fqx $'\tadmin unix//run/caddy/admin.sock' "$template"
[[ $(grep -Ec '^\s*admin ' "$template") == 1 ]]
dropin="$repo_dir/deploy/caddy/caddy.service.d-jarvis-admin.conf"
grep -Fqx 'RuntimeDirectory=caddy' "$dropin"
grep -Fqx 'RuntimeDirectoryMode=0700' "$dropin"
grep -Fqx $'\t\t\tdisable_http_challenge' "$template"
# The Core API is reachable only on its Unix socket, never over TCP.
grep -Fqx $'\t\treverse_proxy unix//run/jarvis-core/api.sock' "$template"
[[ $(grep -Ec '^\s*reverse_proxy ' "$template") == 1 ]]
if grep -Eq 'reverse_proxy .*:8080' "$template"; then
    echo "Caddy must not proxy to Core over TCP" >&2
    exit 1
fi
grep -Fqx $'\t@downloads path /downloads /downloads/' "$template"
grep -Fqx $'\t\trewrite * /index.html' "$template"
grep -Fqx $'\t@altstoreSource path /downloads/altstore.json' "$template"
grep -Fqx $'\t\trewrite * /altstore.json' "$template"
[[ $(grep -Ec '^\s*rewrite ' "$template") == 2 ]]
! grep -Eq '^\s*(root|file_server).*app-updates|file_server browse' "$template"
grep -Fqx '{$JARVIS_PUBLIC_HOSTNAME} {' "$template"
! grep -Eq 'Access-Control-Allow-Origin|0\.0\.0\.0:8080' "$template"
