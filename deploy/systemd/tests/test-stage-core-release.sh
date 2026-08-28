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
    jq -n --arg tag "$manifest_tag" \
      '{tag:$tag,revision:"0123456789abcdef0123456789abcdef01234567",schema_sha256:"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' \
      > "$asset/jarvis-core-$tag/release.json"
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
