#!/usr/bin/env bash
# Install the protected owner persona from a separately cloned private checkout.
# This is intentionally root-operated and is never called by the public updater.
set -euo pipefail

readonly destination_dir=/etc/jarvis
readonly destination="$destination_dir/Jarvis.md"
readonly history_dir="$destination_dir/persona-history"

fail() { echo "private persona: $*" >&2; exit 1; }
usage() { echo "Usage: sudo $0 --source /path/to/PersonalJarvisAgents" >&2; exit 64; }

source_root=
while (($#)); do
    case "$1" in
        --source) source_root=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ -n $source_root ]] || usage
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is required"
getent group jarvis >/dev/null || fail "jarvis group is missing; run prepare-home-node first"

source_root=$(realpath -e -- "$source_root") || fail "private checkout does not exist"
[[ -d $source_root && ! -L $source_root ]] || fail "private checkout must be a real directory"
source_file="$source_root/personaljarvis/jarvis-core/Jarvis.md"
[[ -f $source_file && ! -L $source_file ]] || fail "private Jarvis.md is missing or is a symlink"
[[ -s $source_file ]] || fail "private Jarvis.md is empty"
[[ $(stat -c '%F' "$source_file") == "regular file" ]] || fail "private Jarvis.md is not a regular file"

source_hash=$(sha256sum "$source_file" | awk '{print $1}')
install -d -o root -g root -m 0750 "$destination_dir" "$history_dir"
if [[ -f $destination && ! -L $destination ]]; then
    current_hash=$(sha256sum "$destination" | awk '{print $1}')
    if [[ $current_hash == "$source_hash" ]]; then
        echo "UNCHANGED protected persona sha256=${source_hash:0:12}"
        exit 0
    fi
fi

stage=$(mktemp "$destination_dir/.Jarvis.md.XXXXXX")
trap 'rm -f -- "$stage"' EXIT
umask 077
cp -- "$source_file" "$stage"
chown root:jarvis "$stage"
chmod 0640 "$stage"
[[ $(stat -c '%U:%G:%a' "$stage") == root:jarvis:640 ]] || fail "cannot secure staged persona"
[[ $(sha256sum "$stage" | awk '{print $1}') == "$source_hash" ]] || fail "staged persona hash mismatch"

# Keep a root-controlled rollback copy without printing its private contents.
if [[ ! -e $history_dir/$source_hash ]]; then
    install -o root -g jarvis -m 0640 "$stage" "$history_dir/$source_hash"
fi
mv -f -- "$stage" "$destination"
trap - EXIT
[[ $(stat -c '%U:%G:%a' "$destination") == root:jarvis:640 ]] || fail "installed persona has unsafe ownership or mode"
[[ $(sha256sum "$destination" | awk '{print $1}') == "$source_hash" ]] || fail "installed persona hash mismatch"
echo "UPDATE protected persona sha256=${source_hash:0:12}"
