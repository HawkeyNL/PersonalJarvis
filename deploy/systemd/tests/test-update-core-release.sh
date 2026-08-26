#!/usr/bin/env bash
# Linux-only CI fixture for the privileged Home Node updater. It runs the real
# updater with fake GitHub/systemd commands and never contacts the network.
set -euo pipefail

[[ ${GITHUB_ACTIONS:-} == true ]] || {
    echo "refusing to run outside GitHub Actions" >&2
    exit 1
}
[[ ${EUID} -eq 0 ]] || {
    echo "must run as root (use sudo in CI)" >&2
    exit 1
}

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
updater="$repo_dir/deploy/systemd/update-core-release.sh"
fixture_dir=$(mktemp -d)
fake_bin="$fixture_dir/bin"
mkdir -p "$fake_bin"

cleanup() {
    rm -rf -- "$fixture_dir" /opt/jarvis
}
trap cleanup EXIT

cat > "$fake_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ ${1:-} == restart && ${2:-} == jarvis-core.service ]] || exit 1
EOF
chmod 0755 "$fake_bin/systemctl"

cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

output=
url=
while (($#)); do
    case "$1" in
        -o)
            output=$2
            shift 2
            ;;
        *)
            [[ $1 == http://* || $1 == https://* ]] && url=$1
            shift
            ;;
    esac
done
[[ -n $url ]] || { echo "curl invocation lacks a URL" >&2; exit 1; }

case "$url" in
    */releases/latest)
        cat "$JARVIS_UPDATER_FIXTURE/metadata.json"
        ;;
    *.tar.gz.sha256)
        cp "$JARVIS_UPDATER_FIXTURE/${url##*/}" "$output"
        ;;
    *.tar.gz)
        cp "$JARVIS_UPDATER_FIXTURE/${url##*/}" "$output"
        ;;
    http://127.0.0.1:8080/readyz)
        [[ ${JARVIS_UPDATER_READYZ_FAIL:-false} != true ]] || exit 1
        exit 0
        ;;
    *)
        echo "unexpected curl URL: $url" >&2
        exit 1
        ;;
esac
EOF
chmod 0755 "$fake_bin/curl"

write_release() {
    local root=$1
    local tag=$2
    local schema_sha256=$3
    mkdir -p "$root/jarvis-core-$tag"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-api"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-api"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-agent-bundle"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-agent-bundle"
    jq -n \
        --arg tag "$tag" \
        --arg schema_sha256 "$schema_sha256" \
        '{tag: $tag, revision: "0123456789abcdef0123456789abcdef01234567", schema_sha256: $schema_sha256}' \
        > "$root/jarvis-core-$tag/release.json"
}

seed_active_release() {
    local tag=$1
    local schema_sha256=$2
    rm -rf -- /opt/jarvis
    install -d -o root -g root -m 0755 /opt/jarvis/releases
    write_release /opt/jarvis/releases "$tag" "$schema_sha256"
    mv "/opt/jarvis/releases/jarvis-core-$tag" "/opt/jarvis/releases/$tag"
    ln -s "/opt/jarvis/releases/$tag" /opt/jarvis/current
    install -d -o root -g root -m 0750 /etc/jarvis
    printf 'synthetic protected persona\n' > /etc/jarvis/Jarvis.md
    chown root:root /etc/jarvis/Jarvis.md
    chmod 0600 /etc/jarvis/Jarvis.md
}

prepare_candidate() {
    local tag=$1
    local schema_sha256=$2
    local asset_root="$fixture_dir/asset"
    local artifact="jarvis-core-$tag-linux-x86_64.tar.gz"
    rm -rf -- "$asset_root"
    mkdir -p "$asset_root"
    write_release "$asset_root" "$tag" "$schema_sha256"
    rm -f -- "$fixture_dir"/*.tar.gz "$fixture_dir"/*.tar.gz.sha256
    tar -C "$asset_root" -czf "$fixture_dir/$artifact" "jarvis-core-$tag"
    (
        cd "$fixture_dir"
        sha256sum "$artifact" > "$artifact.sha256"
    )
    jq -n --arg tag "$tag" '
        {
          tag_name: $tag,
          draft: false,
          prerelease: false,
          assets: [
            {
              name: ("jarvis-core-" + $tag + "-linux-x86_64.tar.gz"),
              browser_download_url: ("https://github.com/HawkeyNL/PersonalJarvis/releases/download/" + $tag + "/jarvis-core-" + $tag + "-linux-x86_64.tar.gz")
            },
            {
              name: ("jarvis-core-" + $tag + "-linux-x86_64.tar.gz.sha256"),
              browser_download_url: ("https://github.com/HawkeyNL/PersonalJarvis/releases/download/" + $tag + "/jarvis-core-" + $tag + "-linux-x86_64.tar.gz.sha256")
            }
          ]
        }
    ' > "$fixture_dir/metadata.json"
}

run_updater() {
    PATH="$fake_bin:$PATH" \
        JARVIS_UPDATER_FIXTURE="$fixture_dir" \
        JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis \
        JARVIS_UPDATER_READYZ_FAIL="${JARVIS_UPDATER_READYZ_FAIL:-false}" \
        bash "$updater"
}

same_migrations=$(printf 'a%.0s' {1..64})
changed_migrations=$(printf 'b%.0s' {1..64})

seed_active_release v1.0.0 "$same_migrations"
prepare_candidate v1.0.1 "$same_migrations"
run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v1.0.1 ]]
[[ -f /opt/jarvis/releases/v1.0.1/release.json ]]

seed_active_release v2.0.0 "$same_migrations"
prepare_candidate v2.0.1 "$changed_migrations"
if run_updater; then
    echo "migration-changing update unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v2.0.0 ]]
[[ ! -e /opt/jarvis/releases/v2.0.1 ]]

# A locally built initial release is not a safe baseline for the timer because
# it cannot prove migration compatibility with a future binary.
seed_active_release v3.0.0 "$same_migrations"
rm /opt/jarvis/releases/v3.0.0/release.json
prepare_candidate v3.0.1 "$same_migrations"
if run_updater; then
    echo "manifest-less baseline unexpectedly updated" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v3.0.0 ]]
[[ ! -e /opt/jarvis/releases/v3.0.1 ]]

# A readiness failure after activation must restore the previous Core binary.
seed_active_release v4.0.0 "$same_migrations"
prepare_candidate v4.0.1 "$same_migrations"
if JARVIS_UPDATER_READYZ_FAIL=true run_updater; then
    echo "update with failed readiness unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v4.0.0 ]]
[[ -e /opt/jarvis/releases/v4.0.1 ]]

# The timer must never replace a newer active binary with an older release.
seed_active_release v5.0.1 "$same_migrations"
prepare_candidate v5.0.0 "$same_migrations"
run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v5.0.1 ]]
[[ ! -e /opt/jarvis/releases/v5.0.0 ]]

echo "Home Node updater fixture tests passed"
