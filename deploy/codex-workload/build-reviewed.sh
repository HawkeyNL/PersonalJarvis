#!/usr/bin/env bash
# Explicit owner-operated offline image build from a verified Core candidate.
set -euo pipefail

[[ $# == 3 ]] || { echo 'usage: build-reviewed.sh RELEASE_DIR UBUNTU_BASE_DIGEST LOCAL_IMAGE_TAG' >&2; exit 2; }
release=$1 base=$2 image=$3
[[ $base =~ ^ubuntu@sha256:[0-9a-f]{64}$ ]] || { echo 'base must be a reviewed Ubuntu digest' >&2; exit 2; }
[[ $image =~ ^[a-z0-9][a-z0-9._:/-]{1,160}$ && $image != *latest* ]] || { echo 'invalid local image tag' >&2; exit 2; }
[[ -d $release && ! -L $release ]] || { echo 'release directory missing or unsafe' >&2; exit 1; }
[[ -f $release/release.json && ! -L $release/release.json ]] || exit 1
jq -e '.tooling.codex_runtime == 1 and (.revision | test("^[0-9a-f]{40}$"))' "$release/release.json" >/dev/null
[[ -f $release/jarvis-codex-runtime && ! -L $release/jarvis-codex-runtime && -x $release/jarvis-codex-runtime ]] || exit 1
[[ $(stat -c '%a' "$release/jarvis-codex-runtime") == 755 ]] || exit 1
(cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null)
[[ $(awk '$2 == "jarvis-codex-runtime" { count++ } END { print count+0 }' "$release/artifact-binaries.sha256") == 1 ]] || exit 1
docker --host unix:///var/run/docker.sock image inspect "$base" >/dev/null

build_root=$(mktemp -d /tmp/jarvis-codex-workload.XXXXXXXX)
trap 'rm -rf -- "$build_root"' EXIT
install -m 0555 "$release/jarvis-codex-runtime" "$build_root/jarvis-codex-runtime"
install -m 0644 "$(dirname "$0")/Dockerfile" "$build_root/Dockerfile"
runtime_hash=$(sha256sum "$build_root/jarvis-codex-runtime" | awk '{print $1}')
revision=$(jq -r '.revision' "$release/release.json")
docker --host unix:///var/run/docker.sock build --network none --pull=false --no-cache \
  --build-arg "BASE_IMAGE=$base" \
  --build-arg "SOURCE_REVISION=$revision" \
  --build-arg "RUNTIME_SHA256=$runtime_hash" \
  --tag "$image" "$build_root"
echo 'Local workload built. Production still requires a reviewed registry digest and Home Node/Kata acceptance.'
