#!/usr/bin/env bash
# Static public-tree guard: private prompt files must never be packaged or read
# by public CI. The synthetic Rust tests exercise the parser separately.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
release_workflow="$repo_dir/.github/workflows/release.yml"
publish_workflow="$repo_dir/.github/workflows/publish-release.yml"
release_builder="$repo_dir/scripts/release/build-linux.sh"
stage_release="$repo_dir/deploy/systemd/stage-core-release.sh"
update_release="$repo_dir/deploy/systemd/update-core-release.sh"
gitlab_ci="$repo_dir/.gitlab-ci.yml"

fail() {
    echo "public release boundary: $*" >&2
    exit 1
}

require_literal() {
    local file=$1
    local literal=$2
    local reason=$3
    grep -Fq -- "$literal" "$file" || fail "$reason"
}

reject_pattern() {
    local file=$1
    local pattern=$2
    local reason=$3
    if grep -Eq -- "$pattern" "$file"; then
        fail "$reason"
    fi
}

require_literal "$release_workflow" \
    'bash scripts/release/build-linux.sh stage "$RELEASE_TAG" "$RELEASE_REVISION"' \
    "release workflow must stage through scripts/release/build-linux.sh"
require_literal "$release_workflow" \
    'bash scripts/release/build-linux.sh package "$RELEASE_TAG" "$RELEASE_REVISION"' \
    "release workflow must package through scripts/release/build-linux.sh"
require_literal "$release_builder" \
    'install -m 0755 deploy/systemd/update-core-release.sh "$temporary_release/update-core-release"' \
    "canonical release builder must stage update-core-release.sh as update-core-release"
require_literal "$release_builder" \
    'install -m 0755 deploy/systemd/jarvis-models.sh "$temporary_release/jarvis-models"' \
    "canonical release builder must stage the reviewed model helper as jarvis-models"
require_literal "$release_builder" \
    'install -m 0755 deploy/systemd/jarvis-credentials.sh "$temporary_release/jarvis-credentials"' \
    "canonical release builder must stage the reviewed credential helper as jarvis-credentials"
require_literal "$release_builder" \
    'install -m 0755 deploy/private/install-agent-bundle.sh "$temporary_release/install-agent-bundle"' \
    "canonical release builder must stage the public private-agent bundler without private content"
require_literal "$release_builder" \
    'install -m 0755 deploy/private/jarvis-private-agent-poll.sh "$temporary_release/private-agent-poll"' \
    "canonical release builder must stage the fixed private-agent poll boundary"
require_literal "$release_builder" \
    'install -m 0755 deploy/private/jarvis-private-update.sh "$temporary_release/jarvis-private-update"' \
    "canonical release builder must stage the fixed private-agent update wrapper"
require_literal "$release_builder" \
    'install -m 0755 "$release_target_dir/release/jarvis-core-admin" "$temporary_release/jarvis-core-admin"' \
    "canonical release builder must stage the exact Core Admin App binary"
require_literal "$release_builder" \
    '--features custom-protocol' \
    "canonical release builder must embed the Core Admin App frontend instead of its Vite development URL"
require_literal "$release_builder" \
    '"$temporary_release/jarvis-core-admin" --frontend-mode' \
    "canonical release builder must verify the Core Admin App production frontend mode"
require_literal "$release_builder" \
    'components: {core: $core_version, cli: $cli_version, core_admin: $core_admin_version}' \
    "release manifest must expose separate Core, CLI, and Core Admin App versions"
require_literal "$release_builder" \
    'tooling: {private_agents: 1, admin_helpers: 1}' \
    "release manifest must bind private-agent and admin-helper tooling capabilities"
require_literal "$release_builder" \
    'jarvis-core-admin.version update-core-release jarvis-models jarvis-credentials' \
    "artifact checksum manifest must bind both versioned admin helpers"
require_literal "$gitlab_ci" \
    'npm run tauri:build --prefix jarvis-core-admin' \
    "GitLab CI must build the Ubuntu Core Admin App package"
require_literal "$gitlab_ci" \
    '"$package_root/usr/bin/jarvis-core-admin" --frontend-mode' \
    "GitLab CI must verify the packaged Core Admin App production frontend mode"
reject_pattern "$release_workflow" 'gh release create' \
    "candidate workflow must not publish before real Home Node acceptance"
require_literal "$publish_workflow" \
    'run-id: ${{ inputs.candidate_run_id }}' \
    "publish workflow must download from the explicitly accepted candidate run"
require_literal "$publish_workflow" \
    'github-token: ${{ github.token }}' \
    "cross-run artifact download must be authenticated and immutable"
require_literal "$publish_workflow" \
    'sha256sum --check --strict "$checksum"' \
    "publish workflow must strictly verify the accepted candidate checksum"
require_literal "$publish_workflow" \
    '--verify-tag' \
    "publish workflow must refuse to create a release for a missing tag"
require_literal "$publish_workflow" \
    'GH_REPO: ${{ github.repository }}' \
    "publish workflow must give GitHub CLI explicit repository context"
reject_pattern "$publish_workflow" '^[[:space:]]+--target ' \
    "publish workflow must not re-target the already verified existing tag"

# These files build, transfer or publish the public release artifact. None may
# acquire a private checkout, persona, or agent-content path.
for packaging_file in "$release_workflow" "$publish_workflow" "$release_builder" "$gitlab_ci"; do
    reject_pattern "$packaging_file" 'Jarvis\.md' \
        "$packaging_file must not package the protected Jarvis.md persona"
    reject_pattern "$packaging_file" 'PersonalJarvisAgents' \
        "$packaging_file must not access the private PersonalJarvisAgents repository"
    reject_pattern "$packaging_file" 'agents/' \
        "$packaging_file must not package private agent definitions"
done

# Both trusted installation paths must independently reject protected content,
# even if a malformed archive somehow passes the static packaging boundary.
for release_guard in "$stage_release" "$update_release"; do
    require_literal "$release_guard" "-name 'Jarvis.md'" \
        "$release_guard must reject packaged Jarvis.md files"
    require_literal "$release_guard" "-path '*/agents/*'" \
        "$release_guard must reject packaged private agent files"
    require_literal "$release_guard" 'release contains protected private configuration' \
        "$release_guard must report protected private release content"
done

echo "Public release boundary fixture tests passed"
