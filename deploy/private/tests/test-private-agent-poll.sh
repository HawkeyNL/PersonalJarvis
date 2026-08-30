#!/usr/bin/env bash
# Root-only synthetic transaction fixture. No private owner content is read.
set -euo pipefail

[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || {
    echo "CI root fixture only" >&2
    exit 1
}

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
poller=$repo_dir/deploy/private/jarvis-private-agent-poll.sh
fixture=$(mktemp -d)
fake_bin=$fixture/bin
source_root=/var/lib/jarvis/agents-source
mkdir -p "$fake_bin" "$source_root/.git"

cleanup() {
    rm -rf -- "$fixture" /var/lib/jarvis/agents /var/lib/jarvis/agents-source
    rm -f -- /etc/jarvis/private-agent-updater.env /usr/local/sbin/jarvis-private-update \
        /run/jarvis-private-agent-update.lock
}
trap cleanup EXIT

install -d -o root -g root -m 0750 /etc/jarvis
cat > /etc/jarvis/private-agent-updater.env <<EOF
JARVIS_PRIVATE_AGENT_SOURCE=$source_root
JARVIS_PRIVATE_AGENT_REPOSITORY=HawkeyNL/PersonalJarvisAgents
EOF
chown root:root /etc/jarvis/private-agent-updater.env
chmod 0600 /etc/jarvis/private-agent-updater.env

printf '%040d\n' 1 > "$fixture/current-revision"
printf '%040d\n' 2 > "$fixture/remote-revision"

cat > "$fake_bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
fixture=$JARVIS_PRIVATE_AGENT_TEST_FIXTURE
[[ ${1:-} == -C && ${2:-} == /var/lib/jarvis/agents-source ]] || exit 1
shift 2
case "$*" in
    'remote get-url origin') printf '%s\n' 'https://github.com/HawkeyNL/PersonalJarvisAgents.git' ;;
    'fetch --quiet origin refs/heads/main') printf 'fetch\n' >> "$fixture/git-events" ;;
    'rev-parse FETCH_HEAD') cat "$fixture/remote-revision" ;;
    'rev-parse HEAD') cat "$fixture/current-revision" ;;
    merge\ --ff-only\ *)
        cp "$fixture/remote-revision" "$fixture/current-revision"
        printf 'merge\n' >> "$fixture/git-events"
        ;;
    *) exit 1 ;;
esac
EOF
chmod 0755 "$fake_bin/git"

cat > "$fake_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == 'restart jarvis-core.service' ]] || exit 1
printf 'restart\n' >> "$JARVIS_PRIVATE_AGENT_TEST_FIXTURE/core-events"
EOF
chmod 0755 "$fake_bin/systemctl"

cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
fixture=$JARVIS_PRIVATE_AGENT_TEST_FIXTURE
[[ ${*: -1} == http://127.0.0.1:8080/readyz ]] || exit 1
count=$(wc -l < "$fixture/ready-events" 2>/dev/null || printf '0')
printf 'ready\n' >> "$fixture/ready-events"
if [[ ${JARVIS_PRIVATE_AGENT_READY_FAIL_ONCE:-false} == true && $count -eq 0 ]]; then
    exit 1
fi
EOF
chmod 0755 "$fake_bin/curl"

install -d -o root -g root -m 0755 /usr/local/sbin
cat > /usr/local/sbin/jarvis-private-update <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ ${1:-} == --source && ${2:-} == /var/lib/jarvis/agents-source && $# -eq 2 ]] || exit 1
fixture=$JARVIS_PRIVATE_AGENT_TEST_FIXTURE
bundle=$JARVIS_PRIVATE_AGENT_NEXT_BUNDLE
root=/var/lib/jarvis/agents
mkdir -p "$root/releases/$bundle"
printf '{"version":1,"bundle_id":"%s","agents":[{"id":"fixture"}]}\n' "$bundle" \
    > "$root/releases/$bundle/manifest.json"
rm -f -- "$root/.current.new"
ln -s "releases/$bundle" "$root/.current.new"
mv -Tf "$root/.current.new" "$root/current"
printf 'update:%s\n' "$bundle" >> "$fixture/update-events"
EOF
chmod 0755 /usr/local/sbin/jarvis-private-update

seed_bundle() {
    local bundle=$1
    rm -rf -- /var/lib/jarvis/agents
    mkdir -p "/var/lib/jarvis/agents/releases/$bundle"
    printf '{"version":1,"bundle_id":"%s","agents":[{"id":"fixture"}]}\n' "$bundle" \
        > "/var/lib/jarvis/agents/releases/$bundle/manifest.json"
    ln -s "releases/$bundle" /var/lib/jarvis/agents/current
    printf '%s\n' "$bundle" > /var/lib/jarvis/agents/core-loaded-bundle
    chown root:root /var/lib/jarvis/agents/core-loaded-bundle
    chmod 0644 /var/lib/jarvis/agents/core-loaded-bundle
    : > "$fixture/git-events"
    : > "$fixture/update-events"
    : > "$fixture/core-events"
    : > "$fixture/ready-events"
}

run_poll() {
    PATH="$fake_bin:/usr/sbin:/usr/bin:/sbin:/bin" \
        JARVIS_PRIVATE_AGENT_TEST_FIXTURE="$fixture" \
        JARVIS_PRIVATE_AGENT_NEXT_BUNDLE="${JARVIS_PRIVATE_AGENT_NEXT_BUNDLE:-bundle-new}" \
        JARVIS_PRIVATE_AGENT_READY_FAIL_ONCE="${JARVIS_PRIVATE_AGENT_READY_FAIL_ONCE:-false}" \
        bash "$poller" "$@"
}

# Check fetches remote state but never merges, builds, activates, or restarts.
seed_bundle bundle-old
check_output=$(run_poll --check)
grep -Fq 'Update:  available' <<< "$check_output"
[[ ! -s $fixture/update-events && ! -s $fixture/core-events ]]
[[ $(readlink /var/lib/jarvis/agents/current) == releases/bundle-old ]]
[[ $(cat "$fixture/current-revision") != "$(cat "$fixture/remote-revision")" ]]

# Update fast-forwards, activates the validated bundle, restarts Core, and
# records which exact bundle passed readiness.
JARVIS_PRIVATE_AGENT_NEXT_BUNDLE=bundle-new run_poll
grep -qx merge "$fixture/git-events"
grep -qx update:bundle-new "$fixture/update-events"
grep -qx restart "$fixture/core-events"
[[ $(cat /var/lib/jarvis/agents/core-loaded-bundle) == bundle-new ]]

# An unchanged source is still passed through the versioned bundler, allowing
# a new safe manifest schema to create and load a different immutable bundle.
seed_bundle bundle-new
cp "$fixture/remote-revision" "$fixture/current-revision"
JARVIS_PRIVATE_AGENT_NEXT_BUNDLE=bundle-schema run_poll
[[ ! -s $fixture/git-events || $(grep -c '^merge$' "$fixture/git-events") == 0 ]]
grep -qx update:bundle-schema "$fixture/update-events"
[[ $(cat /var/lib/jarvis/agents/core-loaded-bundle) == bundle-schema ]]

# Failed Core readiness restores the previous symlink and confirms that the
# restored bundle itself starts before returning failure.
seed_bundle bundle-stable
cp "$fixture/remote-revision" "$fixture/current-revision"
if JARVIS_PRIVATE_AGENT_NEXT_BUNDLE=bundle-bad \
    JARVIS_PRIVATE_AGENT_READY_FAIL_ONCE=true run_poll; then
    echo "failed agent bundle unexpectedly remained active" >&2
    exit 1
fi
[[ $(readlink /var/lib/jarvis/agents/current) == releases/bundle-stable ]]
[[ $(cat /var/lib/jarvis/agents/core-loaded-bundle) == bundle-stable ]]
[[ $(grep -c '^restart$' "$fixture/core-events") == 2 ]]
[[ $(grep -c '^ready$' "$fixture/ready-events") == 2 ]]

echo "Private agent poll transaction fixture tests passed"
