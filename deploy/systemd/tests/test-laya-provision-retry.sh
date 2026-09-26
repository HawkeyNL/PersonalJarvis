#!/usr/bin/env bash
# Unprivileged fixture for the production materialization and EXIT cleanup path.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d /tmp/jarvis-laya-provision.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
trap 'rc=$?; echo "Laya provisioning fixture failed at line $LINENO: $BASH_COMMAND" >&2; [[ ! -f $fixture/output ]] || sed -n "1,12p" "$fixture/output" >&2; exit "$rc"' ERR
mkdir -p "$fixture/reviewed/wheels" "$fixture/runtime"
printf 'laya[serve]==0.3.20 --hash=sha256:%064d\n' 0 > "$fixture/reviewed/requirements.lock"
printf 'fixture wheel bytes\n' > "$fixture/reviewed/wheels/fixture.whl"
chmod 0755 "$fixture/reviewed" "$fixture/reviewed/wheels"
chmod 0644 "$fixture/reviewed/requirements.lock" "$fixture/reviewed/wheels/fixture.whl"
runner=(bash)
if (( EUID == 0 )); then
    # In CI, use actual root:root reviewed input and an unprivileged installer.
    chmod 0755 "$fixture"
    chown -R root:root "$fixture/reviewed"
    chown nobody:nogroup "$fixture/runtime"
    runner=(runuser -u nobody -- bash)
fi
before=$(find "$fixture/reviewed" -type f -print0 | sort -z | xargs -0 sha256sum)
ownership=$(find "$fixture/reviewed" -printf '%P %u:%g %m\n' | sort)
for attempt in 1 2; do
    if GITHUB_ACTIONS=true "${runner[@]}" "$repo/deploy/systemd/provision-laya.sh" \
        --fixture-installer-input "$fixture/reviewed" "$fixture/runtime" \
        >"$fixture/output" 2>&1; then
        echo 'simulated provisioning failure unexpectedly succeeded' >&2; exit 1
    fi
    grep -Fq 'simulated failure after temporary installer input preparation' "$fixture/output"
    [[ $(find "$fixture/reviewed" -type f -print0 | sort -z | xargs -0 sha256sum) == "$before" ]]
    [[ $(find "$fixture/reviewed" -printf '%P %u:%g %m\n' | sort) == "$ownership" ]]
    [[ -z $(find "$fixture/reviewed" -type l -print -quit) ]]
    [[ -z $(find "$fixture/runtime" -mindepth 1 -print -quit) ]]
done
ln -s /etc/passwd "$fixture/reviewed/wheels/unsafe.whl"
if GITHUB_ACTIONS=true "${runner[@]}" "$repo/deploy/systemd/provision-laya.sh" \
    --fixture-installer-input "$fixture/reviewed" "$fixture/runtime" \
    >"$fixture/output" 2>&1; then
    echo 'symlinked installer input was accepted' >&2; exit 1
fi
grep -Fq 'unsafe wheelhouse entry' "$fixture/output"
[[ -z $(find "$fixture/runtime" -mindepth 1 -print -quit) ]]
echo 'Laya reviewed staging survives failure and retry unchanged'
