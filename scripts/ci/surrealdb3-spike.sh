#!/usr/bin/env bash
# PR0 spike for docs/SURREALDB_3_MIGRATION.md: probe SurrealDB 3.3 against the
# real schema chain and the runtime statement shapes, and test the 2.6.5 ->
# 3.3 export/import path. CI only, throwaway: it records findings and never
# fails on a single probe. Delete it once the findings are in the plan.
set -uo pipefail
[[ ${GITHUB_ACTIONS:-} == true ]] || { echo 'CI only' >&2; exit 1; }

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
V2=surrealdb/surrealdb:v2.6.5
V3=surrealdb/surrealdb:v3.3.0
work=$(mktemp -d)
out=${GITHUB_STEP_SUMMARY:-/dev/stdout}
cleanup() { docker rm -f s2 s2u s3 s3u >/dev/null 2>&1; rm -rf -- "$work"; }
trap cleanup EXIT

note() { printf '%s\n' "$*" | tee -a "$out"; }
block() { { printf '\n<details><summary>%s</summary>\n\n```\n' "$1"; head -c 6000; printf '\n```\n</details>\n\n'; } | tee -a "$out"; }

# POST SurrealQL to /sql. Args: port ns db [user pass]. Body on stdin.
sql() {
    local port=$1 ns=$2 db=$3 user=${4:-root} pass=${5:-root} auth=()
    [[ $user != none ]] && auth=(-u "$user:$pass")
    curl -sS "${auth[@]}" -H 'Accept: application/json' \
        -H "surreal-ns: $ns" -H "surreal-db: $db" \
        -H "surreal-auth-ns: $ns" -H "surreal-auth-db: $db" \
        --data-binary @- "http://127.0.0.1:$port/sql"
}
sql_root() { curl -sS -u root:root -H 'Accept: application/json' -H "surreal-ns: $2" -H "surreal-db: $3" --data-binary @- "http://127.0.0.1:$1/sql"; }
statuses() { local r; r=$(cat); jq -c '[.[]? | .status]' <<<"$r" 2>/dev/null || printf 'not JSON: %s' "${r:0:300}"; }
define_db() { printf 'DEFINE NAMESPACE IF NOT EXISTS %s; USE NS %s; DEFINE DATABASE IF NOT EXISTS %s;' "$2" "$2" "$3" | sql "$1" "$2" "$3" "${4:-root}" | statuses; }

wait_ready() {
    for _ in $(seq 1 30); do
        docker exec "$1" /surreal isready --endpoint http://127.0.0.1:8000 >/dev/null 2>&1 && return 0
        sleep 1
    done
    return 1
}

docker pull -q "$V2" >/dev/null && docker pull -q "$V3" >/dev/null
note "# SurrealDB 3.3 spike findings"
note "- 2.6.5: \`$(docker image inspect --format '{{index .RepoDigests 0}}' "$V2")\`"
note "- 3.3.0: \`$(docker image inspect --format '{{index .RepoDigests 0}}' "$V3")\`"

## T8: start flags, root from environment, isready, /health
docker run -d --name s3 -p 127.0.0.1:8003:8000 -e SURREAL_USER=root -e SURREAL_PASS=root \
    "$V3" start --bind 0.0.0.0:8000 memory >/dev/null
if wait_ready s3; then note "- T8 isready (3.3, root from SURREAL_USER/SURREAL_PASS): ready"; else note "- T8 isready: NOT READY"; docker logs s3 2>&1 | block 's3 logs'; fi
note "- T8 /health: HTTP $(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8003/health)"
note "- T8 root login from env: $(echo 'RETURN 1;' | sql_root 8003 t t | statuses)"
docker run -d --name s3u -p 127.0.0.1:8004:8000 "$V3" start --unauthenticated --bind 0.0.0.0:8000 memory >/dev/null
if wait_ready s3u; then note "- T8 --unauthenticated start: ready"; else note "- T8 --unauthenticated start: NOT READY"; docker logs s3u 2>&1 | block 's3u logs'; fi
for help in 'start --help' 'export --help' 'import --help' 'v2 --help' 'v2 export --help'; do
    # shellcheck disable=SC2086
    docker run --rm "$V3" $help 2>&1 | block "3.3 \`surreal $help\`"
done

## Schema chain as root (B3, B15: DEFINE inside BEGIN/COMMIT, implicit ns/db)
apply_chain() { # port ns db user pass patched
    local f body
    for f in "$repo"/schema/surreal/*.surql; do
        body=$(cat "$f")
        [[ $6 == patched ]] && body=${body//FLEXIBLE TYPE option<object>/TYPE option<object> FLEXIBLE}
        note "  - $(basename "$f"): $(printf '%s' "$body" | sql "$1" "$2" "$3" "$4" "$5" | statuses)"
    done
}
note ""; note "- B15 implicit namespace/database on 3.3: $(sql 8003 implicit core <"$repo/schema/surreal/0001_baseline.surql" | statuses)"
note "- define chain_raw: $(define_db 8003 chain_raw core)"; note "- define chain: $(define_db 8003 chain core)"
note ""; note "## Schema chain on 3.3 as root, unmodified"
apply_chain 8003 chain_raw core root root raw
note ""; note "## Schema chain on 3.3 as root, B3 fix applied"
apply_chain 8003 chain core root root patched
printf '%s' "$(sed 's/FLEXIBLE TYPE option<object>/TYPE option<object> FLEXIBLE/' "$repo/schema/surreal/0010_coding_reservations.surql")" \
    | sql 8003 chain_raw core | jq -c '[.[]? | select(.status != "OK")]' | block 'B3: errors of unmodified 0010 vs patched'

## T7: schema chain as a database EDITOR
echo "DEFINE NAMESPACE ed; USE NS ed; DEFINE DATABASE core; USE DB core; DEFINE USER core ON DATABASE PASSWORD 'core-pw' ROLES EDITOR;" \
    | sql_root 8003 ed core | statuses | sed 's/^/- T7 define EDITOR user: /' | tee -a "$out"
note ""; note "## T7: schema chain on 3.3 as database EDITOR"
apply_chain 8003 ed core core core-pw patched
note "- T7 INFO FOR DB as EDITOR: $(echo 'INFO FOR DB;' | sql 8003 ed core core core-pw | statuses)"

## T3: INFO FOR DB shape, `sql --json`, export --auth-level root
echo 'INFO FOR DB;' | sql_root 8003 chain core | jq '.[0].result | if type == "object" then with_entries(.value |= (if type == "object" then (keys | .[0:4]) else . end)) else . end' | block 'T3 INFO FOR DB (top-level keys, sample)'
echo 'SELECT version FROM schema_version;' | docker exec -i s3 /surreal sql --hide-welcome --json \
    --endpoint ws://127.0.0.1:8000 --auth-level root --user root --pass root --namespace chain --database core 2>&1 | block 'T3 surreal sql --json'
docker exec s3 /surreal export --endpoint http://127.0.0.1:8000 --auth-level root --user root --pass root \
    --namespace chain --database core - 2>&1 | head -c 1500 | block 'T3 export --auth-level root (head)'

docker run -d --name s2 -p 127.0.0.1:8002:8000 -e SURREAL_USER=root -e SURREAL_PASS=root \
    "$V2" start --bind 0.0.0.0:8000 memory >/dev/null
wait_ready s2 || note "- 2.6.5 NOT READY"
note "- 2.6.5 define chain: $(define_db 8002 chain core)"
note "- 2.6.5 chain: $(for f in "$repo"/schema/surreal/*.surql; do sql 8002 chain core <"$f" | statuses; done | tr '\n' ' ' | head -c 400)"
## Runtime statement shapes on the migrated 3.3 database
probe() { local r; r=$(sql_root "$port" chain core); note "- $1: \`$(jq -c '[.[]? | {status, result: (.result | tostring | .[0:200])}]' <<<"$r" 2>/dev/null || echo "${r:0:300}")\`"; }
for port in 8002 8003; do
note ""; note "## Runtime statement shapes (port $port: 8002 = 2.6.5, 8003 = 3.3)"
echo 'SELECT * FROM table_that_does_not_exist;' | probe 'B4 select unknown table'
cat <<'SQL' | probe 'B11 UPSERT table without id (twice, then count)'
LET $user_id = 'u1'; LET $embedding = 'AAEC'; LET $dims = 3; LET $engine = 'stub';
UPSERT voice_profiles SET user_id = $user_id, embedding = <bytes>$embedding, dims = $dims, engine = $engine, created_at = time::now(), updated_at = time::now() RETURN NONE;
UPSERT voice_profiles SET user_id = $user_id, embedding = <bytes>$embedding, dims = $dims, engine = $engine, created_at = time::now(), updated_at = time::now() RETURN NONE;
SELECT count() FROM voice_profiles GROUP ALL;
SQL
cat <<'SQL' | probe 'B12 DELETE ... RETURN $id AS id in a transaction'
LET $id = 'c1'; LET $user_id = 'u1';
BEGIN TRANSACTION; DELETE chat_messages WHERE conversation_id = $id AND user_id = $user_id;
DELETE conversations WHERE record::id(id) = $id AND user_id = $user_id RETURN $id AS id; COMMIT TRANSACTION;
SQL
cat <<'SQL' | probe 'B16 percentile/time::max on empty llm_usage'
SELECT math::percentile(latency_ms, 50) AS p50, math::percentile(latency_ms, 95) AS p95 FROM llm_usage WHERE latency_ms > 0 GROUP ALL;
SELECT agent_id, time::max(ts) AS last_used FROM llm_usage WHERE agent_id != NONE GROUP BY agent_id;
SQL
cat <<'SQL' | probe 'B7 NULL vs NONE into option<string> (account_actions.verifier)'
CREATE account_actions:n1 SET id = 'n1', user_id = 'u', device_id = 'd', action = 'device-revoke', target = 't', nonce = <bytes>'AA==', verifier = NULL, created_at = time::now(), expires_at = time::now();
CREATE account_actions:n2 SET id = 'n2', user_id = 'u', device_id = 'd', action = 'device-revoke', target = 't', nonce = <bytes>'AA==', verifier = NONE, created_at = time::now(), expires_at = time::now();
SELECT record::id(id) AS id, verifier, type::is::none(verifier) AS is_none, type::is::null(verifier) AS is_null FROM account_actions ORDER BY id;
SQL
done

## 2.6.5 -> 3.3 export/import (unauthenticated source, passhash, typed values)
note ""; note "## Export 2.6.5 with the 3.3 CLI, import into 3.3"
docker run -d --name s2u -p 127.0.0.1:8005:8000 "$V2" start --unauthenticated --bind 0.0.0.0:8000 memory >/dev/null
wait_ready s2u || note "- 2.6.5 unauthenticated NOT READY"
note "- define data on 2.6.5: $(define_db 8005 data core none)"
for f in "$repo"/schema/surreal/*.surql; do sql 8005 data core none <"$f" >/dev/null; done
cat <<'SQL' | sql 8005 data core none | statuses | sed 's/^/- seed 2.6.5: /' | tee -a "$out"
DEFINE USER core ON DATABASE PASSWORD 'core-pw' ROLES EDITOR;
DEFINE TABLE samples SCHEMALESS;
CREATE samples:typed SET b = <bytes>'AAECAw==', dt = d'2026-10-05T12:34:56.789Z', dur = 90s, obj = { nested: { list: [1, 'two', NONE] } }, n = NULL, f = 1.5, i = 42, s = 'tekst é';
CREATE samples:link SET other = samples:typed;
SQL
before=$(echo 'SELECT * FROM samples ORDER BY id; SELECT count() FROM schema_version GROUP ALL;' | sql 8005 data core none | jq -c '[.[].result]')
docker run --rm --network container:s2u -v "$work:/out" "$V3" v2 export --v3 \
    --endpoint http://127.0.0.1:8000 --namespace data --database core /out/export.surql >"$work/export.log" 2>&1
note "- v2 export --v3 (unauthenticated source) exit: $?"
block 'export log' <"$work/export.log"
[[ -s $work/export.surql ]] && grep -n 'DEFINE USER\|samples:typed\|FLEXIBLE' "$work/export.surql" | head -20 | block 'export excerpt'
note "- define data on 3.3: $(define_db 8003 data core)"
docker cp "$work/export.surql" s3:/tmp/export.surql 2>/dev/null
docker exec s3 /surreal import --endpoint http://127.0.0.1:8000 --user root --pass root \
    --namespace data --database core /tmp/export.surql >"$work/import.log" 2>&1
note "- import into 3.3 exit: $?"
block 'import log' <"$work/import.log"
after=$(echo 'SELECT * FROM samples ORDER BY id; SELECT count() FROM schema_version GROUP ALL;' | sql_root 8003 data core | jq -c '[.[].result]')
if [[ $before == "$after" ]]; then note "- data equal after migration: yes"; else note "- data equal after migration: NO"; printf 'before: %s\nafter:  %s\n' "$before" "$after" | block 'data diff'; fi
note "- passhash survives (EDITOR signin on 3.3): $(echo 'INFO FOR DB;' | sql 8003 data core core core-pw | statuses)"
note "- wrong password rejected: $(echo 'RETURN 1;' | sql 8003 data core core wrong | head -c 200)"
exit 0
