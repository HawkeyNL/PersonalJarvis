#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 stage|package vMAJOR.MINOR.PATCH REVISION [OUTPUT_ROOT]" >&2
  exit 2
}

verify_admin_helper_candidate() {
  local release=$1 helper matches
  jq -e '.tooling.admin_helpers == 1 and (.tooling.admin_helpers | type) == "number"' \
    "$release/release.json" >/dev/null || {
    echo "release candidate does not declare admin-helper tooling capability 1" >&2
    exit 1
  }
  [[ -f "$release/artifact-binaries.sha256" && ! -L "$release/artifact-binaries.sha256" ]] || {
    echo "release candidate artifact checksum manifest is missing or unsafe" >&2
    exit 1
  }
  for helper in jarvis-models jarvis-credentials; do
    [[ -f "$release/$helper" && ! -L "$release/$helper" && -x "$release/$helper" ]] || {
      echo "release candidate is missing executable $helper" >&2
      exit 1
    }
    matches=$(awk -v helper="$helper" '$2 == helper { count++ } END { print count + 0 }' \
      "$release/artifact-binaries.sha256")
    [[ $matches == 1 ]] || {
      echo "release candidate does not uniquely checksum-bind $helper" >&2
      exit 1
    }
  done
  [[ -f "$release/pricing-registry.json" && ! -L "$release/pricing-registry.json" ]] || {
    echo "release candidate is missing the reviewed pricing registry" >&2
    exit 1
  }
  jq -e '.version == 1 and (.models | type == "array")' \
    "$release/pricing-registry.json" >/dev/null || {
    echo "release candidate pricing registry is malformed" >&2
    exit 1
  }
  matches=$(awk '$2 == "pricing-registry.json" { count++ } END { print count + 0 }' \
    "$release/artifact-binaries.sha256")
  [[ $matches == 1 ]] || {
    echo "release candidate does not uniquely checksum-bind pricing-registry.json" >&2
    exit 1
  }
}

verify_systemd_unit_candidate() {
  local release=$1
  jq -e '.tooling.systemd_units == 1 and (.tooling.systemd_units | type) == "number"' \
    "$release/release.json" >/dev/null || {
    echo "release candidate does not declare managed-systemd capability 1" >&2
    exit 1
  }
  "$release/manage-systemd-units" validate-artifacts "$release"
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
components_asset="$output_root/$release_name-components.json"

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
  verify_admin_helper_candidate "$release_dir"
  verify_systemd_unit_candidate "$release_dir"
  (cd "$release_dir" && sha256sum --check --strict artifact-binaries.sha256)
  mkdir -p "$output_root"
  tar --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" --owner=0 --group=0 --numeric-owner \
    -C "$stage_root" -czf "$artifact" "$release_name"
  (cd "$output_root" && sha256sum "$(basename "$artifact")" > "$(basename "$artifact").sha256")
  jq '{tag, revision, components}' "$release_dir/release.json" > "$components_asset"
  (cd "$output_root" && sha256sum "$(basename "$components_asset")" > "$(basename "$components_asset").sha256")
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
[[ "$(node --version)" == "v24.20.0" ]] || {
  echo "release staging requires Node.js v24.20.0" >&2
  node --version >&2
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

bash jarvis-core-admin/scripts/verify-versions.sh
workspace_metadata=$(cargo metadata --locked --no-deps --format-version 1)
app_metadata=$(cargo metadata --locked --no-deps --format-version 1 \
  --manifest-path jarvis-core-admin/src-tauri/Cargo.toml)
core_version=$(jq -er '.packages[] | select(.name == "jarvis-api") | .version' <<<"$workspace_metadata")
cli_version=$(jq -er '.packages[] | select(.name == "jarvis-admin") | .version' <<<"$workspace_metadata")
core_admin_version=$(jq -er '.packages[] | select(.name == "jarvis-core-admin") | .version' <<<"$app_metadata")

# One Cargo invocation produces every release executable. Later steps copy and
# test these exact bytes; nothing recompiles between acceptance and packaging.
cargo build --locked --release \
  -p jarvis-api --bins \
  -p jarvis-core --bin jarvis-agent-bundle \
  -p jarvis-admin --bin jarvis

# The Linux graphical administrator is a separate Tauri workspace. Its Vue
# assets and Rust executable are built once here; the exact executable is then
# staged, acceptance-tested and published with the Core release candidate.
npm ci --prefix jarvis-core-admin
npm run build --prefix jarvis-core-admin
cargo build --locked --release \
  --manifest-path jarvis-core-admin/src-tauri/Cargo.toml \
  --features custom-protocol \
  --bin jarvis-core-admin

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
install -m 0755 "$release_target_dir/release/jarvis-core-admin" "$temporary_release/jarvis-core-admin"
# A production Core Admin App must serve its embedded Vue assets through
# Tauri's custom protocol. Tauri retains the devUrl in compiled configuration,
# so inspect the compile-time feature selection instead of grepping strings.
if [[ $("$temporary_release/jarvis-core-admin" --frontend-mode) != production ]]; then
  echo "Core Admin App release binary was built in development frontend mode" >&2
  exit 1
fi
install -m 0644 jarvis-core-admin/packaging/jarvis-core-admin.desktop \
  "$temporary_release/jarvis-core-admin.desktop"
install -m 0644 jarvis-app/src-tauri/icons/128x128.png \
  "$temporary_release/jarvis-core-admin.png"
printf '%s\n' "$core_admin_version" > "$temporary_release/jarvis-core-admin.version"
install -m 0755 deploy/systemd/update-core-release.sh "$temporary_release/update-core-release"
install -m 0755 deploy/systemd/jarvis-models.sh "$temporary_release/jarvis-models"
install -m 0755 deploy/systemd/jarvis-credentials.sh "$temporary_release/jarvis-credentials"
install -m 0755 deploy/systemd/manage-systemd-units.sh "$temporary_release/manage-systemd-units"
install -m 0755 deploy/systemd/verify-home-node.sh "$temporary_release/verify-home-node"
install -m 0755 deploy/systemd/install-home-node-core.sh "$temporary_release/install-home-node-core"
install -m 0644 deploy/lib/ui.sh "$temporary_release/ui.sh"
for unit in \
  jarvis-core.service \
  jarvis-config-broker.service \
  jarvis-codex-broker.service \
  jarvis-codex.service \
  jarvis-opensandbox.service \
  jarvis-surrealdb.service \
  jarvis-updater.service \
  jarvis-updater.timer \
  jarvis-private-agent-updater.service \
  jarvis-private-agent-updater.timer; do
  install -m 0644 "deploy/systemd/$unit" "$temporary_release/systemd-$unit"
done
install -m 0644 deploy/systemd/pricing-registry.json "$temporary_release/pricing-registry.json"
install -m 0755 deploy/private/install-agent-bundle.sh "$temporary_release/install-agent-bundle"
install -m 0755 deploy/private/jarvis-private-agent-poll.sh "$temporary_release/private-agent-poll"
install -m 0755 deploy/private/jarvis-private-update.sh "$temporary_release/jarvis-private-update"

schema_manifest="$temporary/schema.sha256"
while IFS= read -r schema; do
  sha256sum "schema/surreal/$schema"
done < <(find schema/surreal -type f -name '*.surql' -printf '%P\n' | LC_ALL=C sort) > "$schema_manifest"
schema_sha256=$(sha256sum "$schema_manifest" | awk '{print $1}')
jq -n \
  --arg tag "$release_tag" \
  --arg revision "$release_revision" \
  --arg schema_sha256 "$schema_sha256" \
  --arg core_version "$core_version" \
  --arg cli_version "$cli_version" \
  --arg core_admin_version "$core_admin_version" \
  '{tag: $tag, revision: $revision, schema_sha256: $schema_sha256, components: {core: $core_version, cli: $cli_version, core_admin: $core_admin_version}, tooling: {private_agents: 1, admin_helpers: 1, systemd_units: 1}}' \
  > "$temporary_release/release.json"

(
  cd "$temporary_release"
  sha256sum jarvis-api jarvis-config-broker jarvis-codex-broker jarvis-agent-bundle \
    jarvis jarvis-core-admin jarvis-core-admin.desktop jarvis-core-admin.png \
    jarvis-core-admin.version update-core-release jarvis-models jarvis-credentials \
    manage-systemd-units verify-home-node install-home-node-core ui.sh \
    pricing-registry.json \
    install-agent-bundle \
    private-agent-poll jarvis-private-update systemd-*.service systemd-*.timer \
    > artifact-binaries.sha256
)
verify_admin_helper_candidate "$temporary_release"
verify_systemd_unit_candidate "$temporary_release"
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
