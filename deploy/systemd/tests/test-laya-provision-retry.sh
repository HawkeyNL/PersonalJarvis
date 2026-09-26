#!/usr/bin/env bash
# Unprivileged fixture for the production materialization and EXIT cleanup path.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d /tmp/jarvis-laya-provision.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
trap 'rc=$?; echo "Laya provisioning fixture failed at line $LINENO: $BASH_COMMAND" >&2; [[ ! -f $fixture/output ]] || sed -n "1,12p" "$fixture/output" >&2; exit "$rc"' ERR
install -m 0755 "$repo/deploy/systemd/provision-laya.sh" "$fixture/provision-laya.sh"
mkdir -p "$fixture/reviewed/wheels" "$fixture/runtime"
printf 'fixture wheel bytes\n' > "$fixture/reviewed/wheels/fixture.whl"
wheel_hash=$(sha256sum "$fixture/reviewed/wheels/fixture.whl")
printf 'laya[serve]==0.3.20 --hash=sha256:%s\n' "${wheel_hash%% *}" > "$fixture/reviewed/requirements.lock"
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
    if GITHUB_ACTIONS=true "${runner[@]}" "$fixture/provision-laya.sh" \
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
for remote in \
    '--extra-index-url https://example.invalid/simple' \
    '--find-links https://example.invalid/wheels' \
    'some-package @ https://example.invalid/pkg.whl' \
    'some-package @ git+https://example.invalid/repo.git' \
    'some-package @ file:///etc/passwd'; do
    printf '%s\n' "$remote" >> "$fixture/reviewed/requirements.lock"
    if GITHUB_ACTIONS=true "${runner[@]}" "$fixture/provision-laya.sh" \
        --fixture-installer-input "$fixture/reviewed" "$fixture/runtime" \
        >"$fixture/output" 2>&1; then
        echo "remote/direct requirement was accepted: $remote" >&2; exit 1
    fi
    grep -Fq 'unsupported requirement or source' "$fixture/output"
    [[ -z $(find "$fixture/runtime" -mindepth 1 -print -quit) ]]
    sed -i '$d' "$fixture/reviewed/requirements.lock"
done
printf 'unreviewed source distribution\n' > "$fixture/reviewed/wheels/fixture.tar.gz"
if GITHUB_ACTIONS=true "${runner[@]}" "$fixture/provision-laya.sh" \
    --fixture-installer-input "$fixture/reviewed" "$fixture/runtime" \
    >"$fixture/output" 2>&1; then
    echo 'sdist was accepted' >&2; exit 1
fi
grep -Fq 'unsafe wheelhouse entry' "$fixture/output"
rm -- "$fixture/reviewed/wheels/fixture.tar.gz"
ln -s /etc/passwd "$fixture/reviewed/wheels/unsafe.whl"
if GITHUB_ACTIONS=true "${runner[@]}" "$fixture/provision-laya.sh" \
    --fixture-installer-input "$fixture/reviewed" "$fixture/runtime" \
    >"$fixture/output" 2>&1; then
    echo 'symlinked installer input was accepted' >&2; exit 1
fi
grep -Fq 'unsafe wheelhouse entry' "$fixture/output"
[[ -z $(find "$fixture/runtime" -mindepth 1 -print -quit) ]]
echo 'Laya reviewed staging survives failure and retry unchanged'
