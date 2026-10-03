#!/usr/bin/env bash
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
# The reviewed runtime exclusively creates these directories with create_new
# semantics; a precreated image directory would make every run fail closed.
if grep -Eq 'mkdir.*(/workspace/source|/workspace/channel|/workspace/artifacts)' "$repo/deploy/codex-workload/Dockerfile"; then
    echo 'runtime-owned workspace directory is precreated by the image' >&2; exit 1
fi
fixture=$(mktemp -d /tmp/jarvis-codex-workload-test.XXXXXXXX)
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/release" "$fixture/bin"
printf '#!/bin/sh\nexit 2\n' > "$fixture/release/jarvis-codex-runtime"
chmod 0755 "$fixture/release/jarvis-codex-runtime"
printf '{"revision":"%s","tooling":{"codex_runtime":1}}\n' "$(printf a%.0s {1..40})" > "$fixture/release/release.json"
(cd "$fixture/release" && sha256sum jarvis-codex-runtime > artifact-binaries.sha256)
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >> "$FIXTURE_DOCKER_LOG"\n' > "$fixture/bin/docker"
chmod 0755 "$fixture/bin/docker"
export FIXTURE_DOCKER_LOG="$fixture/docker.log"
export PATH="$fixture/bin:$PATH"
base="ubuntu@sha256:$(printf b%.0s {1..64})"
"$repo/deploy/codex-workload/build-reviewed.sh" "$fixture/release" "$base" jarvis-codex:fixture >/dev/null
grep -Fq -- '--host unix:///var/run/docker.sock build --network none --pull=false --no-cache' "$FIXTURE_DOCKER_LOG"
grep -Fq -- "--host unix:///var/run/docker.sock image inspect $base" "$FIXTURE_DOCKER_LOG"
if "$repo/deploy/codex-workload/build-reviewed.sh" "$fixture/release" ubuntu:latest jarvis-codex:fixture >/dev/null 2>&1; then
    echo 'mutable base was accepted' >&2; exit 1
fi
printf 'tamper\n' >> "$fixture/release/jarvis-codex-runtime"
if "$repo/deploy/codex-workload/build-reviewed.sh" "$fixture/release" "$base" jarvis-codex:fixture >/dev/null 2>&1; then
    echo 'tampered runtime was accepted' >&2; exit 1
fi
echo 'Codex workload digest/offline/checksum fixture passed'
