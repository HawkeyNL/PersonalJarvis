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
    rm -f -- /etc/jarvis/updater.env
    rm -rf -- /usr/local/sbin/jarvis
    rm -f -- /usr/local/sbin/jarvis-private-update \
        /usr/local/libexec/jarvis/update-core-release \
        /usr/local/libexec/jarvis/jarvis-agent-bundle \
        /usr/local/libexec/jarvis/install-agent-bundle \
        /usr/local/libexec/jarvis/private-agent-poll
    rm -f -- /usr/bin/jarvis-core-admin \
        /usr/share/applications/jarvis-core-admin.desktop \
        /usr/share/icons/hicolor/128x128/apps/jarvis-core-admin.png
    rm -rf -- /usr/share/jarvis-core-admin
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
    */releases/latest|*/releases/tags/*)
        cat "$JARVIS_UPDATER_FIXTURE/metadata.json"
        ;;
    *.tar.gz.sha256|*-components.json|*-components.json.sha256)
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
    local core_version=${4:-${tag#v}}
    local cli_version=${5:-${tag#v}}
    local app_version=${6:-${tag#v}}
    mkdir -p "$root/jarvis-core-$tag"
    chmod 0755 "$root/jarvis-core-$tag"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-api"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-api"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-config-broker"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-config-broker"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-codex-broker"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-codex-broker"
    printf '#!/usr/bin/env bash\nexit 0\n' > "$root/jarvis-core-$tag/jarvis-agent-bundle"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-agent-bundle"
    printf '#!/usr/bin/env bash\nprintf "Jarvis admin %s\\n"\n' "$tag" > "$root/jarvis-core-$tag/jarvis"
    chmod 0755 "$root/jarvis-core-$tag/jarvis"
    printf '#!/usr/bin/env bash\n[[ ${1:-} == --component-version ]] && { printf "%%s\\n" "%s"; exit 0; }\nexit 0\n' \
        "$app_version" > "$root/jarvis-core-$tag/jarvis-core-admin"
    chmod 0755 "$root/jarvis-core-$tag/jarvis-core-admin"
    printf '[Desktop Entry]\nType=Application\nName=Jarvis Core Administration\nExec=/usr/bin/jarvis-core-admin\n' \
        > "$root/jarvis-core-$tag/jarvis-core-admin.desktop"
    printf 'synthetic png fixture\n' > "$root/jarvis-core-$tag/jarvis-core-admin.png"
    printf '%s\n' "$app_version" > "$root/jarvis-core-$tag/jarvis-core-admin.version"
    chmod 0644 "$root/jarvis-core-$tag/jarvis-core-admin.desktop" \
        "$root/jarvis-core-$tag/jarvis-core-admin.png" \
        "$root/jarvis-core-$tag/jarvis-core-admin.version"
    cp "$updater" "$root/jarvis-core-$tag/update-core-release"
    printf '\n# verified release tooling: %s\n' "$tag" >> "$root/jarvis-core-$tag/update-core-release"
    chmod 0755 "$root/jarvis-core-$tag/update-core-release"
    for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
        printf '#!/usr/bin/env bash\n# verified %s tooling: %s\nexit 0\n' "$helper" "$tag" \
            > "$root/jarvis-core-$tag/$helper"
        chmod 0755 "$root/jarvis-core-$tag/$helper"
    done
    jq -n \
        --arg tag "$tag" \
        --arg schema_sha256 "$schema_sha256" \
        --arg core_version "$core_version" \
        --arg cli_version "$cli_version" \
        --arg app_version "$app_version" \
        '{tag: $tag, revision: "0123456789abcdef0123456789abcdef01234567", schema_sha256: $schema_sha256, components: {core: $core_version, cli: $cli_version, core_admin: $app_version}, tooling: {private_agents: 1}}' \
        > "$root/jarvis-core-$tag/release.json"
    chmod 0644 "$root/jarvis-core-$tag/release.json"
    printf '%064d  jarvis-core-%s-linux-x86_64.tar.gz\n' 0 "$tag" \
        > "$root/jarvis-core-$tag/release.verification"
    chmod 0644 "$root/jarvis-core-$tag/release.verification"
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
    install -d -o root -g root -m 0755 /usr/share/jarvis-core-admin \
        /usr/share/applications /usr/share/icons/hicolor/128x128/apps
    install -o root -g root -m 0755 "/opt/jarvis/releases/$tag/jarvis-core-admin" /usr/bin/jarvis-core-admin
    install -o root -g root -m 0644 "/opt/jarvis/releases/$tag/jarvis-core-admin.desktop" \
        /usr/share/applications/jarvis-core-admin.desktop
    install -o root -g root -m 0644 "/opt/jarvis/releases/$tag/jarvis-core-admin.png" \
        /usr/share/icons/hicolor/128x128/apps/jarvis-core-admin.png
    install -o root -g root -m 0644 "/opt/jarvis/releases/$tag/jarvis-core-admin.version" \
        /usr/share/jarvis-core-admin/version
    install -d -o root -g root -m 0755 /usr/local/libexec/jarvis /usr/local/sbin
    install -o root -g root -m 0755 "/opt/jarvis/releases/$tag/jarvis-agent-bundle" \
        /usr/local/libexec/jarvis/jarvis-agent-bundle
    install -o root -g root -m 0755 "/opt/jarvis/releases/$tag/install-agent-bundle" \
        /usr/local/libexec/jarvis/install-agent-bundle
    install -o root -g root -m 0755 "/opt/jarvis/releases/$tag/private-agent-poll" \
        /usr/local/libexec/jarvis/private-agent-poll
    install -o root -g root -m 0755 "/opt/jarvis/releases/$tag/jarvis-private-update" \
        /usr/local/sbin/jarvis-private-update
}

prepare_candidate() {
    local tag=$1
    local schema_sha256=$2
    local core_version=${3:-${tag#v}}
    local cli_version=${4:-${tag#v}}
    local app_version=${5:-${tag#v}}
    local asset_root="$fixture_dir/asset"
    local artifact="jarvis-core-$tag-linux-x86_64.tar.gz"
    rm -rf -- "$asset_root"
    mkdir -p "$asset_root"
    write_release "$asset_root" "$tag" "$schema_sha256" "$core_version" "$cli_version" "$app_version"
    # Published artifacts do not carry an installation marker. The updater
    # must create it only after validating the downloaded archive checksum.
    rm -f -- "$asset_root/jarvis-core-$tag/release.verification"
    rm -f -- "$fixture_dir"/*.tar.gz "$fixture_dir"/*.tar.gz.sha256 \
        "$fixture_dir"/*-components.json "$fixture_dir"/*-components.json.sha256
    tar -C "$asset_root" -czf "$fixture_dir/$artifact" "jarvis-core-$tag"
    (
        cd "$fixture_dir"
        sha256sum "$artifact" > "$artifact.sha256"
    )
    jq '{tag, revision, components}' "$asset_root/jarvis-core-$tag/release.json" \
        > "$fixture_dir/jarvis-core-$tag-components.json"
    (
        cd "$fixture_dir"
        sha256sum "jarvis-core-$tag-components.json" > "jarvis-core-$tag-components.json.sha256"
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
            },
            {
              name: ("jarvis-core-" + $tag + "-components.json"),
              browser_download_url: ("https://github.com/HawkeyNL/PersonalJarvis/releases/download/" + $tag + "/jarvis-core-" + $tag + "-components.json")
            },
            {
              name: ("jarvis-core-" + $tag + "-components.json.sha256"),
              browser_download_url: ("https://github.com/HawkeyNL/PersonalJarvis/releases/download/" + $tag + "/jarvis-core-" + $tag + "-components.json.sha256")
            }
          ]
        }
    ' > "$fixture_dir/metadata.json"
}

run_updater() {
    PATH="$fake_bin:$PATH" \
        JARVIS_UPDATER_FIXTURE="$fixture_dir" \
        JARVIS_UPDATER_READYZ_FAIL="${JARVIS_UPDATER_READYZ_FAIL:-false}" \
        bash "$updater" "$@"
}

same_migrations=$(printf 'a%.0s' {1..64})
changed_migrations=$(printf 'b%.0s' {1..64})

# Existing markers are validated, not trusted merely because the filename is
# present. A corrupted marker blocks a privileged mutation.
seed_active_release v0.9.0 "$same_migrations"
printf 'forged marker\n' > /opt/jarvis/releases/v0.9.0/release.verification
prepare_candidate v0.9.1 "$same_migrations"
if run_updater; then
    echo "corrupted release marker unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v0.9.0 ]]

# A realistic legacy layout has an old shell dispatcher/updater outside the
# active release. A successful activation must replace both from the already
# verified candidate, never from a checkout or inherited environment.
install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
printf 'legacy admin tooling\n' > /usr/local/sbin/jarvis
printf 'legacy updater tooling\n' > /usr/local/libexec/jarvis/update-core-release
chmod 0755 /usr/local/sbin/jarvis /usr/local/libexec/jarvis/update-core-release
seed_active_release v1.0.0 "$same_migrations"
prepare_candidate v1.0.1 "$same_migrations"
run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v1.0.1 ]]
[[ -f /opt/jarvis/releases/v1.0.1/release.json ]]
[[ -f /opt/jarvis/releases/v1.0.1/release.verification ]]
grep -Eq "^[0-9a-f]{64}  jarvis-core-v1.0.1-linux-x86_64.tar.gz$" /opt/jarvis/releases/v1.0.1/release.verification
cmp /opt/jarvis/releases/v1.0.1/jarvis /usr/local/sbin/jarvis
cmp /opt/jarvis/releases/v1.0.1/update-core-release /usr/local/libexec/jarvis/update-core-release
cmp /opt/jarvis/releases/v1.0.1/jarvis-agent-bundle /usr/local/libexec/jarvis/jarvis-agent-bundle
cmp /opt/jarvis/releases/v1.0.1/install-agent-bundle /usr/local/libexec/jarvis/install-agent-bundle
cmp /opt/jarvis/releases/v1.0.1/private-agent-poll /usr/local/libexec/jarvis/private-agent-poll
cmp /opt/jarvis/releases/v1.0.1/jarvis-private-update /usr/local/sbin/jarvis-private-update
cmp /opt/jarvis/releases/v1.0.1/jarvis-core-admin /usr/bin/jarvis-core-admin
cmp /opt/jarvis/releases/v1.0.1/jarvis-core-admin.version /usr/share/jarvis-core-admin/version
[[ $(stat -c '%U:%G:%a' /etc/jarvis/updater.env) == root:root:600 ]]
grep -qx 'JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis' /etc/jarvis/updater.env
grep -qx 'JARVIS_UPDATE_CHANNEL=stable' /etc/jarvis/updater.env

# A shell-supplied repository must not redirect a configured root update.
seed_active_release v1.1.0 "$same_migrations"
prepare_candidate v1.1.1 "$same_migrations"
JARVIS_UPDATE_REPOSITORY=attacker/redirect run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v1.1.1 ]]

# Malformed trusted configuration fails closed rather than falling back to an
# inherited environment. Restore the canonical config for later fixtures.
printf 'JARVIS_UPDATE_REPOSITORY=attacker/redirect;id\n' > /etc/jarvis/updater.env
chmod 0600 /etc/jarvis/updater.env
seed_active_release v1.2.0 "$same_migrations"
prepare_candidate v1.2.1 "$same_migrations"
if run_updater; then
    echo "corrupted updater config unexpectedly succeeded" >&2
    exit 1
fi
printf 'JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis\nJARVIS_UPDATE_CHANNEL=stable\n' > /etc/jarvis/updater.env
chmod 0600 /etc/jarvis/updater.env

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
cp /usr/local/sbin/jarvis "$fixture_dir/admin-before-readiness-failure"
cp /usr/local/libexec/jarvis/update-core-release "$fixture_dir/updater-before-readiness-failure"
if JARVIS_UPDATER_READYZ_FAIL=true run_updater; then
    echo "update with failed readiness unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v4.0.0 ]]
[[ -e /opt/jarvis/releases/v4.0.1 ]]
cmp "$fixture_dir/admin-before-readiness-failure" /usr/local/sbin/jarvis
cmp "$fixture_dir/updater-before-readiness-failure" /usr/local/libexec/jarvis/update-core-release

# A tooling activation failure after candidate readiness must restore Core and
# both canonical tools, never leave a mixed-version installation.
seed_active_release v4.1.0 "$same_migrations"
prepare_candidate v4.1.1 "$same_migrations"
cp /usr/local/libexec/jarvis/update-core-release "$fixture_dir/updater-before-tooling-failure"
rm -f -- /usr/local/sbin/jarvis
mkdir /usr/local/sbin/jarvis
if run_updater; then
    echo "update with failed tooling activation unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v4.1.0 ]]
cmp "$fixture_dir/updater-before-tooling-failure" /usr/local/libexec/jarvis/update-core-release
rm -rf -- /usr/local/sbin/jarvis
printf 'restored synthetic admin\n' > /usr/local/sbin/jarvis
chmod 0755 /usr/local/sbin/jarvis

# The timer must never replace a newer active binary with an older release.
seed_active_release v5.0.1 "$same_migrations"
prepare_candidate v5.0.0 "$same_migrations"
run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v5.0.1 ]]
[[ ! -e /opt/jarvis/releases/v5.0.0 ]]
if run_updater --version v5.0.0; then
    echo "explicit downgrade unexpectedly succeeded" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v5.0.1 ]]

# A check is strictly non-mutating and uses a useful exit status for scripts.
# This candidate changes only the graphical administrator component version;
# it must still be visible as an update for the newer immutable bundle tag.
seed_active_release v6.0.0 "$same_migrations"
prepare_candidate v6.0.1 "$same_migrations" 6.0.0 6.0.0 6.0.1
check_status=0
check_output=$(run_updater --check) || check_status=$?
[[ $check_status == 2 ]]
grep -Fq 'Core current: 6.0.0' <<< "$check_output"
grep -Fq 'Core latest: 6.0.0' <<< "$check_output"
grep -Fq 'CLI latest: 6.0.0' <<< "$check_output"
grep -Fq 'Core app latest: 6.0.1' <<< "$check_output"
grep -Fq 'Update:   available' <<< "$check_output"
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v6.0.0 ]]
run_updater
grep -qx '6.0.1' /usr/share/jarvis-core-admin/version
cmp /opt/jarvis/releases/v6.0.1/jarvis-core-admin /usr/bin/jarvis-core-admin

# Explicit versions still use the same published-release metadata and verified
# artifact path; they are not raw tags or arbitrary URLs.
seed_active_release v7.0.0 "$same_migrations"
prepare_candidate v7.0.1 "$same_migrations"
run_updater --version v7.0.1
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v7.0.1 ]]

# Rollback may only select an already verified release below the managed root.
seed_active_release v8.0.1 "$same_migrations"
write_release /opt/jarvis/releases v8.0.0 "$same_migrations"
mv /opt/jarvis/releases/jarvis-core-v8.0.0 /opt/jarvis/releases/v8.0.0
rollback_candidates=$(run_updater --rollback-candidates)
jq -e '.[] | select(.version == "v8.0.0" and .verified == true and .rollback_capable == true)' \
    <<< "$rollback_candidates" >/dev/null || {
    echo "expected v8.0.0 rollback candidate is unavailable: $rollback_candidates" >&2
    exit 1
}
run_updater --rollback
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.0.0 ]]
cmp /opt/jarvis/releases/v8.0.0/jarvis /usr/local/sbin/jarvis
cmp /opt/jarvis/releases/v8.0.0/update-core-release /usr/local/libexec/jarvis/update-core-release

# A release installed by the automatic path must itself remain an eligible,
# version-consistent rollback target after a later successful update.
seed_active_release v8.1.0 "$same_migrations"
prepare_candidate v8.1.1 "$same_migrations"
run_updater
prepare_candidate v8.1.2 "$same_migrations"
run_updater
run_updater --rollback
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.1.1 ]]
cmp /opt/jarvis/releases/v8.1.1/jarvis /usr/local/sbin/jarvis
cmp /opt/jarvis/releases/v8.1.1/update-core-release /usr/local/libexec/jarvis/update-core-release

# The first updater carrying verification markers must migrate releases that
# were checksum-verified by the legacy updater but lack its persisted marker.
# Both the current release and the immediate rollback target are revalidated;
# arbitrary older directories are never promoted by this compatibility path.
seed_active_release v8.2.1 "$same_migrations"
write_release /opt/jarvis/releases v8.2.0 "$same_migrations"
mv /opt/jarvis/releases/jarvis-core-v8.2.0 /opt/jarvis/releases/v8.2.0
rm -f -- /opt/jarvis/releases/v8.2.1/release.verification \
    /opt/jarvis/releases/v8.2.0/release.verification
run_updater --rollback
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.2.0 ]]
grep -Eq '^legacy-active-release-manifest [0-9a-f]{64}$' \
    /opt/jarvis/releases/v8.2.1/release.verification
grep -Eq '^legacy-active-release-manifest [0-9a-f]{64}$' \
    /opt/jarvis/releases/v8.2.0/release.verification
cmp /opt/jarvis/releases/v8.2.0/jarvis /usr/local/sbin/jarvis
cmp /opt/jarvis/releases/v8.2.0/update-core-release /usr/local/libexec/jarvis/update-core-release

# Candidate enumeration is read-only and never promotes malformed legacy
# directories. Invalid history also cannot mask a valid rollback target or
# block a later unrelated update.
seed_active_release v8.3.2 "$same_migrations"
write_release /opt/jarvis/releases v8.3.1 "$same_migrations"
mv /opt/jarvis/releases/jarvis-core-v8.3.1 /opt/jarvis/releases/v8.3.1
write_release /opt/jarvis/releases v99.0.0 "$same_migrations"
mv /opt/jarvis/releases/jarvis-core-v99.0.0 /opt/jarvis/releases/v99.0.0
printf 'forged marker\n' > /opt/jarvis/releases/v99.0.0/release.verification
candidates=$(run_updater --rollback-candidates)
jq -e '.[] | select(.version == "v8.3.2" and .current == true and .rollback_capable == false)' \
    <<< "$candidates" >/dev/null
jq -e '.[] | select(.version == "v8.3.1" and .verified == true and .rollback_capable == true)' \
    <<< "$candidates" >/dev/null
jq -e '.[] | select(.version == "v99.0.0" and .verified == false and .rollback_capable == false and (.reason | length > 0))' \
    <<< "$candidates" >/dev/null
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.3.2 ]]
if run_updater --rollback-version v99.0.0; then
    echo "invalid explicit rollback candidate unexpectedly succeeded" >&2
    exit 1
fi
run_updater --rollback-version v8.3.1
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.3.1 ]]
prepare_candidate v8.3.3 "$same_migrations"
run_updater
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v8.3.3 ]]

# Drafts and prereleases are never update targets even when their tag/assets
# appear structurally valid.
seed_active_release v9.0.0 "$same_migrations"
prepare_candidate v9.0.1 "$same_migrations"
jq '.draft = true' "$fixture_dir/metadata.json" > "$fixture_dir/metadata.tmp"
mv "$fixture_dir/metadata.tmp" "$fixture_dir/metadata.json"
if run_updater; then
    echo "draft release was accepted" >&2
    exit 1
fi
[[ $(readlink -f /opt/jarvis/current) == /opt/jarvis/releases/v9.0.0 ]]

echo "Home Node updater fixture tests passed"
