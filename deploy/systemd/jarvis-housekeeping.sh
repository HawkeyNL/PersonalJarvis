#!/usr/bin/env bash
# Deterministic, allowlist-based disk housekeeping for the Home Node; see
# docs/HOUSEKEEPING.md. Exactly two kinds of directory are ever deleted:
#   /opt/jarvis/releases/vX.Y.Z               old Core releases
#   /var/lib/jarvis-app-updates/releases/vX.Y.Z  old protected app generations
# Public downloads, the legacy app mirror, Docker and log files are reported
# only. The global journal is never vacuumed: it is shared with other services.
#
#   jarvis-housekeeping status [--json]  read-only plan; takes no lock
#   jarvis-housekeeping apply [--json]   delete what the plan lists (the timer)
#
# Every keep/remove decision is a fixed rule over strict names; there is no
# caller-supplied path or pattern. Each rule fails closed: an unexpected layout
# skips that rule and makes the run fail. apply exits 75 before deleting
# anything when a Core update, backup, app release sync or another
# housekeeping run holds its lock.
set -euo pipefail
shopt -s inherit_errexit
umask 077

readonly keep_releases=3
readonly keep_generations=3
# A generation installed or staged this recently is always kept, so an older
# release staged by hand for --rollback-version survives until it is used.
# Measured by inode change time: tar restores the archive's mtime, but
# neither tar nor the final rename can set a past ctime.
readonly recent_days=7
readonly tag_pattern='v[0-9]+\.[0-9]+\.[0-9]+'

fail() { echo "jarvis housekeeping: $*" >&2; exit 1; }
log() { echo "jarvis housekeeping: $*" >&2; }
busy() { log "$1"; exit 75; }

# --- Inspection helpers (never follow symlinks) ----------------------------

owned_dir() { [[ -d $1 && ! -L $1 && $(stat -c %u -- "$1") == "$owner_uid" ]]; }

# Disk usage in bytes; 0 when unreadable.
size_of() {
    local out
    out=$(du -s -B1 -x -- "$1" 2>/dev/null) || true
    out=${out%%[[:space:]]*}
    if [[ $out =~ ^[0-9]+$ ]]; then printf '%s\n' "$out"; else echo 0; fi
}

# True when tag $1 sorts strictly after tag $2.
newer() { [[ $1 != "$2" && $(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1) == "$1" ]]; }

in_list() { [[ $'\n'$2$'\n' == *$'\n'$1$'\n'* ]]; }

# Real child directories named exactly vMAJOR.MINOR.PATCH, oldest first.
# find -type d never matches a symlink and descends no further.
list_tags() {
    find "$1" -mindepth 1 -maxdepth 1 -type d -regextype posix-extended \
        -regex ".*/$tag_pattern" -printf '%f\n' | sort -V
}

# Final component of every symlink at most two levels below $1. Link targets
# are read, never followed; any link naming a generation pins it.
referenced_tags() {
    local links
    links=$(find "$1" -mindepth 1 -maxdepth 2 -type l -printf '%l\n') || return 1
    sed -E 's#/+$##; s#.*/##' <<<"$links" | grep -Ex "$tag_pattern" || true
}

# Fails safe: an unreadable change time counts as recent.
recent() {
    local changed
    changed=$(stat -c %Z -- "$1" 2>/dev/null) || return 0
    [[ $changed =~ ^[0-9]+$ ]] || return 0
    (( changed > now - recent_days * 86400 ))
}

# --- Rule bookkeeping -------------------------------------------------------

add_rule() { rules=$(jq -c --argjson rule "$1" '. + [$rule]' <<<"$rules"); }

rule_state() { add_rule "$(jq -nc --arg rule "$1" --arg state "$2" '{rule: $rule, state: $state}')"; }

rule_error() {
    add_rule "$(jq -nc --arg rule "$1" --arg reason "$2" '{rule: $rule, state: "error", reason: $reason}')"
    errors=$((errors + 1))
}

# Classify every strictly named generation below $2. Kept: the active one,
# anything newer (staged or not yet activated), the rollback target(s), the
# newest $6, recently installed, symlink-referenced and foreign-owned ones.
# Adds the rule and sets plan_remove (newline-separated tags).
classify() {
    local rule=$1 dir=$2 active=$3 rollback=$4 referenced=$5 keep_count=$6
    local tags newest tag reason bytes keep='[]' remove='[]'
    plan_remove=
    tags=$(list_tags "$dir") || { rule_error "$rule" 'cannot list generations'; return; }
    newest=$(tail -n "$keep_count" <<<"$tags")
    while IFS= read -r tag; do
        [[ -n $tag ]] || continue
        if [[ $tag == "$active" ]]; then reason=active
        elif newer "$tag" "$active"; then reason=newer-than-active
        elif in_list "$tag" "$rollback"; then reason=rollback-target
        elif in_list "$tag" "$newest"; then reason=recent-release
        elif in_list "$tag" "$referenced"; then reason=symlink-referenced
        elif ! owned_dir "$dir/$tag"; then reason=unexpected-owner
        elif recent "$dir/$tag"; then reason=recently-installed
        else reason=
        fi
        if [[ -n $reason ]]; then
            keep=$(jq -c --arg name "$tag" --arg reason "$reason" '. + [{name: $name, reason: $reason}]' <<<"$keep")
        else
            bytes=$(size_of "$dir/$tag")
            remove=$(jq -c --arg name "$tag" --argjson bytes "$bytes" '. + [{name: $name, bytes: $bytes}]' <<<"$remove")
            plan_remove+=$tag$'\n'
        fi
    done <<<"$tags"
    add_rule "$(jq -nc --arg rule "$rule" --argjson keep "$keep" --argjson remove "$remove" \
        '{rule: $rule, state: "ok", keep: $keep, remove: $remove,
          reclaimable_bytes: ($remove | map(.bytes) | add // 0)}')"
}

# --- Rule 1: Core releases --------------------------------------------------

core_active() {
    local target tag
    [[ -L $current_link ]] || return 1
    target=$(readlink -- "$current_link") || return 1
    tag=${target##*/}
    [[ $tag =~ ^$tag_pattern$ && $target == "$releases_dir/$tag" ]] || return 1
    owned_dir "$releases_dir/$tag" || return 1
    printf '%s\n' "$tag"
}

# The active updater's newest-first rollback candidates. The updater takes
# the updater lock itself, so this runs before housekeeping takes it.
rollback_candidates() {
    local updater="$releases_dir/$1/update-core-release"
    [[ -f $updater && ! -L $updater && -x $updater && $(stat -c %u -- "$updater") == "$owner_uid" ]] || return 1
    env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin LANG=C.UTF-8 "$updater" --rollback-candidates
}

plan_core() {
    local mode=$1 active candidates status=0 rollback referenced
    core_remove=
    if [[ ! -e $releases_dir && ! -L $releases_dir ]]; then rule_state core-releases absent; return; fi
    owned_dir "$releases_dir" || { rule_error core-releases 'release directory is unsafe'; return; }
    active=$(core_active) || { rule_error core-releases 'active release link is missing or unsafe'; return; }
    candidates=$(rollback_candidates "$active") || status=$?
    if (( status == 75 )); then
        [[ $mode == status ]] || busy 'a Core update is running'
        rule_state core-releases busy
        return
    fi
    (( status == 0 )) || { rule_error core-releases 'updater rollback candidates are unavailable'; return; }
    rollback=$(jq -r --arg active "$active" '
        if type == "array" and length <= 10000
            and all(.[]; type == "object" and (.version | type) == "string"
                and (.current | type) == "boolean" and (.rollback_capable | type) == "boolean")
            and ([.[] | select(.current)] | length == 1 and .[0].version == $active)
        # `jarvis update --rollback` takes the first rollback-capable entry,
        # but may first migrate a newer legacy release that only lacks its
        # verification marker. Keep all of those down to that first entry.
        then [.[] | select(.current | not)]
            | (map(.rollback_capable) | index(true) // length) as $last
            | .[:$last + 1][]
            | select(.rollback_capable or .reason == "verification marker is missing or invalid")
            | .version
        else error("invalid") end' <<<"$candidates" 2>/dev/null) ||
        { rule_error core-releases 'updater rollback candidates are invalid'; return; }
    if grep -Evxq "$tag_pattern" <<<"$rollback" && [[ -n $rollback ]]; then
        rule_error core-releases 'updater rollback target is invalid'
        return
    fi
    if [[ $mode == apply ]]; then
        exec 9> "$lock_dir/jarvis-updater.lock"
        flock -n 9 || busy 'a Core update or backup is running'
        [[ $(core_active || true) == "$active" ]] || busy 'the active release changed while planning'
    fi
    referenced=$(referenced_tags "$opt_root") || { rule_error core-releases 'cannot inspect release links'; return; }
    classify core-releases "$releases_dir" "$active" "$rollback" "$referenced" "$keep_releases"
    core_remove=$plan_remove
}

# --- Rule 2: protected app-update mirror ------------------------------------

plan_mirror() {
    local mode=$1 lock target active referenced
    mirror_remove=
    if [[ ! -e $mirror_root/releases && ! -L $mirror_root/releases ]]; then rule_state app-update-mirror absent; return; fi
    if ! owned_dir "$mirror_root" || ! owned_dir "$mirror_root/releases"; then
        rule_error app-update-mirror 'mirror directory is unsafe'
        return
    fi
    if [[ $mode == apply ]]; then
        # The release store's own lock: no sync runs while generations go.
        lock="$mirror_root/.sync.lock"
        if [[ -e $lock || -L $lock ]]; then
            # The same checks the release store applies before locking.
            if [[ ! -f $lock || -L $lock || $(stat -c '%u %h' -- "$lock") != "$owner_uid 1" ]] ||
                (( 8#$(stat -c %a -- "$lock") & 8#077 )); then
                rule_error app-update-mirror 'sync lock is unsafe'
                return
            fi
        fi
        exec 8>> "$lock"
        flock -n 8 || busy 'an app release sync is running'
    fi
    target=
    [[ -L $mirror_root/current ]] && target=$(readlink -- "$mirror_root/current")
    active=${target#releases/}
    if [[ $target != "releases/$active" || ! $active =~ ^$tag_pattern$ ]] || ! owned_dir "$mirror_root/releases/$active"; then
        rule_error app-update-mirror 'active generation is missing or unsafe'
        return
    fi
    referenced=$(referenced_tags "$mirror_root") || { rule_error app-update-mirror 'cannot inspect generation links'; return; }
    classify app-update-mirror "$mirror_root/releases" "$active" '' "$referenced" "$keep_generations"
    mirror_remove=$plan_remove
}

# --- Report-only rules ------------------------------------------------------

report_public() {
    local tags count
    if [[ ! -e $public_root/releases ]]; then rule_state public-downloads absent; return; fi
    owned_dir "$public_root/releases" || { rule_error public-downloads 'download directory is unsafe'; return; }
    tags=$(list_tags "$public_root/releases") || { rule_error public-downloads 'cannot list versions'; return; }
    count=$(grep -c . <<<"$tags" || true)
    add_rule "$(jq -nc --argjson versions "$count" --argjson bytes "$(size_of "$public_root/releases")" \
        '{rule: "public-downloads", state: "report-only", versions: $versions, bytes: $bytes,
          note: "public installers follow the owner retention policy of jarvis-app-downloads plan; never deleted here"}')"
}

report_legacy() {
    if [[ ! -e $legacy_root ]]; then rule_state legacy-app-updates absent; return; fi
    add_rule "$(jq -nc --argjson bytes "$(size_of "$legacy_root")" \
        '{rule: "legacy-app-updates", state: "report-only", bytes: $bytes,
          note: "legacy mirror; the owner decides whether to remove it"}')"
}

report_docker() {
    local listing size bytes=0
    local -a ids=()
    if [[ ! -x $docker ]] || ! listing=$(timeout 30 "$docker" image ls --quiet --no-trunc --filter dangling=true 2>/dev/null); then
        rule_state docker unavailable
        return
    fi
    mapfile -t ids < <(grep -E '^sha256:[0-9a-f]{64}$' <<<"$listing" | sort -u || true)
    if (( ${#ids[@]} > 0 )); then
        while read -r size; do
            [[ $size =~ ^[0-9]+$ ]] && bytes=$((bytes + size))
        done < <(timeout 30 "$docker" image inspect --format '{{.Size}}' "${ids[@]}" 2>/dev/null || true)
    fi
    add_rule "$(jq -nc --argjson count "${#ids[@]}" --argjson bytes "$bytes" \
        '{rule: "docker", state: "report-only", dangling_images: $count, bytes: $bytes,
          note: "images on the shared Docker daemon cannot be attributed to Jarvis; never removed"}')"
}

report_logs() {
    local files count bytes=0 file
    files=$(find "$log_root" -mindepth 1 -maxdepth 1 -name 'jarvis*' -printf '%f\n' 2>/dev/null) || true
    count=$(grep -c . <<<"$files" || true)
    while IFS= read -r file; do
        [[ -n $file ]] && bytes=$((bytes + $(size_of "$log_root/$file")))
    done <<<"$files"
    add_rule "$(jq -nc --argjson count "$count" --argjson bytes "$bytes" \
        '{rule: "logs", state: "report-only", jarvis_log_entries: $count, bytes: $bytes,
          note: "Jarvis services log to the shared journal, which is never vacuumed"}')"
}

# --- Deletion ---------------------------------------------------------------

audit() { "$logger" --tag jarvis-housekeeping --priority authpriv.notice -- "$1"; }

# Re-check the planned name immediately before deleting it. Each deletion is
# audited first; an unrecordable audit event deletes nothing.
remove_generation() {
    local rule=$1 dir=$2 tag=$3 bytes
    if [[ ! $tag =~ ^$tag_pattern$ ]] || ! owned_dir "$dir/$tag"; then
        log "$rule: $tag changed since planning; kept"
        return 1
    fi
    bytes=$(size_of "$dir/$tag")
    audit "rule=$rule action=delete name=$tag bytes=$bytes outcome=started" ||
        { log 'audit event could not be recorded; nothing deleted'; return 1; }
    if rm -rf --one-file-system -- "${dir:?}/${tag:?}"; then
        audit "rule=$rule action=delete name=$tag bytes=$bytes outcome=removed" || true
        reclaimed=$((reclaimed + bytes))
        removed=$((removed + 1))
        log "$rule: removed $tag"
    else
        audit "rule=$rule action=delete name=$tag bytes=$bytes outcome=failed" || true
        return 1
    fi
}

remove_all() {
    local rule=$1 dir=$2 list=$3 tag
    while IFS= read -r tag; do
        [[ -n $tag ]] || continue
        remove_generation "$rule" "$dir" "$tag" || errors=$((errors + 1))
    done <<<"$list"
}

# --- Report -----------------------------------------------------------------

disk_json() {
    local avail total
    read -r avail total < <(df --output=avail,size -B1 -- "$disk_path" | tail -n 1)
    jq -nc --argjson free "$avail" --argjson total "$total" '{path: "/", free_bytes: $free, total_bytes: $total}'
}

last_run_json() {
    local file="$state_dir/last-run.json"
    if [[ -f $file && ! -L $file && $(stat -c %s -- "$file") -le 65536 ]] &&
        jq -ce 'select(type == "object" and .format_version == 1)' "$file" 2>/dev/null; then
        return
    fi
    echo null
}

write_state() {
    local disk=$1 temporary outcome=ok
    (( errors == 0 )) || outcome=error
    [[ -e $state_dir ]] || install -d -m 0755 "$state_dir"
    owned_dir "$state_dir" || fail 'state directory is unsafe'
    temporary=$(mktemp "$state_dir/.last-run.XXXXXXXX")
    jq -n --arg at "$(date -u +%FT%TZ)" --arg outcome "$outcome" \
        --argjson removed "$removed" --argjson reclaimed "$reclaimed" --argjson disk "$disk" \
        '{format_version: 1, finished_at: $at, outcome: $outcome, removed: $removed,
          reclaimed_bytes: $reclaimed, disk_free_bytes: $disk.free_bytes, disk_total_bytes: $disk.total_bytes}' > "$temporary"
    chmod 0644 "$temporary"
    mv -Tf -- "$temporary" "$state_dir/last-run.json"
}

print_report() {
    local mode=dry-run disk=$2 report
    [[ $1 != apply ]] || mode=apply
    report=$(jq -nc --arg mode "$mode" --arg at "$(date -u +%FT%TZ)" --argjson disk "$disk" \
        --argjson rules "$rules" --argjson reclaimed "$reclaimed" --argjson last "$(last_run_json)" \
        '{format_version: 1, mode: $mode, generated_at: $at, disk: $disk, rules: $rules,
          reclaimable_bytes: ([$rules[] | .reclaimable_bytes // 0] | add // 0),
          reclaimed_bytes: $reclaimed, last_run: $last}')
    if [[ $json == true ]]; then
        printf '%s\n' "$report"
        return
    fi
    jq -r '
        def size: if . >= 1073741824 then "\(. / 1073741824 * 10 | floor / 10) GiB"
            elif . >= 1048576 then "\(. / 1048576 * 10 | floor / 10) MiB"
            elif . >= 1024 then "\(. / 1024 | floor) KiB" else "\(.) B" end;
        (if .mode == "apply" then "Jarvis housekeeping (applied)"
         else "Jarvis housekeeping (dry run: nothing was deleted)" end),
        "  Disk free: \(.disk.free_bytes | size) of \(.disk.total_bytes | size)",
        (.rules[] | "  \(.rule): \(.state)"
            + (if .reason then " (\(.reason))" else "" end)
            + (if .bytes != null then ", \(.bytes | size)" else "" end),
          (.remove[]? | "    remove \(.name) (\(.bytes | size))"),
          (.keep[]? | "    keep   \(.name) (\(.reason))"),
          (.note // empty | "    \(.)")),
        (if .mode == "apply" then "  Reclaimed: \(.reclaimed_bytes | size)"
         else "  Reclaimable: \(.reclaimable_bytes | size)" end),
        (.last_run // empty | "  Last run: \(.finished_at) (\(.outcome)), reclaimed \(.reclaimed_bytes | size)")
    ' <<<"$report"
}

run() {
    local mode=$1 disk
    rules='[]' errors=0 reclaimed=0 removed=0
    if [[ $mode == apply ]]; then
        exec 7> "$lock_dir/jarvis-housekeeping.lock"
        flock -n 7 || busy 'another housekeeping run is active'
    fi
    # Reports first, so the updater and sync locks are held only for
    # planning and deleting. All those locks are taken while planning, so
    # every busy exit happens before the first deletion.
    report_public
    report_legacy
    report_docker
    report_logs
    plan_core "$mode"
    plan_mirror "$mode"
    if [[ $mode == apply ]]; then
        remove_all core-releases "$releases_dir" "$core_remove"
        remove_all app-update-mirror "$mirror_root/releases" "$mirror_remove"
    fi
    disk=$(disk_json)
    [[ $mode != apply ]] || write_state "$disk"
    print_report "$mode" "$disk"
    (( errors == 0 )) || exit 1
}

main() {
    opt_root=/opt/jarvis
    lock_dir=/run
    mirror_root=/var/lib/jarvis-app-updates
    public_root=/var/lib/jarvis-public-downloads
    legacy_root=/var/lib/jarvis/app-updates
    log_root=/var/log
    state_dir=/var/lib/jarvis-housekeeping
    disk_path=/
    logger=/usr/bin/logger
    docker=$(command -v docker || true)
    owner_uid=0
    now=$(date +%s)
    if [[ -n ${JARVIS_HOUSEKEEPING_FIXTURE_ROOT:-} ]]; then
        [[ ${GITHUB_ACTIONS:-} == true && ${JARVIS_HOUSEKEEPING_TEST_MODE:-} == true && \
            $JARVIS_HOUSEKEEPING_FIXTURE_ROOT == /tmp/* ]] || fail 'test-only override refused'
        local root=$JARVIS_HOUSEKEEPING_FIXTURE_ROOT
        [[ $(realpath -e -- "$root") == "$root" ]] || fail 'test root must be canonical'
        opt_root=$root/opt/jarvis
        lock_dir=$root/run
        mirror_root=$root/var/lib/jarvis-app-updates
        public_root=$root/var/lib/jarvis-public-downloads
        legacy_root=$root/var/lib/jarvis/app-updates
        log_root=$root/var/log
        state_dir=$root/var/lib/jarvis-housekeeping
        disk_path=$root
        logger=$root/bin/logger
        docker=$root/bin/docker
        owner_uid=$(id -u)
        if [[ -n ${JARVIS_HOUSEKEEPING_TEST_NOW:-} ]]; then
            [[ $JARVIS_HOUSEKEEPING_TEST_NOW =~ ^[0-9]+$ ]] || fail 'invalid test clock'
            now=$JARVIS_HOUSEKEEPING_TEST_NOW
        fi
    elif [[ $EUID != 0 ]]; then
        fail 'root required (use: sudo jarvis housekeeping status)'
    fi
    releases_dir=$opt_root/releases
    current_link=$opt_root/current
    local tool
    for tool in jq flock find du df stat sort sed grep readlink timeout; do
        command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
    done
    json=false
    case $# in
        1) ;;
        2) [[ $2 == --json ]] || set -- usage; json=true ;;
        *) set -- usage ;;
    esac
    case $1 in
        status|apply) run "$1" ;;
        *) fail 'usage: jarvis-housekeeping status|apply [--json]' ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
