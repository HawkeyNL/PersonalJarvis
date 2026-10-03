#!/usr/bin/env bash
# Rootless checks for jarvis-backup: config parsing, encryption to pinned
# public keys, archive verification, rotation and secret hygiene. Uses real
# gpg with throwaway keys; no database, Docker or root is involved.
set -euo pipefail
shopt -s inherit_errexit
# shellcheck disable=SC2034  # globals below are consumed by the sourced script
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
# shellcheck disable=SC2317  # invoked through the EXIT trap
cleanup() {
    for home in "$fixture"/keys-*; do gpgconf --homedir "$home" --kill all >/dev/null 2>&1 || true; done
    gpgconf --homedir "$fixture/run/gnupg" --kill all >/dev/null 2>&1 || true
    rm -rf -- "$fixture"
}
trap cleanup EXIT
# shellcheck source=deploy/systemd/jarvis-backup.sh
source "$repo/deploy/systemd/jarvis-backup.sh"
expect_fail() {
    if ( "$@" ) >/dev/null 2>&1; then echo "expected failure: $*" >&2; exit 1; fi
}

# --- Config parser ----------------------------------------------------------
fpr_a=$(printf 'A%.0s' {1..40})
fpr_b=$(printf 'B%.0s' {1..40})
config() { printf '%s\n' "$@" > "$fixture/backup.conf"; }
config '# comment' '' "destination=/var/backups/jarvis-dr" "recipients=$fpr_a,$fpr_b"
read_config "$fixture/backup.conf"
[[ $destination == /var/backups/jarvis-dr && ${recipients[*]} == "$fpr_a $fpr_b" ]]
# shellcheck disable=SC2016  # literal command-substitution text is the input
for bad in 'destination=/x;rm -rf /' 'destination=relative' 'destination=/var/../etc' \
    'recipients=abc' "recipients=$fpr_a;$fpr_b" 'rivetlink_hook=/x' 'destination =/x' '$(id)=/x'; do
    config "$bad" "destination=/var/backups/x" "recipients=$fpr_a"
    expect_fail read_config "$fixture/backup.conf"
done
config "destination=/var/backups/x"
expect_fail read_config "$fixture/backup.conf"
for inside in /etc/jarvis/backups /run/backups /var/lib/jarvis/backups; do
    config "destination=$inside" "recipients=$fpr_a"
    expect_fail read_config "$fixture/backup.conf"
done
config "destination=/var/backups/x" "destination=/var/backups/y" "recipients=$fpr_a"
expect_fail read_config "$fixture/backup.conf"
config "destination=/var/backups/x" "recipients=$fpr_a" "recipients=$fpr_b"
expect_fail read_config "$fixture/backup.conf"

# --- Single-key reads from root-owned files -------------------------------------
printf 'namespace=jarvis\ndatabase=core\n' > "$fixture/marker"
[[ $(single_value "$fixture/marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}') == jarvis ]]
printf 'namespace=jarvis\nnamespace=other\n' > "$fixture/marker"
expect_fail single_value "$fixture/marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}'
printf 'namespace=bad;name\n' > "$fixture/marker"
expect_fail single_value "$fixture/marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}'
printf 'other=1\n' > "$fixture/marker"
expect_fail single_value "$fixture/marker" namespace '[A-Za-z][A-Za-z0-9_]{0,63}'

# --- Throwaway owner keys -----------------------------------------------------
new_key() {
    local home="$fixture/keys-$1"
    install -d -m 0700 "$home"
    gpg --batch --quiet --homedir "$home" --passphrase '' \
        --quick-gen-key "Backup $1 <$1@example.invalid>" default default never 2>/dev/null
    gpg --batch --homedir "$home" --armor --export > "$fixture/$1.asc"
    gpg --batch --homedir "$home" --with-colons --list-keys 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }'
}
owner=$(new_key owner)
other=$(new_key other)
recipients=("$owner")

# Fingerprint pinning: wrong, extra or private keys are refused.
mkdir -p "$fixture/run"
recipients=("$other")
expect_fail prepare_keyring "$fixture/pin-wrong" "$fixture/owner.asc"
cat "$fixture/owner.asc" "$fixture/other.asc" > "$fixture/both.asc"
recipients=("$owner")
expect_fail prepare_keyring "$fixture/pin-extra" "$fixture/both.asc"
gpg --batch --homedir "$fixture/keys-owner" --armor --export-secret-keys --pinentry-mode loopback \
    --passphrase '' > "$fixture/secret.asc" 2>/dev/null
expect_fail prepare_keyring "$fixture/pin-secret" "$fixture/secret.asc"
gnupg_home="$fixture/run/gnupg"
prepare_keyring "$gnupg_home" "$fixture/owner.asc"

# --- Build, publish and verify an archive ------------------------------------
canary='CANARY-not-for-output-7d1c'
mkdir -p "$fixture/root/etc/jarvis/secrets" "$fixture/dest"
chmod 0700 "$fixture/dest"
printf 'PROVIDER_KEY=%s\n' "$canary" > "$fixture/root/etc/jarvis/secrets/provider.env"
printf 'DEFINE TABLE marker; CREATE marker:one; -- %s\n' "$canary" > "$fixture/run/export.surql"
printf '{"marker":1}\n' > "$fixture/run/live-before.json"
printf '{"marker":1}\n' > "$fixture/run/live-after.json"
printf '{"marker":1}\n' > "$fixture/run/restored.json"
namespace=jarvis database=core image="surrealdb/surrealdb@sha256:$(printf '0%.0s' {1..64})"
mkdir "$fixture/stage"
output=$( { build_members "$fixture/stage" "$fixture/run" "$fixture/root"
            publish "$fixture/stage" "$fixture/dest" 2026-10-03; } 2>&1 )
archive="$fixture/dest/jarvis-backup-2026-10-03.tar"
[[ $output == "$archive" ]]
[[ $(stat -c '%a' "$archive") == 600 && $(stat -c '%a' "$archive.sha256") == 600 ]]
verify "$archive" 2>/dev/null
if grep -aqF "$canary" "$archive"; then echo 'plaintext secret inside the archive' >&2; exit 1; fi

# Only the owner key decrypts, and the payload round-trips.
mkdir "$fixture/open"
tar -C "$fixture/open" -xf "$archive"
gpg --batch --quiet --homedir "$fixture/keys-owner" -d "$fixture/open/surrealdb.surql.zst.gpg" 2>/dev/null \
    | zstd -dq | cmp - "$fixture/run/export.surql"
restored_secret=$(gpg --batch --quiet --homedir "$fixture/keys-owner" -d "$fixture/open/etc-jarvis.tar.zst.gpg" 2>/dev/null \
    | zstd -dq | tar -xOf - etc/jarvis/secrets/provider.env)
[[ $restored_secret == "PROVIDER_KEY=$canary" ]]
manifest=$(gpg --batch --quiet --homedir "$fixture/keys-owner" -d "$fixture/open/manifest.json.gpg" 2>/dev/null)
jq -e --arg sha "$(sha256sum < "$fixture/run/export.surql" | cut -d' ' -f1)" \
    '.format_version == 1 and .export_sha256 == $sha and .tables == [{table: "marker", live_before: 1, live_after: 1, restored: 1}]' \
    <<< "$manifest" >/dev/null
expect_fail gpg --batch --homedir "$fixture/keys-other" -d "$fixture/open/manifest.json.gpg"

# Truncated, byte-flipped and reshaped archives are rejected.
cp -a "$archive" "$fixture/good.tar"
cp -a "$archive.sha256" "$fixture/good.tar.sha256"
truncate -s -100 "$archive"
expect_fail verify "$archive"
cp -a "$fixture/good.tar" "$archive"
printf 'X' | dd of="$archive" bs=1 seek=700 conv=notrunc status=none
expect_fail verify "$archive"
reshape() {
    cp -a "$fixture/good.tar" "$archive"
    "$@"
    (cd "$fixture/dest" && sha256sum -- "${archive##*/}") > "$archive.sha256"
    expect_fail verify "$archive"
}
printf 'x' > "$fixture/open/extra"
reshape tar -C "$fixture/open" -rf "$archive" extra
reshape tar -C "$fixture/open" --transform 's|^extra|../escape|' -rf "$archive" extra
reshape tar -C "$fixture/open" --delete -f "$archive" manifest.json.gpg
cp -a "$fixture/good.tar" "$archive"
{ cat "$fixture/good.tar.sha256"; printf '%s  %s\n' "$(printf '0%.0s' {1..64})" other.tar; } > "$archive.sha256"
expect_fail verify "$archive"
cp -a "$fixture/good.tar.sha256" "$archive.sha256"
verify "$archive" 2>/dev/null
ln -s "$archive" "$fixture/dest/link.tar"
expect_fail verify "$fixture/dest/link.tar"
rm "$fixture/dest/link.tar"

# --- Restore-count comparison ---------------------------------------------------
count_matches 5 5 5
count_matches 5 7 6
count_matches 7 5 5
expect_fail count_matches 5 5 4
expect_fail count_matches 5 7 8

# --- Rotation -------------------------------------------------------------------
for day in 01 02 03 04 05 06 07 08 09; do
    printf 'archive' > "$fixture/dest/jarvis-backup-2026-09-$day.tar"
    printf 'sum' > "$fixture/dest/jarvis-backup-2026-09-$day.tar.sha256"
done
printf 'keep' > "$fixture/dest/notes.txt"
printf 'target' > "$fixture/outside"
ln -s "$fixture/outside" "$fixture/dest/jarvis-backup-2026-01-01.tar"
# A future-dated archive (clock skew, manual copy) sorts first by name but must
# never evict the archive that was just published.
printf 'future' > "$fixture/dest/jarvis-backup-2099-01-01.tar"
mkdir "$fixture/dest/.jarvis-backup.stale"
expect_fail prune "$fixture/dest" ''
expect_fail prune "$fixture/dest" notes.txt
prune "$fixture/dest" jarvis-backup-2026-10-03.tar
remaining=$(find "$fixture/dest" -maxdepth 1 -name 'jarvis-backup-*.tar' -type f -printf '%f\n' | sort | tr '\n' ' ')
# Only the current archive is kept.
[[ $remaining == 'jarvis-backup-2026-10-03.tar ' ]]
[[ ! -e $fixture/dest/jarvis-backup-2099-01-01.tar && ! -e $fixture/dest/jarvis-backup-2026-09-09.tar.sha256 ]]
[[ -e $fixture/dest/jarvis-backup-2026-10-03.tar.sha256 ]]
verify "$archive" 2>/dev/null
[[ -e $fixture/dest/jarvis-backup-2026-10-03.tar && -e $fixture/dest/notes.txt ]]
[[ -L $fixture/dest/jarvis-backup-2026-01-01.tar && $(cat "$fixture/outside") == target ]]
[[ ! -e $fixture/dest/.jarvis-backup.stale ]]

# --- Leftovers of a SIGKILLed run ---------------------------------------------------
# Only exact run directories go; the lock file, symlinks (and their targets)
# and other names stay. Restore containers are removed by the anchored filter.
lock_dir="$fixture/stale"
mkdir -p "$lock_dir/jarvis-backup.AbCd1234" "$lock_dir/jarvis-backup-other" "$fixture/linked-run"
printf 'plaintext' > "$lock_dir/jarvis-backup.AbCd1234/export.surql"
printf 'keep' > "$fixture/linked-run/export.surql"
ln -s "$fixture/linked-run" "$lock_dir/jarvis-backup.Zz99Zz99"
: > "$lock_dir/jarvis-backup.lock"
docker() {
    printf '%s\n' "$*" >> "$fixture/docker.log"
    [[ $1 != ps ]] || printf 'abc123\ndef456\n'
}
remove_stale_runs
[[ ! -e $lock_dir/jarvis-backup.AbCd1234 && -d $lock_dir/jarvis-backup-other && -f $lock_dir/jarvis-backup.lock ]]
[[ -L $lock_dir/jarvis-backup.Zz99Zz99 && $(cat "$fixture/linked-run/export.surql") == keep ]]
[[ $(cat "$fixture/docker.log") == $'ps -aq --filter name=^jarvis-backup-verify-\nrm -f abc123 def456' ]]
# shellcheck disable=SC2317  # invoked through remove_stale_runs
docker() { [[ $1 != ps ]] || return 1; }
expect_fail remove_stale_runs
unset -f docker

# --- Entry point refusals ---------------------------------------------------------
if [[ $EUID != 0 ]]; then
    expect_fail bash "$repo/deploy/systemd/jarvis-backup.sh" create
fi
expect_fail env JARVIS_BACKUP_FIXTURE_ROOT=/tmp/x bash "$repo/deploy/systemd/jarvis-backup.sh" verify "$archive"
usage=$(bash "$repo/deploy/systemd/jarvis-backup.sh" bogus 2>&1) && exit 1
[[ $usage == *usage* && $usage != *"$canary"* ]]

if grep -qF "$canary" <<< "$output"; then echo 'secret printed during backup' >&2; exit 1; fi
echo 'jarvis-backup: config, pinned encryption, verification, rotation and secret hygiene hold'
