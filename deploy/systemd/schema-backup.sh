#!/usr/bin/env bash
# Cold, root-only snapshot for the fixed production database. No credentials
# are read into this process and no caller-selected database path is accepted.
set -euo pipefail
umask 077
fail() { echo "jarvis schema backup: $*" >&2; exit 1; }
[[ $EUID == 0 ]] || fail 'root required'
root=/var/lib/jarvis
backups=/var/backups/jarvis-migrations
if [[ -n ${JARVIS_SCHEMA_FIXTURE_ROOT:-} ]]; then
    [[ ${GITHUB_ACTIONS:-} == true && ${JARVIS_SCHEMA_TEST_MODE:-} == true && $JARVIS_SCHEMA_FIXTURE_ROOT == /tmp/* ]] || fail 'test-only override refused'
    root=$JARVIS_SCHEMA_FIXTURE_ROOT
    backups="$root/migration-backups"
fi
database="$root/surrealdb"
safe_dir() {
    [[ -d $1 && ! -L $1 ]] || fail 'required directory is missing or symlinked'
    local metadata
    metadata=$(stat -c '%u:%g:%a' "$1")
    [[ $metadata == 0:0:* ]] && (( (8#${metadata##*:} & 0022) == 0 )) || fail 'unsafe directory ownership or permissions'
}
# Production /var/lib/jarvis is owned by the service account. Never store
# recovery material there: an account owning the parent could rename/remove
# even root-owned child directories. Backups have a root-controlled parent.
[[ -d $root && ! -L $root ]] || fail 'unsafe application state directory'
if [[ $backups == /var/backups/jarvis-migrations ]]; then
    safe_dir /var
    safe_dir /var/backups
fi
[[ ! -L $backups ]] || fail 'unsafe backup root'
if [[ ! -e $backups ]]; then install -d -o root -g root -m 0700 "$backups"; fi
safe_dir "$backups"
[[ $(stat -c '%a' "$backups") == 700 ]] || fail 'backup root must be private'

stop_database() {
    systemctl stop jarvis-core.service || return 1
    systemctl stop jarvis-surrealdb.service || return 1
    if systemctl is-active --quiet jarvis-core.service || systemctl is-active --quiet jarvis-surrealdb.service; then
        fail 'services did not stop'
    fi
    # Verify Docker really stopped the fixed compose service. Never inspect
    # container environments, and never put database credentials in arguments.
    local running
    running=$(docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml ps --status running -q) || return 1
    [[ -z $running ]] || fail 'database container is still running'
}
validate_tree() {
    local tree=$1 bad
    safe_dir "$tree"
    bad=$(find "$tree" -xdev ! -type f ! -type d -print -quit)
    [[ -z $bad ]] || fail 'database snapshot contains a link or special file'
    # RocksDB filenames are not user input. Reject ambiguous checksum paths.
    while IFS= read -r -d '' name; do
        [[ $name =~ ^[A-Za-z0-9._/-]+$ && $name != *'/../'* ]] || fail 'unsafe database filename'
    done < <(cd "$tree" && find . -xdev -type f -print0)
}
verify_snapshot() {
    safe_dir "$transaction"
    [[ $(stat -c '%a' "$transaction") == 700 && -f $transaction/complete && ! -L $transaction/complete ]] || fail 'snapshot is incomplete'
    [[ -f $transaction/checksums && ! -L $transaction/checksums ]] || fail 'snapshot checksums missing'
    validate_tree "$transaction/database"
    (cd "$transaction/database" && find . -xdev -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$transaction/checksums.verify"
    cmp -s "$transaction/checksums" "$transaction/checksums.verify" || fail 'snapshot integrity mismatch'
    rm -- "$transaction/checksums.verify"
}

case ${1:-} in
    create)
        [[ $# == 3 && $2 =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ && $3 =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'expected previous and candidate release tags'
        safe_dir "$database"
        [[ $(stat -c '%a' "$database") == 700 ]] || fail 'database directory must be private'
        stop_database || fail 'could not stop database for snapshot'
        validate_tree "$database"
        transaction=$(mktemp -d "$backups/txn.XXXXXXXX")
        cp -a --reflink=auto -- "$database" "$transaction/database" || fail 'snapshot copy failed; original database unchanged'
        diff -qr -- "$database" "$transaction/database" >/dev/null || fail 'snapshot copy verification failed'
        (cd "$transaction/database" && find . -xdev -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum) > "$transaction/checksums"
        printf '%s\n%s\n' "$2" "$3" > "$transaction/releases"
        touch "$transaction/complete"
        sync -f "$transaction"
        printf '%s\n' "${transaction##*/}"
        ;;
    restore|commit)
        [[ $# == 2 && $2 =~ ^txn\.[A-Za-z0-9]{8}$ ]] || fail 'invalid snapshot ID'
        transaction="$backups/$2"
        verify_snapshot
        [[ ! -e $transaction/committed ]] || fail 'post-commit database rewind is forbidden; use an explicit disaster recovery procedure'
        if [[ $1 == commit ]]; then
            touch "$transaction/committed"
            sync -f "$transaction"
            exit 0
        fi
        stop_database || fail 'could not stop database for recovery'
        safe_dir "$database"
        [[ ! -e $root/.surrealdb-failed-$2 ]] || fail 'recovery already started; preserve both trees for operator inspection'
        replacement=$(mktemp -d "$root/.surrealdb-restore.XXXXXXXX")
        cp -a --reflink=auto -- "$transaction/database/." "$replacement/" || fail 'recovery copy failed; live tree unchanged'
        chmod 0700 "$replacement"
        diff -qr -- "$replacement" "$transaction/database" >/dev/null || fail 'recovery copy verification failed'
        sync -f "$replacement"
        mv -T -- "$database" "$root/.surrealdb-failed-$2"
        mv -T -- "$replacement" "$database"
        sync -f "$root"
        echo 'jarvis schema backup: previous database restored; failed candidate data retained privately' >&2
        ;;
    *) fail 'usage: schema-backup create PREVIOUS_TAG CANDIDATE_TAG | restore SNAPSHOT_ID | commit SNAPSHOT_ID' ;;
esac
