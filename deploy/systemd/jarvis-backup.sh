#!/usr/bin/env bash
# Owner-run disaster-recovery backup for the Home Node. One run writes one
# dated tar of gpg-encrypted members plus a sha256 file:
#   surrealdb.surql.zst.gpg  logical export of the Core namespace/database
#   etc-jarvis.tar.zst.gpg   protected configuration including secrets
#   manifest.json.gpg        versions, table counts and member hashes
# Members are encrypted to owner public keys only: this host cannot decrypt
# its own backups (`init-local` generates that key here once and prints the
# private half for the owner to store elsewhere). Before encryption the export
# is imported into a disposable, networkless SurrealDB container and its table
# counts are compared with the live database. Nothing is uploaded. Archives
# are kept 7 daily plus 4 weekly.
#
# The only plaintext file of `create` is the export, on tmpfs below /run,
# removed on exit. `verify --decrypt` and `restore` read the owner private key
# from stdin into a throwaway keyring there and decrypt there as well; only
# `restore` moves the plaintext to the directory the owner names.
# No secret is read into this process or placed on a command line: SurrealDB
# root credentials stay in the production container environment.
set -euo pipefail
shopt -s inherit_errexit
umask 077

readonly archive_pattern='jarvis-backup-[0-9]{4}-[0-9]{2}-[0-9]{2}\.tar'
readonly members=(etc-jarvis.tar.zst.gpg manifest.json.gpg surrealdb.surql.zst.gpg)
readonly keep_daily=7 keep_weekly=4
# The disposable restore container has 1 GiB; a larger export cannot be tested.
readonly max_export_kib=$((1024 * 1024))
readonly max_manifest_kib=1024

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
    destination='' recipients=()
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

# The tar must hold exactly the expected members, each a non-empty regular file
# owned by 0/0 without owner names, as publish writes them. The listing is
# untrusted: owner names could shift fields, so the split listing is numeric,
# and the size is matched, never evaluated arithmetically.
check_members() {
    local archive=$1 listing mode owner size name
    listing=$(tar --numeric-owner -tvf "$archive" 2>/dev/null) || fail 'archive is not a readable tar'
    [[ $(tar -tf "$archive" | LC_ALL=C sort) == "$(printf '%s\n' "${members[@]}")" ]] || fail 'unexpected archive members'
    # Owner names (even "0" or ones holding a newline) make the named listing
    # differ from the numeric one.
    [[ $(tar -tvf "$archive" 2>/dev/null) == "$listing" ]] || fail 'unexpected member owner'
    while read -r mode owner size _ _ name; do
        [[ $owner == 0/0 ]] || fail 'unexpected member owner'
        [[ $mode == -* && $size =~ ^[1-9][0-9]*$ ]] || fail "member $name is not a non-empty regular file"
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

# Keep the just-published archive $2, the newest $keep_daily archives and the
# newest archive of each of the newest $keep_weekly ISO weeks (names carry one
# date, so one archive per day). The current archive is never a deletion
# candidate, and nothing is pruned while a future-dated name (clock skew,
# manual copy) exists, so a wrong clock cannot evict history. Only regular
# files with the exact archive name and a valid date are considered; symlinks
# and other files are never touched.
prune() {
    local dest=$1 current=$2 name week last_week='' days=0 weeks=0 new_week
    [[ $current =~ ^$archive_pattern$ ]] || fail 'prune needs the current archive name'
    find "$dest" -mindepth 1 -maxdepth 1 -type d -name '.jarvis-backup.*' -exec rm -rf -- {} +
    while IFS= read -r name; do
        week=$(date -u -d "${name:14:10}" +%G-%V 2>/dev/null) || continue
        # Names sort newest first, so this returns before anything is deleted.
        if [[ $name > $current ]]; then
            log 'an archive is dated after the current one; not pruning (check the clock)'
            return 0
        fi
        new_week=false
        if [[ $week != "$last_week" ]]; then weeks=$((weeks + 1)) last_week=$week new_week=true; fi
        days=$((days + 1))
        [[ $name == "$current" ]] || (( days <= keep_daily )) || { $new_week && (( weeks <= keep_weekly )); } \
            || rm -f -- "$dest/$name" "$dest/$name.sha256"
    done < <(find "$dest" -mindepth 1 -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/$archive_pattern" -printf '%f\n' | LC_ALL=C sort -r)
}

# Refuse to start unless the destination keeps room for two archives of the
# newest size plus 1 GiB, and $2 (tmpfs) has 2 GiB for the plaintext export.
check_space() {
    local dest=$1 run=$2 newest need
    newest=$(find "$dest" -mindepth 1 -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/$archive_pattern" -printf '%s\n' | sort -n | tail -1)
    need=$(( ${newest:-0} * 2 + 1024 * 1024 * 1024 ))
    (( $(df --output=avail -B1 "$dest" | tail -1) >= need )) || fail 'not enough free space at the destination'
    (( $(df --output=avail -B1 "$run" | tail -1) >= 2 * 1024 * 1024 * 1024 )) \
        || fail 'not enough free space below /run for the export'
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

decrypt_member() {
    gpg --batch --quiet --homedir "$gnupg_home" --no-auto-key-retrieve --decrypt -- "$run_dir/members/$1" 2>/dev/null
}

# Decrypt a verified archive with the owner private key read from stdin into
# the throwaway keyring, check every member against the manifest, and leave
# manifest.json, export.surql and etc-jarvis.tar in $run_dir/out (tmpfs below
# /run, removed on exit). Each plaintext output is capped like the export.
decrypt_archive() {
    local archive=$1 out="$run_dir/out" copy name want size
    [[ ${archive##*/} =~ ^$archive_pattern$ && -f $archive && ! -L $archive ]] || fail 'not a jarvis backup archive'
    [[ -f $archive.sha256 && ! -L $archive.sha256 ]] || fail 'checksum file missing'
    size=$(stat -c %s -- "$archive")
    # A private copy, the ciphertext members and up to 1 GiB per plaintext
    # output share /run.
    (( $(df --output=avail -B1 "$lock_dir" | tail -1) >= 2 * size + 2 * 1024 * 1024 * 1024 )) \
        || fail 'not enough free space below /run to decrypt'
    # Verify and extract the same private copy: the archive directory may be
    # writable by others, who could swap the file between the two steps. The
    # copy is capped at the size checked above.
    copy="$run_dir/${archive##*/}"
    ( ulimit -f $(( size / 1024 + 1 )) \
        && cp -P -- "$archive" "$copy" && cp -P -- "$archive.sha256" "$copy.sha256" ) \
        || fail 'cannot copy the archive'
    verify "$copy"
    install -d -m 0700 "$gnupg_home" "$run_dir/members" "$out"
    [[ ! -t 0 ]] || log 'paste the backup private key, then press Ctrl-D'
    gpg --batch --quiet --homedir "$gnupg_home" --import 2>/dev/null || fail 'cannot import the private key from stdin'
    gpg --batch --homedir "$gnupg_home" --list-secret-keys --with-colons 2>/dev/null \
        | awk -F: '$1 == "sec" { found = 1 } END { exit !found }' || fail 'stdin holds no private key'
    tar -C "$run_dir/members" --no-same-owner -xf "$copy" "${members[@]}"
    rm -f -- "$copy" "$copy.sha256"
    ( ulimit -f "$max_manifest_kib" && decrypt_member manifest.json.gpg > "$out/manifest.json" ) \
        || fail 'cannot decrypt the manifest (wrong key or over 1 MiB?)'
    jq -e '.format_version == 1' "$out/manifest.json" >/dev/null 2>&1 || fail 'unsupported manifest'
    for name in etc-jarvis.tar.zst.gpg surrealdb.surql.zst.gpg; do
        want=$(jq -r --arg n "$name" '.members[$n].sha256 // ""' "$out/manifest.json")
        [[ $(sha256sum < "$run_dir/members/$name" | cut -d' ' -f1) == "$want" ]] || fail "member $name does not match the manifest"
    done
    # Chained with &&: errexit does not apply inside a || list.
    ( ulimit -f "$max_export_kib" \
        && decrypt_member surrealdb.surql.zst.gpg | zstd -dq > "$out/export.surql" \
        && decrypt_member etc-jarvis.tar.zst.gpg | zstd -dq > "$out/etc-jarvis.tar" ) \
        || fail 'cannot decrypt the data members'
    [[ $(sha256sum < "$out/export.surql" | cut -d' ' -f1) == "$(jq -r '.export_sha256' "$out/manifest.json")" ]] \
        || fail 'export does not match the manifest'
    # The disaster-recovery rule: only regular files and directories below etc/jarvis/.
    tar -tvf "$out/etc-jarvis.tar" | awk '$1 !~ /^[-d]/ { bad = 1 } END { exit bad }' \
        || fail 'configuration tar has an unexpected entry type'
    tar -tf "$out/etc-jarvis.tar" | awk '$0 !~ /^etc\/jarvis(\/|$)/ || /(^|\/)\.\.(\/|$)/ { bad = 1 } END { exit bad }' \
        || fail 'configuration tar has an entry outside etc/jarvis'
}

take_backup_lock() {
    exec 8> "$lock_dir/jarvis-backup.lock"
    flock -n 8 || { log 'another backup is running'; exit 75; }
}

# A private tmpfs run directory. Leftovers of killed runs (plaintext, a
# private key) go first.
begin_private_run() {
    take_backup_lock
    remove_stale_run_dirs
    trap cleanup EXIT
    run_dir=$(mktemp -d "$lock_dir/jarvis-backup.XXXXXXXX")
    gnupg_home="$run_dir/gnupg"
}

verify_decrypt() {
    begin_private_run
    decrypt_archive "$1"
    log "decrypted ${1##*/}: every member matches the manifest"
}

# Decrypt into the new private directory $2. Live data is never touched.
restore_to() {
    local archive=$1 target=$2
    [[ $target =~ ^/[A-Za-z0-9._/-]+$ && $target != *'/..'* && ! -e $target && ! -L $target ]] \
        || fail 'restore target must be a new absolute path'
    safe_dir "$(dirname -- "$target")"
    # Same filesystem as the run directory: the final move is a rename, so
    # plaintext never lands half-copied on disk.
    [[ $(stat -c %d -- "$(dirname -- "$target")") == "$(stat -c %d -- "$lock_dir")" ]] \
        || fail "restore target must be on the same filesystem as $lock_dir"
    begin_private_run
    decrypt_archive "$archive"
    mv -T -- "$run_dir/out" "$target"
    log "restored ${archive##*/} into $target; it holds plaintext secrets, delete it when done"
}

# One-time local-only setup: generate a key pair in a throwaway tmpfs keyring,
# print the private key once on stdout for the owner to store off this
# machine, and keep only the public key here. Never overwrites a config.
init_local() {
    local dest=$1 fpr name
    for name in backup.conf recipients.asc; do
        [[ ! -e $config_dir/$name && ! -L $config_dir/$name ]] || fail "already configured: $config_dir/$name exists"
    done
    begin_private_run
    install -d -m 0700 "$gnupg_home"
    gpg --batch --quiet --homedir "$gnupg_home" --passphrase '' \
        --quick-gen-key "Jarvis local backup $(date -u +%F)" default default never 2>/dev/null \
        || fail 'cannot generate the backup key'
    fpr=$(gpg --batch --homedir "$gnupg_home" --with-colons --list-keys 2>/dev/null \
        | awk -F: '$1 == "fpr" && !f { print $10; f = 1 }')
    printf 'destination=%s\nrecipients=%s\n' "$dest" "$fpr" > "$run_dir/backup.conf"
    read_config "$run_dir/backup.conf"
    gpg --batch --homedir "$gnupg_home" --armor --export "$fpr" > "$run_dir/recipients.asc"
    prepare_keyring "$run_dir/check" "$run_dir/recipients.asc"
    gpgconf --homedir "$run_dir/check" --kill all >/dev/null 2>&1 || true
    install -d -m 0700 "$config_dir" "$dest"
    safe_dir "$config_dir"
    safe_dir "$dest"
    log 'Store the private key below in your password manager now, then clear the terminal.'
    log 'Without it no archive can be decrypted; this host keeps only the public key.'
    gpg --batch --homedir "$gnupg_home" --armor --pinentry-mode loopback --passphrase '' \
        --export-secret-keys "$fpr" || fail 'cannot print the private key'
    install -m 0600 "$run_dir/recipients.asc" "$config_dir/recipients.asc"
    install -m 0600 "$run_dir/backup.conf" "$config_dir/backup.conf"
    log "configured $dest for key $fpr"
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
    # SurrealDB 3 no longer creates the target on first use.
    printf 'DEFINE NAMESPACE IF NOT EXISTS restoretest; USE NS restoretest; DEFINE DATABASE IF NOT EXISTS restoretest;\n' \
        | docker exec -i "$verify_container" /surreal sql --hide-welcome --endpoint ws://127.0.0.1:8000 >/dev/null \
        || fail 'cannot define the disposable restore database'
    # Import errors quote exported statements (password hashes, user data).
    # Report only the error class and position, never the quoted text.
    if ! docker exec "$verify_container" /surreal import --log error --endpoint http://127.0.0.1:8000 \
        --namespace restoretest --database restoretest /restore/export.surql >/dev/null 2>"$run/import.err"; then
        local reason
        reason=$(grep -oE '(Parse|Thrown|Database|IO|Api) error|-->[[:space:]]*\[[0-9]+:[0-9]+\]' "$run/import.err" \
            | head -n 2 | LC_ALL=C tr -cd '[:alnum:] :[]>\n-' | tr '\n' ' ') || true
        fail "export does not import into a disposable database (${reason:-unclassified error})"
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

# A run killed with SIGKILL (OOM, stop timeout) skips the EXIT trap and leaves
# its plaintext run directory and restore container behind. Call only while
# holding the backup lock, so no live run owns them. Symlinks are never
# followed or removed.
remove_stale_runs() {
    local ids
    local -a containers=()
    ids=$(docker ps -aq --filter 'name=^jarvis-backup-verify-') || fail 'cannot list stale restore containers'
    if [[ -n $ids ]]; then
        mapfile -t containers <<< "$ids"
        docker rm -f "${containers[@]}" >/dev/null || fail 'cannot remove stale restore containers'
    fi
    remove_stale_run_dirs
}

remove_stale_run_dirs() {
    find "$lock_dir" -mindepth 1 -maxdepth 1 -type d -name 'jarvis-backup.????????' -exec rm -rf -- {} +
}

# Check and read the owner configuration; sets $destination and $recipients.
load_config() {
    local parent
    [[ -e $config_dir/backup.conf || -L $config_dir/backup.conf ]] \
        || fail "not configured: $config_dir/backup.conf is missing (see docs/BACKUP_AND_RESTORE.md)"
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
}

latest_archive() {
    local name
    name=$(find "$destination" -mindepth 1 -maxdepth 1 -type f -regextype posix-extended \
        -regex ".*/$archive_pattern" -printf '%f\n' | LC_ALL=C sort | tail -n 1)
    [[ -n $name ]] || fail "no archive in $destination yet"
    printf '%s\n' "$destination/$name"
}

create() {
    local tool day
    [[ $EUID == 0 ]] || fail 'root required'
    day=$(date -u +%F)
    for tool in docker gpg gpgconf zstd jq tar sha256sum flock find stat df; do
        command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
    done
    load_config
    private_root_file "$env_file" 'surrealdb.env'
    private_root_file "$marker" 'the Core provisioning marker'
    namespace=$(single_value "$marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}') \
        || fail 'provisioning marker needs exactly one valid namespace'
    database=$(single_value "$marker" database '[A-Za-z][A-Za-z0-9_]{0,63}') \
        || fail 'provisioning marker needs exactly one valid database'
    # Read exactly one non-secret key; the env file is never sourced.
    image=$(single_value "$env_file" SURREALDB_IMAGE 'surrealdb/surrealdb@sha256:[0-9a-f]{64}') \
        || fail 'SURREALDB_IMAGE must be exactly one digest-pinned official image'

    take_backup_lock
    remove_stale_runs
    exec 9> "$lock_dir/jarvis-updater.lock"
    flock -w 600 9 || { log 'a Core update is running'; exit 75; }
    # Writers of the protected configuration: keep the config tar consistent.
    exec 7> "$lock_dir/jarvis-admin-config.lock"
    flock -w 60 7 || { log 'a configuration change is running'; exit 75; }
    exec 6> "$lock_dir/jarvis-private-agent-update.lock"
    flock -w 60 6 || { log 'a private agent update is running'; exit 75; }

    check_space "$destination" "$lock_dir"

    trap cleanup EXIT
    run_dir=$(mktemp -d "$lock_dir/jarvis-backup.XXXXXXXX")
    staging_dir=$(mktemp -d "$destination/.jarvis-backup.XXXXXXXX")
    gnupg_home="$run_dir/gnupg"
    prepare_keyring "$gnupg_home" "$config_dir/recipients.asc"

    table_counts live_sql "$run_dir/live-before.json"
    # Bounded: /run is a shared tmpfs and the restore test has 1 GiB. The CLI
    # writes info-level logs to stdout, which would corrupt the export stream.
    ( ulimit -f "$max_export_kib"
      compose_exec /surreal export --log error --endpoint http://127.0.0.1:8000 --auth-level root \
          --namespace "$namespace" --database "$database" > "$run_dir/export.surql" ) \
        || fail 'SurrealDB export failed or exceeded 1 GiB'
    [[ -s $run_dir/export.surql ]] || fail 'SurrealDB export is empty'
    # Terminal escapes mean log output leaked into the stream.
    if grep -q $'\x1b' "$run_dir/export.surql"; then fail 'SurrealDB export contains log output'; fi
    table_counts live_sql "$run_dir/live-after.json"
    restore_test "$run_dir"
    build_members "$staging_dir" "$run_dir" "$etc_parent"
    local archive
    archive=$(publish "$staging_dir" "$destination" "$day")
    verify "$archive"
    prune "$destination" "${archive##*/}"
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
    local usage='usage: jarvis-backup create | verify [--decrypt] [ARCHIVE] | restore ARCHIVE DIR | init-local'
    local decrypt=false archive
    case ${1:-} in
        create) [[ $# == 1 ]] || fail "$usage"; create ;;
        verify)
            shift
            if [[ ${1:-} == --decrypt ]]; then decrypt=true; shift; fi
            (( $# <= 1 )) || fail "$usage"
            if (( $# == 0 )) || $decrypt; then [[ $EUID == 0 ]] || fail 'root required'; fi
            if (( $# == 0 )); then load_config; archive=$(latest_archive); else archive=$1; fi
            if $decrypt; then verify_decrypt "$archive"; else verify "$archive"; fi
            ;;
        restore)
            [[ $# == 3 ]] || fail "$usage"
            [[ $EUID == 0 ]] || fail 'root required'
            restore_to "$2" "$3"
            ;;
        init-local)
            [[ $# == 1 ]] || fail "$usage"
            [[ $EUID == 0 ]] || fail 'root required'
            init_local /var/backups/jarvis-dr
            ;;
        *) fail "$usage" ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
