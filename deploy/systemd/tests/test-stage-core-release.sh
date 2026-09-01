#!/usr/bin/env bash
# Linux-only fixture for explicit release staging. It uses a fake curl command
# and never contacts GitHub; the /opt/jarvis fixture is removed on exit.
set -euo pipefail

[[ ${GITHUB_ACTIONS:-} == true ]] || { echo "refusing outside GitHub Actions" >&2; exit 1; }
[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
stager="$repo_dir/deploy/systemd/stage-core-release.sh"
fixture=$(mktemp -d)
fake_bin="$fixture/bin"
mkdir -p "$fake_bin"
cleanup() { rm -rf -- "$fixture" /opt/jarvis; }
trap cleanup EXIT

cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=
for arg in "$@"; do [[ $arg == https://* ]] && url=$arg; done
while (($#)); do
    case "$1" in -o) output=$2; shift 2 ;; *) shift ;; esac
done
[[ -n ${output:-} && -n ${url:-} ]] || exit 1
cp "$JARVIS_STAGE_FIXTURE/${url##*/}" "$output"
EOF
chmod 0755 "$fake_bin/curl"

write_asset() {
    local tag=$1
    local manifest_tag=$2
    local admin_helpers=${3:-true}
    local artifact="jarvis-core-$tag-linux-x86_64.tar.gz"
    local asset="$fixture/asset"
    rm -rf -- "$asset"
    mkdir -p "$asset/jarvis-core-$tag"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/jarvis-api"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis-api"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/jarvis-config-broker"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis-config-broker"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/jarvis-codex-broker"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis-codex-broker"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/jarvis-agent-bundle"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis-agent-bundle"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/jarvis"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis"
    printf '#!/usr/bin/env bash\n[[ ${1:-} == --component-version ]] && { printf "1.2.3\\n"; exit 0; }\nexit 0\n' \
      > "$asset/jarvis-core-$tag/jarvis-core-admin"
    chmod 0755 "$asset/jarvis-core-$tag/jarvis-core-admin"
    printf '[Desktop Entry]\nType=Application\nName=Jarvis Core Administration\nExec=/usr/bin/jarvis-core-admin\n' \
      > "$asset/jarvis-core-$tag/jarvis-core-admin.desktop"
    printf 'synthetic png fixture\n' > "$asset/jarvis-core-$tag/jarvis-core-admin.png"
    printf '1.2.3\n' > "$asset/jarvis-core-$tag/jarvis-core-admin.version"
    chmod 0644 "$asset/jarvis-core-$tag/jarvis-core-admin.desktop" \
      "$asset/jarvis-core-$tag/jarvis-core-admin.png" \
      "$asset/jarvis-core-$tag/jarvis-core-admin.version"
    cp "$repo_dir/deploy/systemd/update-core-release.sh" "$asset/jarvis-core-$tag/update-core-release"
    chmod 0755 "$asset/jarvis-core-$tag/update-core-release"
    for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
      printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/$helper"
      chmod 0755 "$asset/jarvis-core-$tag/$helper"
    done
    if [[ $admin_helpers == true ]]; then
      for helper in jarvis-models jarvis-credentials; do
        printf '#!/usr/bin/env bash\nexit 0\n' > "$asset/jarvis-core-$tag/$helper"
        chmod 0755 "$asset/jarvis-core-$tag/$helper"
      done
    fi
    jq -n --arg tag "$manifest_tag" \
      --argjson admin_helpers "$admin_helpers" \
      '{tag:$tag,revision:"0123456789abcdef0123456789abcdef01234567",schema_sha256:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",components:{core:"1.2.3",cli:"1.2.3",core_admin:"1.2.3"},tooling:({private_agents:1} + if $admin_helpers then {admin_helpers:1} else {} end)}' \
      > "$asset/jarvis-core-$tag/release.json"
    local -a checksummed=(
      jarvis-api jarvis-config-broker jarvis-codex-broker jarvis-agent-bundle
      jarvis jarvis-core-admin jarvis-core-admin.desktop jarvis-core-admin.png
      jarvis-core-admin.version update-core-release install-agent-bundle
      private-agent-poll jarvis-private-update
    )
    [[ $admin_helpers != true ]] || checksummed+=(jarvis-models jarvis-credentials)
    (
      cd "$asset/jarvis-core-$tag"
      sha256sum "${checksummed[@]}" > artifact-binaries.sha256
    )
    tar -C "$asset" -czf "$fixture/$artifact" "jarvis-core-$tag"
    (cd "$fixture" && sha256sum "$artifact" > "$artifact.sha256")
}

run_stage() {
    PATH="$fake_bin:$PATH" JARVIS_STAGE_FIXTURE="$fixture" bash "$stager" "$1"
}

install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.3 v1.2.3
run_stage v1.2.3
[[ -x /opt/jarvis/releases/v1.2.3/jarvis-api ]]
[[ -x /opt/jarvis/releases/v1.2.3/jarvis ]]
[[ -x /opt/jarvis/releases/v1.2.3/jarvis-core-admin ]]
[[ -x /opt/jarvis/releases/v1.2.3/install-agent-bundle ]]
[[ -x /opt/jarvis/releases/v1.2.3/private-agent-poll ]]
[[ -x /opt/jarvis/releases/v1.2.3/jarvis-private-update ]]
[[ -x /opt/jarvis/releases/v1.2.3/jarvis-models ]]
[[ -x /opt/jarvis/releases/v1.2.3/jarvis-credentials ]]
[[ $(stat -c '%U:%G:%a' /opt/jarvis/releases/v1.2.3/jarvis-models) == root:root:755 ]]
[[ $(stat -c '%U:%G:%a' /opt/jarvis/releases/v1.2.3/jarvis-credentials) == root:root:755 ]]
grep -Eq '^[0-9a-f]{64}  jarvis-models$' /opt/jarvis/releases/v1.2.3/artifact-binaries.sha256
grep -Eq '^[0-9a-f]{64}  jarvis-credentials$' /opt/jarvis/releases/v1.2.3/artifact-binaries.sha256
jq -e '.tooling.admin_helpers == 1' /opt/jarvis/releases/v1.2.3/release.json >/dev/null
grep -qx '1.2.3' /opt/jarvis/releases/v1.2.3/jarvis-core-admin.version
[[ -f /opt/jarvis/releases/v1.2.3/release.verification ]]
[[ $(stat -c '%U:%G:%a' /opt/jarvis/releases/v1.2.3) == root:root:755 ]]

rm -rf -- /opt/jarvis
install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.4 v1.2.4
printf '%s  %s\n' "$(printf '0%.0s' {1..64})" 'jarvis-core-v1.2.4-linux-x86_64.tar.gz' > "$fixture/jarvis-core-v1.2.4-linux-x86_64.tar.gz.sha256"
if run_stage v1.2.4; then
    echo "bad checksum was accepted" >&2
    exit 1
fi
[[ ! -e /opt/jarvis/releases/v1.2.4 ]]

write_asset v1.2.5 v9.9.9
if run_stage v1.2.5; then
    echo "mismatched release manifest was accepted" >&2
    exit 1
fi
[[ ! -e /opt/jarvis/releases/v1.2.5 ]]

rm -rf -- /opt/jarvis
install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.7 v1.2.7
rm -f -- "$fixture/asset/jarvis-core-v1.2.7/private-agent-poll"
tar -C "$fixture/asset" -czf "$fixture/jarvis-core-v1.2.7-linux-x86_64.tar.gz" jarvis-core-v1.2.7
(cd "$fixture" && sha256sum jarvis-core-v1.2.7-linux-x86_64.tar.gz > jarvis-core-v1.2.7-linux-x86_64.tar.gz.sha256)
if run_stage v1.2.7; then
    echo "incomplete versioned private-agent tooling was accepted" >&2
    exit 1
fi
[[ ! -e /opt/jarvis/releases/v1.2.7 ]]

rm -rf -- /opt/jarvis
install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.8 v1.2.8
rm -f -- "$fixture/asset/jarvis-core-v1.2.8/jarvis-credentials"
tar -C "$fixture/asset" -czf "$fixture/jarvis-core-v1.2.8-linux-x86_64.tar.gz" jarvis-core-v1.2.8
(cd "$fixture" && sha256sum jarvis-core-v1.2.8-linux-x86_64.tar.gz > jarvis-core-v1.2.8-linux-x86_64.tar.gz.sha256)
if run_stage v1.2.8; then
    echo "release declaring admin helpers but missing jarvis-credentials was accepted" >&2
    exit 1
fi
[[ ! -e /opt/jarvis/releases/v1.2.8 ]]

# A legitimate release predating the capability remains structurally valid.
rm -rf -- /opt/jarvis
install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.9 v1.2.9 false
run_stage v1.2.9
[[ -x /opt/jarvis/releases/v1.2.9/jarvis ]]
[[ ! -e /opt/jarvis/releases/v1.2.9/jarvis-models ]]
jq -e '.tooling | has("admin_helpers") | not' /opt/jarvis/releases/v1.2.9/release.json >/dev/null

rm -rf -- /opt/jarvis
install -d -o root -g root -m 0755 /opt/jarvis/releases
write_asset v1.2.6 v1.2.6
mkdir -p "$fixture/leaked/jarvis-core-v1.2.6/jarvis-core"
printf 'private persona must never be published\n' > "$fixture/leaked/jarvis-core-v1.2.6/Jarvis.md"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/leaked/jarvis-core-v1.2.6/jarvis-api"
chmod 0755 "$fixture/leaked/jarvis-core-v1.2.6/jarvis-api"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/leaked/jarvis-core-v1.2.6/jarvis-config-broker"
chmod 0755 "$fixture/leaked/jarvis-core-v1.2.6/jarvis-config-broker"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/leaked/jarvis-core-v1.2.6/jarvis-codex-broker"
chmod 0755 "$fixture/leaked/jarvis-core-v1.2.6/jarvis-codex-broker"
jq -n --arg tag v1.2.6 \
  '{tag:$tag,revision:"0123456789abcdef0123456789abcdef01234567",schema_sha256:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' \
  > "$fixture/leaked/jarvis-core-v1.2.6/release.json"
tar -C "$fixture/leaked" -czf "$fixture/jarvis-core-v1.2.6-linux-x86_64.tar.gz" jarvis-core-v1.2.6
(cd "$fixture" && sha256sum jarvis-core-v1.2.6-linux-x86_64.tar.gz > jarvis-core-v1.2.6-linux-x86_64.tar.gz.sha256)
if run_stage v1.2.6; then
    echo "release containing a protected persona was accepted" >&2
    exit 1
fi
[[ ! -e /opt/jarvis/releases/v1.2.6 ]]
echo "Home Node release-staging fixture tests passed"
