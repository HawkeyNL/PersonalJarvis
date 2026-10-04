#!/usr/bin/env bash
# Rootless fixture tests for jarvis-housekeeping: keep rules, strict names,
# symlink tricks, fail-closed rules, busy locks (exit 75), audit-before-delete
# and a dry run that changes nothing. No root, Docker or systemd is involved.
set -euo pipefail
shopt -s inherit_errexit
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
script="$repo/deploy/systemd/jarvis-housekeeping.sh"
fx=$(mktemp -d /tmp/jarvis-housekeeping-test.XXXXXXXX)
trap 'rm -rf -- "$fx"' EXIT
root=$fx/root

fail() { echo "FAIL: $*" >&2; exit 1; }

generation() {
    mkdir -p "$1"
    head -c 8192 /dev/zero > "$1/payload"
    touch -d '30 days ago' "$1"
}

build() {
    rm -rf -- "$root"
    mkdir -p "$root"/{run,bin,var/log,outside/target,outside/precious} \
        "$root/var/lib/jarvis/app-updates" "$root/var/lib/jarvis-public-downloads/releases" \
        "$root/var/lib/jarvis-housekeeping"
    printf 'keep me\n' > "$root/outside/precious/file"
    printf 'keep me\n' > "$root/outside/target/file"
    local releases=$root/opt/jarvis/releases tag
    for tag in v0.0.1 v0.0.2 v0.0.3 v0.0.4 v0.0.5 v0.0.6 v0.0.10 \
        v0.0.3.old V0.0.2 v1.2 backup-v0.0.1 .staging.abcdefgh; do
        generation "$releases/$tag"
    done
    # Never followed: a tag-named symlink and a link inside a deleted release.
    ln -s "$root/outside/target" "$releases/v0.0.7"
    ln -s "$root/outside/precious" "$releases/v0.0.3/escape"
    touch -h -d '30 days ago' "$releases/v0.0.3"
    ln -s "$releases/v0.0.6" "$root/opt/jarvis/current"
    ln -s releases/v0.0.2 "$root/opt/jarvis/pinned"
    cat > "$releases/v0.0.6/update-core-release" <<'EOF'
#!/usr/bin/env bash
[[ $1 == --rollback-candidates ]] || exit 64
fixture=$(cd -- "$(dirname -- "$0")/../../../.." && pwd)
cat "$fixture/candidates.json"
exit "$(cat "$fixture/updater.status")"
EOF
    chmod 0755 "$releases/v0.0.6/update-core-release"
    touch -d '30 days ago' "$releases/v0.0.6"
    printf '0\n' > "$root/updater.status"
    jq -n '[{version: "v0.0.10", current: false, verified: true, rollback_capable: false, reason: "schema"},
            {version: "v0.0.6", current: true, verified: true, rollback_capable: false, reason: "active release"},
            {version: "v0.0.5", current: false, verified: false, rollback_capable: false, reason: "marker"},
            {version: "v0.0.4", current: false, verified: false, rollback_capable: false, reason: "marker"},
            {version: "v0.0.3", current: false, verified: false, rollback_capable: false, reason: "marker"},
            {version: "v0.0.2", current: false, verified: false, rollback_capable: false, reason: "marker"},
            {version: "v0.0.1", current: false, verified: true, rollback_capable: true, reason: "eligible"},
            {version: "v0.0.0", current: false, verified: false, rollback_capable: false, reason: "marker"}]' \
        > "$root/candidates.json"

    local mirror=$root/var/lib/jarvis-app-updates
    for tag in v1.0.0 v1.0.1 v1.0.2 v1.0.3 v1.0.4 v1.0.5; do
        generation "$mirror/releases/$tag"
        generation "$root/var/lib/jarvis-public-downloads/releases/$tag"
    done
    generation "$mirror/.release-staging-abcdef"
    ln -s "$root/outside/target" "$mirror/releases/v0.9.0"
    ln -s releases/v1.0.5 "$mirror/current"
    printf 'legacy\n' > "$root/var/lib/jarvis/app-updates/latest.json"
    printf 'old\n' > "$root/var/log/jarvis-old.log"
    # Pre-created so a snapshot is not disturbed by lock opens.
    : > "$root/run/jarvis-housekeeping.lock"
    : > "$root/run/jarvis-updater.lock"
    : > "$mirror/.sync.lock"
    chmod 0600 "$mirror/.sync.lock"

    cat > "$root/bin/logger" <<EOF
#!/usr/bin/env bash
[[ ! -e $root/logger.fail ]] || exit 1
printf '%s\n' "\$*" >> "$root/audit.log"
EOF
    cat > "$root/bin/docker" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
    'image ls') printf 'sha256:%064d\nnot-an-id\n' 0 ;;
    'image inspect') echo 1048576 ;;
    *) exit 1 ;;
esac
EOF
    chmod 0755 "$root/bin/logger" "$root/bin/docker"

    # "Recent" is measured by inode change time, which cannot be backdated.
    # The helper's clock is set 7 days and one second after everything above
    # was created; v0.0.0 is staged afterwards with a tar-like old mtime and
    # is therefore recent although it is old by version and by mtime.
    test_now=$(( $(date +%s) + 7 * 86400 + 1 ))
    sleep 2
    generation "$releases/v0.0.0"
}

# shellcheck disable=SC2317,SC2329  # invoked through expect_exit
hk() {
    env GITHUB_ACTIONS=true JARVIS_HOUSEKEEPING_TEST_MODE=true JARVIS_HOUSEKEEPING_TEST_NOW="$test_now" \
        JARVIS_HOUSEKEEPING_FIXTURE_ROOT="$root" bash "$script" "$@"
}

expect_exit() {
    local want=$1 got=0
    shift
    "$@" > "$fx/out" 2> "$fx/err" || got=$?
    [[ $got == "$want" ]] || { cat "$fx/err" >&2; fail "expected exit $want, got $got: $*"; }
}

# Everything except lock files and the fixture's own audit log.
snapshot() { find "$root" ! -name '*.lock' ! -name audit.log -printf '%P %y %s %T@ %l\n' | LC_ALL=C sort; }

names() { find "$1" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' '; }

readonly core_all='.staging.abcdefgh V0.0.2 backup-v0.0.1 v0.0.0 v0.0.1 v0.0.10 v0.0.2 v0.0.3 v0.0.3.old v0.0.4 v0.0.5 v0.0.6 v0.0.7 v1.2 '
readonly core_kept='.staging.abcdefgh V0.0.2 backup-v0.0.1 v0.0.0 v0.0.1 v0.0.10 v0.0.2 v0.0.3.old v0.0.5 v0.0.6 v0.0.7 v1.2 '
readonly mirror_kept='v0.9.0 v1.0.3 v1.0.4 v1.0.5 '
readonly mirror_all='v0.9.0 v1.0.0 v1.0.1 v1.0.2 v1.0.3 v1.0.4 v1.0.5 '

# --- The test-only override is refused outside the explicit test mode --------
build
expect_exit 1 env -u GITHUB_ACTIONS JARVIS_HOUSEKEEPING_TEST_MODE=true \
    JARVIS_HOUSEKEEPING_FIXTURE_ROOT="$root" bash "$script" status
grep -Fq 'test-only override refused' "$fx/err" || fail 'override refusal reason'
expect_exit 1 hk status --yes
expect_exit 1 hk prune

# --- Dry run: exact plan, nothing changes -------------------------------------
before=$(snapshot)
expect_exit 0 hk status --json
[[ $(snapshot) == "$before" ]] || fail 'status changed the filesystem'
plan=$(cat "$fx/out")
jq -e '.mode == "dry-run" and .last_run == null and .reclaimed_bytes == 0 and .reclaimable_bytes > 0' <<<"$plan" >/dev/null ||
    fail "dry-run summary: $plan"
jq -e '.rules[] | select(.rule == "core-releases") | [.remove[].name] == ["v0.0.3", "v0.0.4"]' <<<"$plan" >/dev/null ||
    fail "core removal plan: $plan"
jq -e '.rules[] | select(.rule == "core-releases") | (.keep | map({(.name): .reason}) | add) ==
    {"v0.0.0": "recently-installed", "v0.0.1": "rollback-target", "v0.0.2": "symlink-referenced",
     "v0.0.5": "recent-release", "v0.0.6": "active", "v0.0.10": "newer-than-active"}' <<<"$plan" >/dev/null ||
    fail "core keep reasons: $plan"
jq -e '.rules[] | select(.rule == "app-update-mirror") | [.remove[].name] == ["v1.0.0", "v1.0.1", "v1.0.2"]
    and (.keep | map({(.name): .reason}) | add) == {"v1.0.3": "recent-release", "v1.0.4": "recent-release", "v1.0.5": "active"}' \
    <<<"$plan" >/dev/null || fail "mirror plan: $plan"
jq -e '[.rules[] | select(.state == "report-only") | .rule] == ["public-downloads", "legacy-app-updates", "docker", "logs"]' \
    <<<"$plan" >/dev/null || fail "report-only rules: $plan"
jq -e '.rules[] | select(.rule == "public-downloads") | .versions == 6' <<<"$plan" >/dev/null || fail 'public count'
jq -e '.rules[] | select(.rule == "docker") | .dangling_images == 1 and .bytes == 1048576' <<<"$plan" >/dev/null || fail 'docker report'
jq -e '.rules[] | select(.rule == "logs") | .jarvis_log_entries == 1' <<<"$plan" >/dev/null || fail 'log report'
expect_exit 0 hk status
grep -Fq 'dry run: nothing was deleted' "$fx/out" || fail 'human dry-run report'
grep -Fq 'remove v0.0.3' "$fx/out" || fail 'human report lists removals'
[[ $(snapshot) == "$before" ]] || fail 'text status changed the filesystem'

# --- Busy: every lock holder makes apply exit 75 before deleting anything ----
for lock in "$root/run/jarvis-housekeeping.lock" "$root/run/jarvis-updater.lock" \
    "$root/var/lib/jarvis-app-updates/.sync.lock"; do
    exec 6>> "$lock"
    flock -n 6
    expect_exit 75 hk apply
    exec 6>&-
    [[ $(snapshot) == "$before" ]] || fail "busy run changed the filesystem: $lock"
done
printf '75\n' > "$root/updater.status"
before=$(snapshot)
expect_exit 75 hk apply
[[ $(snapshot) == "$before" ]] || fail 'busy updater run changed the filesystem'
# status reports a busy updater instead of failing.
expect_exit 0 hk status --json
jq -e '.rules[] | select(.rule == "core-releases") == {rule: "core-releases", state: "busy"}' "$fx/out" >/dev/null ||
    fail 'busy status'
[[ ! -e $root/audit.log ]] || fail 'busy runs audited a deletion'

# --- A legacy release the updater would migrate before rolling back is kept --
build
jq 'map(if .version == "v0.0.4" then .reason = "verification marker is missing or invalid" else . end)' \
    "$root/candidates.json" > "$fx/c" && cp "$fx/c" "$root/candidates.json"
expect_exit 0 hk status --json
jq -e '.rules[] | select(.rule == "core-releases") | [.remove[].name] == ["v0.0.3"]
    and any(.keep[]; . == {name: "v0.0.4", reason: "rollback-target"})' "$fx/out" >/dev/null ||
    fail "legacy rollback candidate: $(cat "$fx/out")"
# Unknown state of the clock never deletes: without a valid change time the
# rule keeps the release (covered by recent()); a bad test clock is refused.
expect_exit 1 env GITHUB_ACTIONS=true JARVIS_HOUSEKEEPING_TEST_MODE=true JARVIS_HOUSEKEEPING_TEST_NOW=soon \
    JARVIS_HOUSEKEEPING_FIXTURE_ROOT="$root" bash "$script" status

# --- An unrecordable audit event deletes nothing --------------------------------
build
touch "$root/logger.fail"
before=$(snapshot)
expect_exit 1 hk apply
[[ $(snapshot | grep -v '^var/lib/jarvis-housekeeping') == "$(grep -v '^var/lib/jarvis-housekeeping' <<<"$before")" ]] ||
    fail 'deleted without an audit record'
jq -e '.outcome == "error" and .removed == 0' "$root/var/lib/jarvis-housekeeping/last-run.json" >/dev/null || fail 'error state'

# --- Each rule fails closed on its own -------------------------------------------
build
printf '1\n' > "$root/updater.status"
expect_exit 1 hk apply --json
[[ $(names "$root/opt/jarvis/releases") == "$core_all" ]] || fail 'core deleted without updater candidates'
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_kept" ]] || fail 'mirror rule did not run independently'

build
jq 'map(.current = (.version == "v0.0.5"))' "$root/candidates.json" > "$fx/c" && cp "$fx/c" "$root/candidates.json"
expect_exit 1 hk apply
[[ $(names "$root/opt/jarvis/releases") == "$core_all" ]] || fail 'core deleted with mismatched candidates'

build
printf 'not json' > "$root/candidates.json"
expect_exit 1 hk apply
[[ $(names "$root/opt/jarvis/releases") == "$core_all" ]] || fail 'core deleted with malformed candidates'

build
ln -sfn "$root/outside/target" "$root/opt/jarvis/current"
expect_exit 1 hk apply
[[ $(names "$root/opt/jarvis/releases") == "$core_all" ]] || fail 'core deleted with an unsafe current link'

build
ln -sfn ../opt/jarvis/releases/v0.0.6 "$root/opt/jarvis/current"
expect_exit 1 hk apply
[[ $(names "$root/opt/jarvis/releases") == "$core_all" ]] || fail 'core deleted with a non-canonical current link'

build
rm -- "$root/var/lib/jarvis-app-updates/current"
expect_exit 1 hk apply
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_all" ]] || fail 'mirror deleted without an active generation'

build
ln -sfn "$root/var/lib/jarvis-app-updates/releases/v1.0.5" "$root/var/lib/jarvis-app-updates/current"
expect_exit 1 hk apply
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_all" ]] || fail 'mirror deleted with an absolute current link'

build
chmod 0644 "$root/var/lib/jarvis-app-updates/.sync.lock"
expect_exit 1 hk apply
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_all" ]] || fail 'mirror deleted with a readable sync lock'

build
rm -- "$root/var/lib/jarvis-app-updates/.sync.lock"
ln -s "$root/outside/precious/file" "$root/var/lib/jarvis-app-updates/.sync.lock"
expect_exit 1 hk apply
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_all" ]] || fail 'mirror deleted with a symlinked sync lock'
[[ $(cat "$root/outside/precious/file") == 'keep me' ]] || fail 'sync lock symlink was followed'

# --- Apply: only the planned generations go; everything else is intact ------------
build
public_before=$(snapshot | grep '^var/lib/jarvis-public-downloads')
expect_exit 0 hk apply --json
result=$(cat "$fx/out")
[[ $(names "$root/opt/jarvis/releases") == "$core_kept" ]] || fail "core after apply: $(names "$root/opt/jarvis/releases")"
[[ $(names "$root/var/lib/jarvis-app-updates/releases") == "$mirror_kept" ]] || fail 'mirror after apply'
[[ -d $root/var/lib/jarvis-app-updates/.release-staging-abcdef ]] || fail 'mirror staging removed'
[[ $(snapshot | grep '^var/lib/jarvis-public-downloads') == "$public_before" ]] || fail 'public downloads changed'
[[ -f $root/var/lib/jarvis/app-updates/latest.json && -f $root/var/log/jarvis-old.log ]] || fail 'report-only data changed'
[[ $(cat "$root/outside/precious/file") == 'keep me' && $(cat "$root/outside/target/file") == 'keep me' ]] ||
    fail 'a symlink target was followed'
[[ -L $root/opt/jarvis/releases/v0.0.7 && -L $root/var/lib/jarvis-app-updates/releases/v0.9.0 ]] || fail 'tag-named symlinks removed'
jq -e '.mode == "apply" and .reclaimed_bytes > 0' <<<"$result" >/dev/null || fail "apply summary: $result"
for tag in v0.0.3 v0.0.4; do
    grep -Eq "^--tag jarvis-housekeeping --priority authpriv.notice -- rule=core-releases action=delete name=$tag bytes=[0-9]+ outcome=started$" \
        "$root/audit.log" || fail "audit start for $tag"
    grep -Eq "rule=core-releases action=delete name=$tag bytes=[0-9]+ outcome=removed$" "$root/audit.log" || fail "audit result for $tag"
done
[[ $(grep -c 'outcome=removed$' "$root/audit.log") == 5 ]] || fail 'unexpected audited deletions'
state=$root/var/lib/jarvis-housekeeping/last-run.json
[[ -f $state && ! -L $state && $(stat -c %a "$state") == 644 ]] || fail 'state file mode'
jq -e '.format_version == 1 and .outcome == "ok" and .removed == 5 and .reclaimed_bytes > 0 and .disk_free_bytes > 0' \
    "$state" >/dev/null || fail 'state file content'

# A second run has nothing left to remove and reports the last run.
expect_exit 0 hk apply --json
jq -e '.reclaimed_bytes == 0 and ([.rules[] | .remove[]?] | length) == 0 and .last_run.removed == 0' "$fx/out" >/dev/null ||
    fail 'second run removed something'
expect_exit 0 hk status
grep -Fq 'Last run:' "$fx/out" || fail 'status omits the last run'

echo 'jarvis-housekeeping fixture tests passed'
