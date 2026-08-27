#!/usr/bin/env bash
# Root-only polling updater for the separately cloned private agent repository.
# Core, sandbox workloads and Codex never receive this checkout or its GitHub
# credential. Only the configured repository's origin/main is eligible.
set -euo pipefail
readonly config=/etc/jarvis/private-agent-updater.env
[[ ${EUID} -eq 0 && -f $config && ! -L $config ]] || exit 0
# shellcheck disable=SC1090
source "$config"
[[ ${JARVIS_PRIVATE_AGENT_SOURCE:-} == /* && ${JARVIS_PRIVATE_AGENT_REPOSITORY:-} == HawkeyNL/PersonalJarvisAgents ]] || exit 1
source_root=$JARVIS_PRIVATE_AGENT_SOURCE
[[ -d $source_root/.git ]] || exit 1
origin=$(git -C "$source_root" remote get-url origin)
[[ $origin == *"HawkeyNL/PersonalJarvisAgents"* ]] || exit 1
git -C "$source_root" fetch --quiet origin refs/heads/main
remote=$(git -C "$source_root" rev-parse FETCH_HEAD)
current=$(git -C "$source_root" rev-parse HEAD)
[[ $remote == "$current" ]] && exit 0
git -C "$source_root" merge --ff-only "$remote"
exec /usr/local/sbin/jarvis-private-update --source "$source_root"
