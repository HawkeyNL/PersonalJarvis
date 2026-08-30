#!/usr/bin/env bash
# Root-only polling updater for the separately cloned private agent repository.
# Core, sandbox workloads and Codex never receive this checkout or its GitHub
# credential. Only the configured repository's origin/main is eligible.
set -euo pipefail

readonly config=/etc/jarvis/private-agent-updater.env
readonly agent_root=/var/lib/jarvis/agents
readonly releases_dir=$agent_root/releases
readonly current_link=$agent_root/current
readonly loaded_marker=$agent_root/core-loaded-bundle
readonly update_lock=/run/jarvis-private-agent-update.lock
readonly private_update=/usr/local/sbin/jarvis-private-update

fail() {
    echo "jarvis private agents: $*" >&2
    exit 1
}

usage() {
    echo "Usage: private-agent-poll [--check]" >&2
    exit 64
}

mode=update
case $#:${1:-} in
    0:) ;;
    1:--check) mode=check ;;
    *) usage ;;
esac

[[ ${EUID} -eq 0 && -f $config && ! -L $config ]] || fail "trusted updater configuration is unavailable"
[[ $(stat -c '%U:%G:%a' "$config") == root:root:600 ]] || fail "trusted updater configuration is unsafe"
source_root=
repository=
while IFS= read -r config_line || [[ -n $config_line ]]; do
    case $config_line in
        JARVIS_PRIVATE_AGENT_SOURCE=*)
            [[ -z $source_root ]] || fail "trusted updater configuration contains duplicate source"
            source_root=${config_line#*=}
            ;;
        JARVIS_PRIVATE_AGENT_REPOSITORY=*)
            [[ -z $repository ]] || fail "trusted updater configuration contains duplicate repository"
            repository=${config_line#*=}
            ;;
        '') ;;
        *) fail "trusted updater configuration contains an unsupported entry" ;;
    esac
done < "$config"
[[ $source_root == /* && $source_root != *[!A-Za-z0-9_./-]* && \
    $repository == HawkeyNL/PersonalJarvisAgents ]] || fail "trusted updater configuration is invalid"
source_root=$(realpath -e -- "$source_root") || fail "private agent checkout is unavailable"
[[ -d $source_root/.git && ! -L $source_root/.git ]] || fail "private agent checkout is unavailable"
origin=$(git -C "$source_root" remote get-url origin)
case $origin in
    https://github.com/HawkeyNL/PersonalJarvisAgents|\
    https://github.com/HawkeyNL/PersonalJarvisAgents.git|\
    git@github.com:HawkeyNL/PersonalJarvisAgents|\
    git@github.com:HawkeyNL/PersonalJarvisAgents.git) ;;
    *) fail "private agent origin is not allowlisted" ;;
esac
if [[ -e $loaded_marker || -L $loaded_marker ]]; then
    [[ -f $loaded_marker && ! -L $loaded_marker && \
        $(stat -c '%U:%G:%a' "$loaded_marker") == root:root:644 ]] || \
        fail "Core-loaded agent bundle marker is unsafe"
fi

exec 9>"$update_lock"
flock -n 9 || fail "another private agent operation is running"
git -C "$source_root" fetch --quiet origin refs/heads/main || fail "could not fetch private agent origin/main"
remote=$(git -C "$source_root" rev-parse FETCH_HEAD)
current=$(git -C "$source_root" rev-parse HEAD)
[[ $remote =~ ^[0-9a-f]{40}$ && $current =~ ^[0-9a-f]{40}$ ]] || fail "private agent revision is invalid"

if [[ $mode == check ]]; then
    printf 'Current: %.12s\nLatest:  %.12s\nUpdate:  %s\n' \
        "$current" "$remote" "$( [[ $current == "$remote" ]] && printf up-to-date || printf available )"
    exit 0
fi

if [[ $remote != "$current" ]]; then
    git -C "$source_root" merge --ff-only "$remote" || fail "private agent checkout cannot fast-forward"
fi

active_bundle() {
    local target
    target=$(readlink -f "$current_link" 2>/dev/null || true)
    [[ $target == "$releases_dir"/bundle-* && -d $target && ! -L $target && \
        -f $target/manifest.json && ! -L $target/manifest.json ]] || return 1
    printf '%s\n' "${target##*/}"
}

write_loaded_marker() {
    local bundle=$1 temporary
    [[ $bundle =~ ^bundle-[A-Za-z0-9_-]{1,64}$ ]] || return 1
    temporary=$(mktemp "$agent_root/.core-loaded-bundle.XXXXXXXX") || return 1
    printf '%s\n' "$bundle" > "$temporary"
    chown root:root "$temporary"
    chmod 0644 "$temporary"
    mv -Tf "$temporary" "$loaded_marker"
}

activate_bundle() {
    local bundle=$1 temporary=$agent_root/.current.new
    [[ $bundle =~ ^bundle-[A-Za-z0-9_-]{1,64}$ && -d $releases_dir/$bundle && \
        ! -L $releases_dir/$bundle ]] || return 1
    rm -f -- "$temporary"
    ln -s "releases/$bundle" "$temporary"
    mv -Tf "$temporary" "$current_link"
}

wait_ready() {
    systemctl restart jarvis-core.service && \
        curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
            --retry 11 --retry-delay 5 --retry-connrefused \
            http://127.0.0.1:8080/readyz >/dev/null
}

previous_bundle=$(active_bundle || true)
[[ -f $private_update && ! -L $private_update && \
    $(stat -c '%U:%G:%a' "$private_update") == root:root:755 ]] || \
    fail "trusted private agent update helper is unsafe"
"$private_update" --source "$source_root" || fail "private agent bundle validation failed"
new_bundle=$(active_bundle) || fail "private agent helper did not activate a safe bundle"

loaded_bundle=
if [[ -f $loaded_marker ]]; then
    read -r loaded_bundle extra < "$loaded_marker" || loaded_bundle=
    [[ -z ${extra:-} && $loaded_bundle =~ ^bundle-[A-Za-z0-9_-]{1,64}$ ]] || loaded_bundle=
fi
if [[ $new_bundle == "$loaded_bundle" ]]; then
    echo "jarvis private agents: active bundle already loaded: $new_bundle"
    exit 0
fi

if wait_ready; then
    if write_loaded_marker "$new_bundle"; then
        echo "jarvis private agents: activated and loaded $new_bundle"
        exit 0
    fi
    echo "jarvis private agents: could not record the Core-loaded agent bundle; rolling back" >&2
fi

if [[ -n $previous_bundle ]] && activate_bundle "$previous_bundle" && wait_ready; then
    write_loaded_marker "$previous_bundle" || true
    fail "Core readiness failed; restored $previous_bundle"
fi
rm -f -- "$loaded_marker"
fail "Core readiness failed and the previous agent bundle could not be restored"
