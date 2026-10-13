#!/usr/bin/env bash
# Install the protected owner persona from a separately cloned private checkout,
# or restore a version from the root-only persona history. This is
# intentionally root-operated and is never called by the public updater.
set -euo pipefail

readonly destination_dir=/etc/jarvis
readonly destination="$destination_dir/Jarvis.md"
readonly history_dir="$destination_dir/persona-history"
# Persona versions kept in the history, including the installed one.
readonly history_keep=10

fail() { echo "private persona: $*" >&2; exit 1; }
usage() {
    echo "Usage: sudo $0 --source /path/to/PersonalJarvisAgents [--expect-sha256 HEX]" >&2
    echo "       sudo $0 --restore HEX" >&2
    exit 64
}
is_sha256() { [[ $1 =~ ^[0-9a-f]{64}$ ]]; }
file_sha256() { sha256sum -- "$1" | awk '{print $1}'; }

source_root=
expected=
restore=
while (($#)); do
    case "$1" in
        --source) source_root=${2:-}; shift 2 ;;
        --expect-sha256) expected=${2:-}; shift 2 ;;
        --restore) restore=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[[ ${EUID} -eq 0 ]] || fail "must run as root"
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
getent group jarvis >/dev/null || fail "jarvis group is missing; run prepare-home-node first"

if [[ -n $restore ]]; then
    [[ -z $source_root && -z $expected ]] && is_sha256 "$restore" || usage
    [[ -d $history_dir && ! -L $history_dir ]] || fail "persona history is missing"
    source_file="$history_dir/$restore"
    [[ -f $source_file && ! -L $source_file ]] || fail "persona ${restore:0:12} is not in the history"
    [[ $(stat -c '%U' "$source_file") == root ]] || fail "persona history entry is not root-owned"
    expected=$restore
    verb=RESTORE
else
    [[ -n $source_root ]] || usage
    [[ -z $expected ]] || is_sha256 "$expected" || usage
    source_root=$(realpath -e -- "$source_root") || fail "private checkout does not exist"
    [[ -d $source_root && ! -L $source_root ]] || fail "private checkout must be a real directory"
    source_file="$source_root/personaljarvis/jarvis-core/Jarvis.md"
    [[ -f $source_file && ! -L $source_file ]] || fail "private Jarvis.md is missing or is a symlink"
    verb=UPDATE
fi
[[ -s $source_file ]] || fail "persona source is empty"
[[ $(stat -c '%F' "$source_file") == "regular file" ]] || fail "persona source is not a regular file"

source_hash=$(file_sha256 "$source_file")
[[ -z $expected || $source_hash == "$expected" ]] || fail "persona source does not match the expected sha256"
# Only the directory is group-traversable: do not apply this to every file
# beneath /etc/jarvis, which would disclose root-only deployment secrets.
install -d -o root -g jarvis -m 0750 "$destination_dir"
install -d -o root -g root -m 0700 "$history_dir"
current_hash=
if [[ -f $destination && ! -L $destination ]]; then
    current_hash=$(file_sha256 "$destination")
    if [[ $current_hash == "$source_hash" ]]; then
        echo "UNCHANGED protected persona sha256=${source_hash:0:12}"
        exit 0
    fi
fi

umask 077
archive_temporary=
stage=$(mktemp "$destination_dir/.Jarvis.md.XXXXXX")
trap 'rm -f -- "$stage" ${archive_temporary:+"$archive_temporary"}' EXIT
cp -- "$source_file" "$stage"
chown root:jarvis "$stage"
chmod 0640 "$stage"
[[ $(stat -c '%U:%G:%a' "$stage") == root:jarvis:640 ]] || fail "cannot secure staged persona"
[[ $(file_sha256 "$stage") == "$source_hash" ]] || fail "staged persona hash mismatch"

# Keep root-controlled rollback copies without printing their private
# contents. Re-archiving an existing entry marks it as recently used.
archive() {
    local file=$1 hash=$2 entry="$history_dir/$2"
    if [[ -f $entry && ! -L $entry ]]; then
        touch -- "$entry"
        return
    fi
    archive_temporary=$(mktemp "$history_dir/.archive.XXXXXX")
    cp -- "$file" "$archive_temporary"
    chown root:jarvis "$archive_temporary"
    chmod 0640 "$archive_temporary"
    [[ $(file_sha256 "$archive_temporary") == "$hash" ]] || fail "archived persona hash mismatch"
    mv -Tf -- "$archive_temporary" "$entry"
}

# Keep the most recently used versions; never remove the installed one.
prune_history() {
    local count=0 name
    while IFS= read -r -d '' name; do
        is_sha256 "$name" || continue
        count=$((count + 1))
        if ((count > history_keep)) && [[ $name != "$source_hash" ]]; then
            rm -f -- "${history_dir:?}/$name"
        fi
    done < <(find "$history_dir" -mindepth 1 -maxdepth 1 -type f -printf '%T@ %f\0' |
        sort -z -r -n | cut -z -d ' ' -f 2-)
}

[[ -z $current_hash ]] || archive "$destination" "$current_hash"
archive "$stage" "$source_hash"
mv -f -- "$stage" "$destination"
[[ $(stat -c '%U:%G:%a' "$destination") == root:jarvis:640 ]] || fail "installed persona has unsafe ownership or mode"
[[ $(file_sha256 "$destination") == "$source_hash" ]] || fail "installed persona hash mismatch"
prune_history
echo "$verb protected persona sha256=${source_hash:0:12}"
