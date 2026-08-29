#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 stage|package vMAJOR.MINOR.PATCH REVISION [OUTPUT_ROOT]" >&2
  exit 2
}

[[ $# -ge 3 && $# -le 4 ]] || usage
mode=$1
release_tag=$2
release_revision=$3
output_root=${4:-dist}
[[ "$mode" == stage || "$mode" == package ]] || usage
[[ "$release_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "release tag must use stable vMAJOR.MINOR.PATCH form" >&2
  exit 1
}
[[ "$release_revision" =~ ^[0-9a-f]{40}$ ]] || {
  echo "release revision must be a full lowercase Git commit SHA" >&2
  exit 1
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
cd "$repo_root"
release_name="jarvis-core-$release_tag"
stage_root="$output_root/candidate"
release_dir="$stage_root/$release_name"
artifact="$output_root/$release_name-linux-x86_64.tar.gz"

if [[ "$mode" == package ]]; then
  [[ -d "$release_dir" ]] || {
    echo "staged release candidate is missing: $release_dir" >&2
    exit 1
  }
  manifest_tag=$(jq -r '.tag' "$release_dir/release.json")
  manifest_revision=$(jq -r '.revision' "$release_dir/release.json")
  [[ "$manifest_tag" == "$release_tag" && "$manifest_revision" == "$release_revision" ]] || {
    echo "staged release manifest does not match requested tag/revision" >&2
    exit 1
  }
  (cd "$release_dir" && sha256sum --check --strict artifact-binaries.sha256)
  mkdir -p "$output_root"
  tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -C "$stage_root" -czf "$artifact" "$release_name"
  (cd "$output_root" && sha256sum "$(basename "$artifact")" > "$(basename "$artifact").sha256")
  echo "Packaged tested candidate: $artifact"
  exit 0
fi

source /etc/os-release
[[ "${ID:-}" == ubuntu && "${VERSION_ID:-}" == 26.04 ]] || {
  echo "release staging requires the canonical Ubuntu 26.04 builder" >&2
  exit 1
}
[[ "$(rustc --version)" == "rustc 1.97.1 (8bab26f4f 2026-07-14)" ]] || {
  echo "release staging requires rustc 1.97.1 (8bab26f4f 2026-07-14)" >&2
  rustc -Vv >&2
  exit 1
}
[[ "$(cargo --version)" == "cargo 1.97.1 (c980f4866 2026-06-30)" ]] || {
  echo "release staging requires cargo 1.97.1 (c980f4866 2026-06-30)" >&2
  cargo -Vv >&2
  exit 1
}
[[ -z "${RUSTFLAGS:-}" && -z "${CARGO_ENCODED_RUSTFLAGS:-}" ]] || {
  echo "refusing inherited Rust flags for a release build" >&2
  exit 1
}
[[ "$(git rev-parse HEAD)" == "$release_revision" ]] || {
  echo "release revision does not match the checked-out commit" >&2
  exit 1
}
[[ -z "$(git status --porcelain --untracked-files=normal)" ]] || {
  echo "release staging requires a clean working tree" >&2
  exit 1
}
[[ ! -e "$release_dir" ]] || {
  echo "refusing to overwrite existing candidate: $release_dir" >&2
  exit 1
}

effective_cargo_home=${CARGO_HOME:-$HOME/.cargo}
deterministic_rustflags="--remap-path-prefix=$repo_root=/usr/src/PersonalJarvis --remap-path-prefix=$effective_cargo_home=/usr/local/cargo"
release_target_dir=${CARGO_TARGET_DIR:-$repo_root/target/release-candidate}
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=$release_target_dir
export RUSTFLAGS=$deterministic_rustflags

# One Cargo invocation produces every release executable. Later steps copy and
# test these exact bytes; nothing recompiles between acceptance and packaging.
cargo build --locked --release \
  -p jarvis-api --bins \
  -p jarvis-core --bin jarvis-agent-bundle \
  -p jarvis-admin --bin jarvis

mkdir -p "$output_root"
temporary=$(mktemp -d "$output_root/.candidate.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
temporary_release="$temporary/$release_name"
mkdir -p "$temporary_release"
install -m 0755 "$release_target_dir/release/jarvis-api" "$temporary_release/jarvis-api"
install -m 0755 "$release_target_dir/release/jarvis-config-broker" "$temporary_release/jarvis-config-broker"
install -m 0755 "$release_target_dir/release/jarvis-codex-broker" "$temporary_release/jarvis-codex-broker"
install -m 0755 "$release_target_dir/release/jarvis-agent-bundle" "$temporary_release/jarvis-agent-bundle"
install -m 0755 "$release_target_dir/release/jarvis" "$temporary_release/jarvis"
install -m 0755 deploy/systemd/update-core-release.sh "$temporary_release/update-core-release"

schema_manifest="$temporary/schema.sha256"
while IFS= read -r schema; do
  sha256sum "schema/surreal/$schema"
done < <(find schema/surreal -type f -name '*.surql' -printf '%P\n' | LC_ALL=C sort) > "$schema_manifest"
schema_sha256=$(sha256sum "$schema_manifest" | awk '{print $1}')
jq -n \
  --arg tag "$release_tag" \
  --arg revision "$release_revision" \
  --arg schema_sha256 "$schema_sha256" \
  '{tag: $tag, revision: $revision, schema_sha256: $schema_sha256}' \
  > "$temporary_release/release.json"

(
  cd "$temporary_release"
  sha256sum jarvis-api jarvis-config-broker jarvis-codex-broker jarvis-agent-bundle jarvis update-core-release \
    > artifact-binaries.sha256
)
rustc_version=$(rustc --version)
cargo_version=$(cargo --version)
llvm_version=$(rustc -Vv | awk -F': ' '/^LLVM version:/ {print $2}')
host_target=$(rustc -Vv | awk -F': ' '/^host:/ {print $2}')
cc_version=$(cc --version | sed -n '1p')
linker_version=$(readelf -p .comment "$temporary_release/jarvis" | awk '/Linker:/ {sub(/^.*Linker: /, ""); print; exit}')
jq -n \
  --arg revision "$release_revision" \
  --arg os "Ubuntu $VERSION_ID" \
  --arg rustc "$rustc_version" \
  --arg cargo "$cargo_version" \
  --arg llvm "$llvm_version" \
  --arg cc "$cc_version" \
  --arg linker "$linker_version" \
  --arg target "$host_target" \
  --arg source_date_epoch "$SOURCE_DATE_EPOCH" \
  --arg rustflags "$deterministic_rustflags" \
  '{revision: $revision, os: $os, rustc: $rustc, cargo: $cargo, llvm: $llvm, cc: $cc, linker: $linker, target: $target, profile: "release", locked: true, cargo_config: "none", rustflags: $rustflags, source_date_epoch: $source_date_epoch}' \
  > "$temporary_release/build-provenance.json"

mkdir -p "$stage_root"
mv "$temporary_release" "$release_dir"
echo "Staged release candidate: $release_dir"
echo "Test these exact bytes before running the package mode."
