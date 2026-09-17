#!/usr/bin/env bash
# Actual RocksDB cold-copy recovery in an isolated disposable container.
set -euo pipefail
[[ $EUID == 0 && ${GITHUB_ACTIONS:-} == true ]] || { echo 'CI root fixture only' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
name="jarvis-cold-backup-${fixture##*.}"
real_docker=$(command -v docker)
cleanup() {
    "$real_docker" rm -f "$name" >/dev/null 2>&1 || true
    rm -rf -- "$fixture"
}
trap cleanup EXIT
mkdir -p "$fixture/bin" "$fixture/state/surrealdb"
chmod 0700 "$fixture/state" "$fixture/state/surrealdb"
"$real_docker" run -d --name "$name" --network none --user 0:0 \
    --cap-drop ALL --security-opt no-new-privileges:true \
    -e SURREAL_USER=root -e SURREAL_PASS=disposable-cold-fixture-password \
    -v "$fixture/state/surrealdb:/data" surrealdb/surrealdb:v2.6.5 \
    start --bind 127.0.0.1:8000 rocksdb:/data/jarvis.db >/dev/null
ready() {
    for attempt in $(seq 1 30); do
        if "$real_docker" exec "$name" /surreal isready --endpoint http://127.0.0.1:8000 >/dev/null 2>&1; then return; fi
        sleep 1
    done
    echo 'disposable database did not become ready' >&2; exit 1
}
sql() {
    "$real_docker" exec -i "$name" /surreal sql --hide-welcome --json \
        --endpoint ws://127.0.0.1:8000 --auth-level root --namespace fixture --database fixture
}
ready
printf "CREATE marker:original SET version = 6, content = 'canonical fixture';\n" | sql >/dev/null
cat > "$fixture/bin/systemctl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case $1 in
    stop) [[ $2 != jarvis-surrealdb.service ]] || "$FIXTURE_DOCKER" stop "$FIXTURE_CONTAINER" >/dev/null ;;
    is-active) exit 3 ;;
    *) exit 1 ;;
esac
SH
cat > "$fixture/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == compose ]] || exit 1
if [[ $("$FIXTURE_DOCKER" inspect --format '{{.State.Running}}' "$FIXTURE_CONTAINER") == true ]]; then echo running; fi
SH
chmod 0755 "$fixture/bin/"*
export FIXTURE_DOCKER="$real_docker" FIXTURE_CONTAINER="$name"
export PATH="$fixture/bin:$PATH" JARVIS_SCHEMA_TEST_MODE=true JARVIS_SCHEMA_FIXTURE_ROOT="$fixture/state"
helper="$repo/deploy/systemd/schema-backup.sh"
id=$(bash "$helper" create v1.0.0 v1.0.1)
"$real_docker" start "$name" >/dev/null
ready
printf "UPDATE marker:original SET version = 8; CREATE marker:new SET content = 'candidate-only';\n" | sql >/dev/null
bash "$helper" restore "$id"
"$real_docker" start "$name" >/dev/null
ready
result=$(printf 'SELECT * FROM marker;\n' | sql)
jq -e '.[0].status == "OK" and (.[0].result | length) == 1 and .[0].result[0].version == 6 and .[0].result[0].content == "canonical fixture"' <<< "$result" >/dev/null
echo 'Real SurrealDB RocksDB cold snapshot restores previous data and removes candidate-only state'
