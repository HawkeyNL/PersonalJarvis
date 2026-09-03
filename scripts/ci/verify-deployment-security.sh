#!/usr/bin/env bash
set -euo pipefail

bash scripts/release/tests/test-build-linux-package.sh
sudo env GITHUB_ACTIONS=true bash deploy/systemd/tests/test-update-core-release.sh
sudo env GITHUB_ACTIONS=true bash deploy/systemd/tests/test-jarvis-admin-cli.sh
bash deploy/systemd/tests/test-ui-tty.sh
bash deploy/systemd/tests/test-systemd-runtime-lifecycle.sh
bash deploy/systemd/tests/test-production-bootstrap-assets.sh
bash deploy/systemd/tests/test-output-presentation.sh
bash deploy/systemd/tests/test-model-credential-boundaries.sh
bash deploy/systemd/tests/test-model-discovery-pipeline.sh
bash deploy/systemd/tests/test-huggingface-credential-probe.sh
sudo env GITHUB_ACTIONS=true bash deploy/surrealdb/tests/test-provision-core-user-shellless.sh
sudo env GITHUB_ACTIONS=true bash deploy/systemd/tests/test-stage-core-release.sh
sudo env GITHUB_ACTIONS=true bash deploy/systemd/tests/test-prepare-codex-worktree.sh
bash deploy/private/tests/test-public-release-boundary.sh
sudo env GITHUB_ACTIONS=true bash deploy/private/tests/test-private-installers.sh
sudo env GITHUB_ACTIONS=true bash deploy/private/tests/test-private-agent-poll.sh
bash deploy/caddy/tests/test-caddy-template.sh
bash deploy/opensandbox/tests/test-opensandbox-template.sh

readonly opensandbox_commit=6b2023e9b7eb80a940d88e6ae05fcbc0eb0cf23f
sandbox_source=$(mktemp -d)
trap 'rm -rf "$sandbox_source"' EXIT

git clone --no-checkout https://github.com/opensandbox-group/OpenSandbox.git "$sandbox_source"
git -C "$sandbox_source" fetch --depth=1 origin "$opensandbox_commit"
git -C "$sandbox_source" checkout --detach FETCH_HEAD
test "$(git -C "$sandbox_source" rev-parse HEAD)" = "$opensandbox_commit"

for patch in \
  0001-loopback-published-ports.patch \
  0002-egress-private-range-deny.patch \
  0003-egress-sidecar-hardening.patch \
  0004-egress-sidecar-resource-limits.patch; do
  git -C "$sandbox_source" apply --check "$GITHUB_WORKSPACE/deploy/opensandbox/patches/$patch"
  git -C "$sandbox_source" apply "$GITHUB_WORKSPACE/deploy/opensandbox/patches/$patch"
done

(cd "$sandbox_source/components/egress" && go test ./pkg/dnsproxy)
