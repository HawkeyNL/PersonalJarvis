#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
app_root="$repo_root/jarvis-core-admin"

fail() {
  echo "core admin version check: $*" >&2
  exit 1
}

stable_version() {
  [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

workspace_metadata=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$repo_root/Cargo.toml")
app_metadata=$(cargo metadata --locked --no-deps --format-version 1 --manifest-path "$app_root/src-tauri/Cargo.toml")
core_version=$(jq -er '.packages[] | select(.name == "jarvis-api") | .version' <<<"$workspace_metadata")
cli_version=$(jq -er '.packages[] | select(.name == "jarvis-admin") | .version' <<<"$workspace_metadata")
app_cargo_version=$(jq -er '.packages[] | select(.name == "jarvis-core-admin") | .version' <<<"$app_metadata")
app_package_version=$(jq -er '.version | strings' "$app_root/package.json")
app_tauri_version=$(jq -er '.version | strings' "$app_root/src-tauri/tauri.conf.json")

stable_version "$core_version" || fail "Core package version is not stable SemVer"
stable_version "$cli_version" || fail "CLI package version is not stable SemVer"
stable_version "$app_cargo_version" || fail "Core Admin App package version is not stable SemVer"
[[ $app_cargo_version == "$app_package_version" ]] || \
  fail "Cargo.toml and package.json app versions differ"
[[ $app_cargo_version == "$app_tauri_version" ]] || \
  fail "Cargo.toml and tauri.conf.json app versions differ"

printf 'Core: %s\nCLI: %s\nCore Admin App: %s\n' \
  "$core_version" "$cli_version" "$app_cargo_version"
