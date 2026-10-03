#!/usr/bin/env bash
# Owner-run disaster-recovery backup for the Home Node. One run writes one
# dated tar of gpg-encrypted members plus a sha256 file:
#   surrealdb.surql.zst.gpg  logical export of the Core namespace/database
#   etc-jarvis.tar.zst.gpg   protected configuration including secrets
#   manifest.json.gpg        versions, table counts and member hashes
# Members are encrypted to owner public keys only: this host cannot decrypt
# its own backups. Before encryption the export is imported into a disposable,
# networkless SurrealDB container and its table counts are compared with the
# live database. Nothing is uploaded; the owner copies archives off-machine.
#
# The only plaintext file is the export, on tmpfs below /run, removed on exit.
# No secret is read into this process or placed on a command line: SurrealDB
# root credentials stay in the production container environment.
set -euo pipefail
shopt -s inherit_errexit
umask 077

readonly archive_pattern='jarvis-backup-[0-9]{4}-[0-9]{2}-[0-9]{2}\.tar'
readonly members=(etc-jarvis.tar.zst.gpg manifest.json.gpg surrealdb.surql.zst.gpg)
readonly keep=7
# The disposable restore container has 1 GiB; a larger export cannot be tested.
readonly max_export_kib=$((1024 * 1024))

fail() { echo "jarvis backup: $*" >&2; exit 1; }
log() { echo "jarvis backup: $*" >&2; }

safe_dir() {
    [[ -d $1 && ! -L $1 ]] || fail 'required directory is missing or symlinked'
    local metadata
    metadata=$(stat -c '%u:%g:%a' "$1")
    if [[ $metadata != 0:0:* ]] || (( (8#${metadata##*:} & 0022) != 0 )); then
        fail 'unsafe directory ownership or permissions'
    fi
}

private_root_file() {
    [[ -f $1 && ! -L $1 && $(stat -c '%u:%g:%a' "$1") == 0:0:600 ]] || fail "$2 must be a root:root 0600 regular file"
}

# Print the value of exactly one KEY=VALUE line matching the anchored regex.
single_value() {
    local file=$1 key=$2 pattern=$3 values
    values=$(grep -E "^$key=" "$file" | cut -d= -f2-) || return 1
    [[ $values != *$'\n'* && $values =~ ^$pattern$ ]] || return 1
    printf '%s\n' "$values"
}

# Parse the owner config without sourcing it. Sets $destination and the
# $recipients array (40-hex primary key fingerprints).
read_config() {
    local line key value
    destination= recipients=()
    while IFS= read -r line || [[ -n $line ]]; do
        [[ -z $line || $line == '#'* ]] && continue
        [[ $line =~ ^([a-z_]+)=(.*)$ ]] || fail 'malformed config line'
        key=${BASH_REMATCH[1]} value=${BASH_REMATCH[2]}
        case $key in
            destination)
                [[ -z $destination ]] || fail 'duplicate destination'
                [[ $value =~ ^/[A-Za-z0-9._/-]+$ && $value != *'/..'* && $value != *'/./'* ]] || fail 'invalid destination'
                destination=${value%/}
                # Never inside the backed-up tree, volatile memory or service state.
                case $destination/ in
                    /etc/jarvis/*|/run/*|/var/lib/jarvis/*) fail 'destination is inside protected or volatile state' ;;
                esac
                ;;
            recipients)
                (( ${#recipients[@]} == 0 )) || fail 'duplicate recipients'
                [[ $value =~ ^[0-9A-F]{40}(,[0-9A-F]{40})*$ ]] || fail 'recipients must be 40-hex fingerprints'
                IFS=, read -r -a recipients <<< "$value"
                ;;
            *) fail "unknown config key: $key" ;;
        esac
    done < "$1"
    [[ -n $destination && ${#recipients[@]} -gt 0 ]] || fail 'config requires destination and recipients'
}

# Import the owner public keys into a throwaway keyring and require that its
# primary-key fingerprints are exactly the configured recipients.
prepare_keyring() {
    local home=$1 keys=$2 imported expected
    install -d -m 0700 "$home"
    gpg --batch --quiet --homedir "$home" --import "$keys" 2>/dev/null || fail 'cannot import recipient public keys'
    imported=$(gpg --batch --homedir "$home" --with-colons --list-keys 2>/dev/null \
        | awk -F: '$1 == "pub" { want = 1; next } want && $1 == "fpr" { print $10; want = 0 }' | LC_ALL=C sort)
    expected=$(printf '%s\n' "${recipients[@]}" | LC_ALL=C sort)
    [[ $imported == "$expected" ]] || fail 'recipient keys do not match the configured fingerprints'
    # awk reads all input: an early-exiting grep -q would SIGPIPE gpg and,
    # under pipefail, hide a private key.
    if gpg --batch --homedir "$home" --list-secret-keys --with-colons 2>/dev/null \
        | awk -F: '$1 == "sec" { found = 1 } END { exit !found }'; then
        fail 'recipient file must not contain private keys'
    fi
}

encrypt_to() {
    local args=() fpr
    for fpr in "${recipients[@]}"; do args+=(--recipient "$fpr"); done
    gpg --batch --quiet --homedir "$gnupg_home" --trust-model always --no-auto-key-retrieve \
        --encrypt "${args[@]}" --output -
}

# Pass when the restored count lies within the live counts taken immediately
# before and after the single-transaction export.
count_matches() {
    local before=$1 after=$2 restored=$3
    (( restored >= (before < after ? before : after) && restored <= (before > after ? before : after) ))
}

# Write the three encrypted members and the manifest into $1 from the run
# directory: export.surql, live-before.json, live-after.json, restored.json.
build_members() {
    local staging=$1 run=$2 etc_parent=$3 export_sha table before after restored
    export_sha=$(sha256sum < "$run/export.surql" | cut -d' ' -f1)
    zstd -q -c < "$run/export.surql" | encrypt_to > "$staging/surrealdb.surql.zst.gpg"
    # Owner and group names are stored so a fresh host maps them by name.
    tar -C "$etc_parent" --one-file-system -cpf - etc/jarvis \
        | zstd -q -c | encrypt_to > "$staging/etc-jarvis.tar.zst.gpg"
    local tables='[]'
    while IFS= read -r table; do
        before=$(jq -r --arg t "$table" '.[$t] // 0' "$run/live-before.json")
        after=$(jq -r --arg t "$table" '.[$t] // 0' "$run/live-after.json")
        restored=$(jq -r --arg t "$table" '.[$t] // -1' "$run/restored.json")
        tables=$(jq -c --arg t "$table" --argjson b "$before" --argjson a "$after" --argjson r "$restored" \
            '. + [{table: $t, live_before: $b, live_after: $a, restored: $r}]' <<< "$tables")
    done < <(jq -r 'keys[]' "$run/live-before.json")
    local member_info='{}' name hash
    for name in etc-jarvis.tar.zst.gpg surrealdb.surql.zst.gpg; do
        hash=$(sha256sum < "$staging/$name" | cut -d' ' -f1)
        member_info=$(jq -c --arg n "$name" --arg h "$hash" --argjson s "$(stat -c %s "$staging/$name")" \
            '. + {($n): {sha256: $h, bytes: $s}}' <<< "$member_info")
    done
    jq -n --arg created "$(date -u +%FT%TZ)" --arg ns "$namespace" --arg db "$database" \
        --arg image "${image:-}" --arg export_sha "$export_sha" --argjson tables "$tables" \
        --argjson members "$member_info" --arg gpg "$(gpg --version | head -1)" \
        --arg zstd "$(zstd --version)" --arg tar "$(tar --version | head -1)" \
        --argjson recipients "$(printf '%s\n' "${recipients[@]}" | jq -R . | jq -s .)" \
        '{format_version: 1, created_at: $created, namespace: $ns, database: $db,
          surrealdb_image: $image, export_sha256: $export_sha, tables: $tables,
          members: $members, recipients: $recipients,
          tools: {gpg: $gpg, zstd: $zstd, tar: $tar}}' \
        | encrypt_to > "$staging/manifest.json.gpg"
}

# The tar must hold exactly the expected members, each a non-empty regular file.
check_members() {
    local archive=$1 listing mode size name
    listing=$(tar -tvf "$archive" 2>/dev/null) || fail 'archive is not a readable tar'
    [[ $(tar -tf "$archive" | LC_ALL=C sort) == "$(printf '%s\n' "${members[@]}")" ]] || fail 'unexpected archive members'
    while read -r mode _ size _ _ name; do
        [[ $mode == -* && $size -gt 0 ]] || fail "member $name is not a non-empty regular file"
    done <<< "$listing"
}

# Publish $staging as <dest>/jarvis-backup-<day>.tar(.sha256). The tar is
# checked before it replaces anything; each file is renamed atomically.
publish() {
    local staging=$1 dest=$2 day=$3 name tmp hash
    name="jarvis-backup-$day.tar"
    tmp="$staging/$name.partial"
    tar -C "$staging" --owner=0 --group=0 --numeric-owner -cf "$tmp" "${members[@]}"
    chmod 0600 "$tmp"
    check_members "$tmp"
    hash=$(sha256sum < "$tmp" | cut -d' ' -f1)
    printf '%s  %s\n' "$hash" "$name" > "$staging/$name.sha256.partial"
    chmod 0600 "$staging/$name.sha256.partial"
    sync -- "$tmp" "$staging/$name.sha256.partial"
    mv -f -- "$tmp" "$dest/$name"
    mv -f -- "$staging/$name.sha256.partial" "$dest/$name.sha256"
    sync -f -- "$dest"
    printf '%s\n' "$dest/$name"
}

# Keep the newest $keep archives. Only regular files with the exact archive
# name are considered; symlinks and other files are never touched.
prune() {
    local dest=$1 old
    find "$dest" -mindepth 1 -maxdepth 1 -type d -name '.jarvis-backup.*' -exec rm -rf -- {} +
    while IFS= read -r old; do
        rm -f -- "$dest/$old" "$dest/$old.sha256"
    done < <(find "$dest" -mindepth 1 -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/$archive_pattern" -printf '%f\n' | LC_ALL=C sort -r | tail -n +$((keep + 1)))
}

# Integrity and shape check; needs no key.
verify() {
    local archive=$1 expected actual
    [[ ${archive##*/} =~ ^$archive_pattern$ && -f $archive && ! -L $archive ]] || fail 'not a jarvis backup archive'
    [[ -f $archive.sha256 && ! -L $archive.sha256 ]] || fail 'checksum file missing'
    # Exactly one line naming this archive; the hash is recomputed here.
    expected=$(cat -- "$archive.sha256")
    [[ $expected =~ ^([0-9a-f]{64})\ \ ${archive##*/}$ ]] || fail 'malformed checksum file'
    expected=${BASH_REMATCH[1]}
    actual=$(sha256sum < "$archive" | cut -d' ' -f1)
    [[ $actual == "$expected" ]] || fail 'checksum mismatch'
    check_members "$archive"
    log "verified ${archive##*/}"
}

# --- Production database access -------------------------------------------

compose_exec() {
    docker compose --env-file "$env_file" -f "$compose_file" exec -T surrealdb "$@"
}

# Print one SurrealQL statement's JSON result from the live database.
# shellcheck disable=SC2317  # called indirectly through table_counts
live_sql() {
    printf '%s\n' "$1" | compose_exec /surreal sql --hide-welcome --json \
        --endpoint ws://127.0.0.1:8000 --auth-level root --namespace "$namespace" --database "$database"
}

# shellcheck disable=SC2317  # called indirectly through table_counts
verify_sql() {
    printf '%s\n' "$1" | docker exec -i "$verify_container" /surreal sql --hide-welcome --json \
        --endpoint ws://127.0.0.1:8000 --namespace restoretest --database restoretest
}

# Write {"table": count, ...} for every table using the given sql function.
table_counts() {
    local query=$1 out=$2 tables table count counts='{}'
    tables=$("$query" 'INFO FOR DB;' | jq -r '.[0].tables // {} | keys[]')
    while IFS= read -r table; do
        [[ -n $table ]] || continue
        [[ $table =~ ^[A-Za-z_][A-Za-z0-9_]{0,127}$ ]] || fail 'unexpected table name'
        count=$("$query" "SELECT count() AS n FROM \`$table\` GROUP ALL;" | jq -r '.[0][0].n // 0')
        [[ $count =~ ^[0-9]+$ ]] || fail 'unexpected count result'
        counts=$(jq -c --arg t "$table" --argjson n "$count" '. + {($t): $n}' <<< "$counts")
    done <<< "$tables"
    printf '%s\n' "$counts" > "$out"
}

restore_test() {
    local run=$1 name attempt table before after restored
    name="jarvis-backup-verify-${run##*.}"
    docker run -d --rm --name "$name" --network none --user 0:0 \
        --cap-drop ALL --security-opt no-new-privileges:true --memory 1g --cpus 1 --pids-limit 256 \
        -v "$run/export.surql:/restore/export.surql:ro" "$image" start --unauthenticated memory >/dev/null \
        || fail 'cannot start disposable restore container'
    verify_container=$name
    for attempt in $(seq 1 30); do
        docker exec "$verify_container" /surreal isready --endpoint http://127.0.0.1:8000 >/dev/null 2>&1 && break
        (( attempt < 30 )) || fail 'disposable restore container did not become ready'
        sleep 1
    done
    # Full import errors can quote exported statements; show one short line.
    if ! docker exec "$verify_container" /surreal import --endpoint http://127.0.0.1:8000 \
        --namespace restoretest --database restoretest /restore/export.surql >/dev/null 2>"$run/import.err"; then
        fail "export does not import into a disposable database: $(sed 's/\x1b\[[0-9;]*m//g' "$run/import.err" \
            | grep -v '^[[:space:]]*$' | tail -n 2 | cut -c1-240 | tr '\n' ' ')"
    fi
    table_counts verify_sql "$run/restored.json"
    [[ $(jq -c 'keys' "$run/restored.json") == "$(jq -c 'keys' "$run/live-before.json")" ]] \
        || fail 'restored tables differ from the live database'
    while IFS= read -r table; do
        before=$(jq -r --arg t "$table" '.[$t]' "$run/live-before.json")
        after=$(jq -r --arg t "$table" '.[$t] // 0' "$run/live-after.json")
        restored=$(jq -r --arg t "$table" '.[$t]' "$run/restored.json")
        count_matches "$before" "$after" "$restored" || fail "restored row count differs for table $table"
    done < <(jq -r 'keys[]' "$run/live-before.json")
    docker rm -f "$verify_container" >/dev/null 2>&1 || true
    verify_container=
    log "restore test passed for $(jq 'length' "$run/live-before.json") tables"
}

cleanup() {
    [[ -z ${verify_container:-} ]] || docker rm -f "$verify_container" >/dev/null 2>&1 || true
    [[ -z ${gnupg_home:-} ]] || gpgconf --homedir "$gnupg_home" --kill all >/dev/null 2>&1 || true
    [[ -z ${run_dir:-} ]] || rm -rf -- "$run_dir"
    [[ -z ${staging_dir:-} ]] || rm -rf -- "$staging_dir"
}

create() {
    local tool newest need avail day parent
    [[ $EUID == 0 ]] || fail 'root required'
    day=$(date -u +%F)
    for tool in docker gpg gpgconf zstd jq tar sha256sum flock find stat df; do
        command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
    done
    safe_dir "$config_dir"
    private_root_file "$config_dir/backup.conf" 'backup.conf'
    [[ -f $config_dir/recipients.asc && ! -L $config_dir/recipients.asc && \
       $(stat -c '%u:%g' "$config_dir/recipients.asc") == 0:0 && \
       $(( 8#$(stat -c '%a' "$config_dir/recipients.asc") & 0022 )) == 0 ]] || fail 'unsafe recipients.asc'
    read_config "$config_dir/backup.conf"
    [[ $(realpath -e -- "$destination" 2>/dev/null) == "$destination" ]] || fail 'destination must exist and be canonical'
    safe_dir "$destination"
    [[ $(stat -c '%a' "$destination") == 700 ]] || fail 'destination must be mode 0700'
    if [[ -z $fixture_root ]]; then
        parent=$destination
        while [[ $parent != / ]]; do parent=$(dirname -- "$parent"); safe_dir "$parent"; done
    fi
    private_root_file "$env_file" 'surrealdb.env'
    private_root_file "$marker" 'the Core provisioning marker'
    namespace=$(single_value "$marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}') \
        || fail 'provisioning marker needs exactly one valid namespace'
    database=$(single_value "$marker" database '[A-Za-z][A-Za-z0-9_]{0,63}') \
        || fail 'provisioning marker needs exactly one valid database'
    # Read exactly one non-secret key; the env file is never sourced.
    image=$(single_value "$env_file" SURREALDB_IMAGE 'surrealdb/surrealdb@sha256:[0-9a-f]{64}') \
        || fail 'SURREALDB_IMAGE must be exactly one digest-pinned official image'

    exec 8> "$lock_dir/jarvis-backup.lock"
    flock -n 8 || { log 'another backup is running'; exit 75; }
    exec 9> "$lock_dir/jarvis-updater.lock"
    flock -w 600 9 || { log 'a Core update is running'; exit 75; }
    # Writers of the protected configuration: keep the config tar consistent.
    exec 7> "$lock_dir/jarvis-admin-config.lock"
    flock -w 60 7 || { log 'a configuration change is running'; exit 75; }
    exec 6> "$lock_dir/jarvis-private-agent-update.lock"
    flock -w 60 6 || { log 'a private agent update is running'; exit 75; }

    newest=$(find "$destination" -mindepth 1 -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/$archive_pattern" -printf '%s\n' | sort -n | tail -1)
    need=$(( ${newest:-0} * 2 + 1024 * 1024 * 1024 ))
    avail=$(df --output=avail -B1 "$destination" | tail -1)
    (( avail >= need )) || fail 'not enough free space at the destination'
    avail=$(df --output=avail -B1 "$lock_dir" | tail -1)
    (( avail >= 2 * 1024 * 1024 * 1024 )) || fail 'not enough free space below /run for the export'

    trap cleanup EXIT
    run_dir=$(mktemp -d "$lock_dir/jarvis-backup.XXXXXXXX")
    staging_dir=$(mktemp -d "$destination/.jarvis-backup.XXXXXXXX")
    gnupg_home="$run_dir/gnupg"
    prepare_keyring "$gnupg_home" "$config_dir/recipients.asc"

    table_counts live_sql "$run_dir/live-before.json"
    # Bounded: /run is a shared tmpfs and the restore test has 1 GiB.
    ( ulimit -f "$max_export_kib"
      compose_exec /surreal export --endpoint http://127.0.0.1:8000 --auth-level root \
          --namespace "$namespace" --database "$database" > "$run_dir/export.surql" ) \
        || fail 'SurrealDB export failed or exceeded 1 GiB'
    [[ -s $run_dir/export.surql ]] || fail 'SurrealDB export is empty'
    table_counts live_sql "$run_dir/live-after.json"
    restore_test "$run_dir"
    build_members "$staging_dir" "$run_dir" "$etc_parent"
    local archive
    archive=$(publish "$staging_dir" "$destination" "$day")
    verify "$archive"
    prune "$destination"
}

main() {
    config_dir=/etc/jarvis-backup
    etc_parent=/
    lock_dir=/run
    env_file=/etc/jarvis/surrealdb.env
    marker=/etc/jarvis/surrealdb-core-provisioned
    compose_file=/opt/jarvis/surrealdb/docker-compose.yml
    fixture_root=
    if [[ -n ${JARVIS_BACKUP_FIXTURE_ROOT:-} ]]; then
        [[ ${GITHUB_ACTIONS:-} == true && ${JARVIS_BACKUP_TEST_MODE:-} == true && $JARVIS_BACKUP_FIXTURE_ROOT == /tmp/* ]] || fail 'test-only override refused'
        fixture_root=$JARVIS_BACKUP_FIXTURE_ROOT
        [[ $(realpath -e -- "$fixture_root") == "$fixture_root" ]] || fail 'test root must be canonical'
        config_dir="$fixture_root/etc/jarvis-backup"
        etc_parent=$fixture_root
        lock_dir="$fixture_root/run"
        env_file="$fixture_root/etc/jarvis/surrealdb.env"
        marker="$fixture_root/etc/jarvis/surrealdb-core-provisioned"
        compose_file="$fixture_root/docker-compose.yml"
    fi
    case ${1:-} in
        create) [[ $# == 1 ]] || fail 'usage: jarvis-backup create'; create ;;
        verify) [[ $# == 2 ]] || fail 'usage: jarvis-backup verify ARCHIVE'; verify "$2" ;;
        *) fail 'usage: jarvis-backup create | verify ARCHIVE' ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
