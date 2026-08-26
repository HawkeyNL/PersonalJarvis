#!/usr/bin/env bash
# Static public-tree guard: private prompt files must never be packaged or read
# by public CI. The synthetic Rust tests exercise the parser separately.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
release_workflow="$repo_dir/.github/workflows/release.yml"
! grep -q 'jarvis-core/Jarvis.md' "$release_workflow"
! grep -q 'PersonalJarvisAgents' "$release_workflow"
! grep -q 'actions/checkout.*PersonalJarvisAgents' "$release_workflow"
grep -q 'release contains protected private configuration' "$repo_dir/deploy/systemd/stage-core-release.sh"
grep -q 'release contains protected private configuration' "$repo_dir/deploy/systemd/update-core-release.sh"
echo "Public release boundary fixture tests passed"
