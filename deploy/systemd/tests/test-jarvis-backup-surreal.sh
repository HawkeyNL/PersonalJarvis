#!/usr/bin/env bash
# Full `jarvis-backup create` against a real, networkless SurrealDB container
# whose root password is a canary: the export authenticates from the container
# environment, the disposable restore test imports it, and the canary never
# reaches output, the archive or leftover files.
set -Eeuo pipefail
shopt -s inherit_errexit
trap 'echo "test-jarvis-backup-surreal: failed at line $LINENO" >&2' ERR
[[ $EUID == 0 && ${GITHUB_ACTIONS:-} == true ]] || { echo 'CI root fixture only' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
name="jarvis-backup-live-${fixture##*.}"
real_docker=$(command -v docker)
canary="canary-root-password-${fixture##*.}"
cleanup() {
    "$real_docker" rm -f "$name" >/dev/null 2>&1 || true
    gpgconf --homedir "$fixture/keys" --kill all >/dev/null 2>&1 || true
    rm -rf -- "$fixture"
}
trap cleanup EXIT
"$real_docker" pull -q surrealdb/surrealdb:v2.6.5 >/dev/null
image=$("$real_docker" image inspect --format '{{index .RepoDigests 0}}' surrealdb/surrealdb:v2.6.5)
[[ $image =~ ^surrealdb/surrealdb@sha256:[0-9a-f]{64}$ ]]
"$real_docker" run -d --name "$name" --network none --user 0:0 \
    --cap-drop ALL --security-opt no-new-privileges:true \
    -e SURREAL_USER=root -e SURREAL_PASS="$canary" \
    "$image" start --bind 127.0.0.1:8000 memory >/dev/null
for attempt in $(seq 1 30); do
    "$real_docker" exec "$name" /surreal isready --endpoint http://127.0.0.1:8000 >/dev/null 2>&1 && break
    (( attempt < 30 )) || { echo 'fixture database did not become ready' >&2; exit 1; }
    sleep 1
done
"$real_docker" exec -i "$name" /surreal sql --hide-welcome --endpoint ws://127.0.0.1:8000 \
    --auth-level root --namespace jarvis --database core >/dev/null <<'SQL'
DEFINE TABLE device SCHEMAFULL;
DEFINE FIELD name ON device TYPE string;
CREATE device:one SET name = 'phone';
CREATE device:two SET name = 'laptop';
CREATE conversation:a SET text = 'hello';
CREATE conversation:b SET text = 'world';
CREATE conversation:c SET text = '!';
DEFINE TABLE empty_table;
DEFINE USER core ON DATABASE PASSWORD 'fixture-core-password' ROLES EDITOR;
SQL

# Production-shaped fixture tree.
install -d -m 0700 "$fixture/etc/jarvis-backup" "$fixture/run" "$fixture/dest" "$fixture/bin"
install -d -m 0750 "$fixture/etc/jarvis/secrets"
printf 'SURREALDB_IMAGE=%s\nSURREAL_ROOT_USER=root\nSURREAL_ROOT_PASSWORD=%s\n' "$image" "$canary" \
    > "$fixture/etc/jarvis/surrealdb.env"
printf 'namespace=jarvis\ndatabase=core\nusername=core\n' > "$fixture/etc/jarvis/surrealdb-core-provisioned"
printf 'PROVIDER_KEY=%s\n' "$canary" > "$fixture/etc/jarvis/secrets/provider.env"
chmod 0600 "$fixture/etc/jarvis/surrealdb.env" "$fixture/etc/jarvis/surrealdb-core-provisioned"
install -d -m 0700 "$fixture/keys"
gpg --batch --quiet --homedir "$fixture/keys" --passphrase '' \
    --quick-gen-key 'Backup owner <owner@example.invalid>' default default never 2>/dev/null
fpr=$(gpg --batch --homedir "$fixture/keys" --with-colons --list-keys 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --batch --homedir "$fixture/keys" --armor --export > "$fixture/etc/jarvis-backup/recipients.asc"
printf 'destination=%s\nrecipients=%s\n' "$fixture/dest" "$fpr" > "$fixture/etc/jarvis-backup/backup.conf"
chmod 0600 "$fixture/etc/jarvis-backup/backup.conf" "$fixture/etc/jarvis-backup/recipients.asc"

# `docker compose ... exec -T surrealdb ARGS` reaches the fixture container,
# exactly as production inherits root credentials from the container env.
cat > "$fixture/bin/docker" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == compose ]]; then
    # The production command line: fixed env file, fixed compose file.
    [[ $2 == --env-file && $3 == "$FIXTURE_ROOT/etc/jarvis/surrealdb.env" && \
       $4 == -f && $5 == "$FIXTURE_ROOT/docker-compose.yml" && \
       $6 == exec && $7 == -T && $8 == surrealdb ]] || { echo 'unexpected compose call' >&2; exit 1; }
    shift 8
    if [[ $2 == export && ${FIXTURE_TRUNCATE_EXPORT:-} == true ]]; then
        "$FIXTURE_DOCKER" exec -i "$FIXTURE_CONTAINER" "$@" | head -c 200
        exit 0
    fi
    exec "$FIXTURE_DOCKER" exec -i "$FIXTURE_CONTAINER" "$@"
fi
exec "$FIXTURE_DOCKER" "$@"
SH
chmod 0755 "$fixture/bin/docker"
export FIXTURE_DOCKER="$real_docker" FIXTURE_CONTAINER="$name" FIXTURE_ROOT="$fixture"
export PATH="$fixture/bin:$PATH" GITHUB_ACTIONS=true JARVIS_BACKUP_TEST_MODE=true JARVIS_BACKUP_FIXTURE_ROOT="$fixture"
helper="$repo/deploy/systemd/jarvis-backup.sh"

status=0
output=$(bash "$helper" create 2>&1) || status=$?
if grep -qF "$canary" <<< "$output"; then echo 'secret printed during backup' >&2; exit 1; fi
(( status == 0 )) || { printf 'create failed (%s):\n%s\n' "$status" "$output" >&2; exit 1; }
archive=$(find "$fixture/dest" -maxdepth 1 -name 'jarvis-backup-*.tar' -type f)
[[ -n $archive && $(stat -c '%u:%g:%a' "$archive") == 0:0:600 && $(stat -c '%a' "$archive.sha256") == 600 ]]
bash "$helper" verify "$archive" 2>/dev/null
if grep -aqF "$canary" "$archive"; then echo 'plaintext secret inside the archive' >&2; exit 1; fi
[[ -z $(find "$fixture/run" "$fixture/dest" -mindepth 1 -maxdepth 1 -name '*jarvis-backup.*' -type d) ]]
[[ -z $("$real_docker" ps -aq --filter 'name=jarvis-backup-verify-') ]]

# The owner key decrypts a manifest whose counts match the seeded database.
mkdir "$fixture/open"
tar -C "$fixture/open" -xf "$archive"
manifest=$(gpg --batch --quiet --homedir "$fixture/keys" -d "$fixture/open/manifest.json.gpg" 2>/dev/null)
jq -e '[.tables[] | {(.table): .restored}] | add == {"conversation": 3, "device": 2, "empty_table": 0}' <<< "$manifest" >/dev/null
export_hash=$(gpg --batch --quiet --homedir "$fixture/keys" -d "$fixture/open/surrealdb.surql.zst.gpg" 2>/dev/null \
    | zstd -dq | sha256sum | cut -d' ' -f1)
[[ $export_hash == "$(jq -r .export_sha256 <<< "$manifest")" ]]

# A concurrent run is refused without touching the archive.
before=$(sha256sum "$archive")
exec 5> "$fixture/run/jarvis-backup.lock"
flock 5
set +e; bash "$helper" create >/dev/null 2>&1; status=$?; set -e
[[ $status == 75 ]]
exec 5>&-

# An export that does not restore to the live counts is never published.
if FIXTURE_TRUNCATE_EXPORT=true bash "$helper" create >/dev/null 2>&1; then
    echo 'truncated export was published' >&2; exit 1
fi
[[ $(sha256sum "$archive") == "$before" ]]
[[ -z $(find "$fixture/run" "$fixture/dest" -mindepth 1 -maxdepth 1 -name '*jarvis-backup.*' -type d) ]]
[[ -z $("$real_docker" ps -aq --filter 'name=jarvis-backup-verify-') ]]

# A failing export leaves the previous archive intact and nothing behind.
"$real_docker" stop "$name" >/dev/null
if bash "$helper" create >/dev/null 2>&1; then echo 'backup succeeded without a database' >&2; exit 1; fi
[[ $(sha256sum "$archive") == "$before" ]]
[[ -z $(find "$fixture/run" "$fixture/dest" -mindepth 1 -maxdepth 1 -name '*jarvis-backup.*' -type d) ]]

# Non-root callers are refused before anything is read.
if setpriv --reuid=65534 --regid=65534 --clear-groups bash "$helper" create >/dev/null 2>&1; then
    echo 'non-root backup was allowed' >&2; exit 1
fi
echo 'jarvis-backup: real SurrealDB export, disposable restore test and encrypted archive hold'
