#!/usr/bin/env bash
# Static safety checks for the Caddyfile shipped to the Home Node. Runtime
# syntax validation remains `caddy validate` on the target package version.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
template="$repo_dir/deploy/caddy/Caddyfile"

grep -Fqx $'\tauto_https disable_redirects' "$template"
grep -Fqx $'\t\t\tdisable_http_challenge' "$template"
grep -Fqx $'\treverse_proxy 127.0.0.1:8080' "$template"
grep -Fqx '{$JARVIS_PUBLIC_HOSTNAME} {' "$template"
! rg -q 'Access-Control-Allow-Origin|0\.0\.0\.0:8080' "$template"
