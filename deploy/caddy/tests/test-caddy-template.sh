#!/usr/bin/env bash
# Static safety checks for the Caddyfile shipped to the Home Node. Runtime
# syntax validation remains `caddy validate` on the target package version.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
template="$repo_dir/deploy/caddy/Caddyfile"

grep -Fqx $'\tauto_https disable_redirects' "$template"
grep -Fqx $'\t\t\tdisable_http_challenge' "$template"
grep -Fqx $'\t\treverse_proxy 127.0.0.1:8080' "$template"
# The Unix-socket upstream is an owner opt-in: shipped only as a comment.
grep -Fqx $'\t\t# reverse_proxy unix//run/jarvis-core/api.sock' "$template"
[[ $(grep -Ec '^\s*reverse_proxy ' "$template") == 1 ]]
grep -Fqx $'\t@downloads path /downloads /downloads/' "$template"
grep -Fqx $'\t\trewrite * /index.html' "$template"
grep -Fqx $'\t@altstoreSource path /downloads/altstore.json' "$template"
grep -Fqx $'\t\trewrite * /altstore.json' "$template"
[[ $(grep -Ec '^\s*rewrite ' "$template") == 2 ]]
! grep -Eq '^\s*(root|file_server).*app-updates|file_server browse' "$template"
grep -Fqx '{$JARVIS_PUBLIC_HOSTNAME} {' "$template"
! grep -Eq 'Access-Control-Allow-Origin|0\.0\.0\.0:8080' "$template"
