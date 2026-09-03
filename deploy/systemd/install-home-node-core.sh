#!/usr/bin/env bash
# Install one already-built, reviewed Jarvis Core release on the Ubuntu Home Node.
# Run as root from a trusted administrator session; this script never installs
# packages, creates secrets, opens firewall ports, or grants Docker/root access to
# the Jarvis service account.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_dir=$(cd -- "$script_dir/../.." && pwd)
if [[ -r /usr/local/libexec/jarvis/ui.sh ]]; then
    # shellcheck disable=SC1091 # installed root-owned helper
    source /usr/local/libexec/jarvis/ui.sh
elif [[ -r $script_dir/ui.sh && ! -L $script_dir/ui.sh ]]; then
    # shellcheck disable=SC1091 # packaged beside the verified installer
    source "$script_dir/ui.sh"
elif [[ -r $repo_dir/deploy/lib/ui.sh ]]; then
    # shellcheck disable=SC1091 # dynamic repository root
    source "$repo_dir/deploy/lib/ui.sh"
else
    echo "missing trusted Jarvis terminal presentation helper" >&2
    exit 1
fi

usage() {
    echo "Usage: sudo $0 /opt/jarvis/releases/vMAJOR.MINOR.PATCH" >&2
    exit 64
}

validate_admin_helper_tooling() {
    local release=$1 helper metadata mode matches
    if ! jq -e '((.tooling? | type) == "object") and (.tooling | has("admin_helpers"))' \
        "$release/release.json" >/dev/null 2>&1; then
        return 0
    fi
    jq -e '(.tooling.admin_helpers | type) == "number" and .tooling.admin_helpers == 1' \
        "$release/release.json" >/dev/null 2>&1 || {
        echo "unsupported admin-helper tooling capability" >&2
        exit 1
    }
    [[ -f $release/artifact-binaries.sha256 && ! -L $release/artifact-binaries.sha256 ]] || {
        echo "admin-helper artifact checksum manifest is missing or unsafe" >&2
        exit 1
    }
    LC_ALL=C awk '
        NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._-]*$/ { exit 1 }
        END { if (NR == 0) exit 1 }
    ' "$release/artifact-binaries.sha256" || {
        echo "artifact checksum manifest is malformed" >&2
        exit 1
    }
    for helper in jarvis-models jarvis-credentials; do
        [[ -f $release/$helper && ! -L $release/$helper ]] || {
            echo "versioned admin helper is missing or unsafe: $helper" >&2
            exit 1
        }
        metadata=$(stat -c '%u:%g:%a' "$release/$helper")
        [[ $metadata == 0:0:* ]] || {
            echo "versioned admin helper is not root-owned: $helper" >&2
            exit 1
        }
        mode=${metadata##*:}
        (( (8#$mode & 0022) == 0 && (8#$mode & 0111) != 0 )) || {
            echo "versioned admin helper permissions are unsafe: $helper" >&2
            exit 1
        }
        matches=$(awk -v helper="$helper" '$2 == helper { count++ } END { print count + 0 }' \
            "$release/artifact-binaries.sha256")
        [[ $matches == 1 ]] || {
            echo "versioned admin helper is not uniquely checksum-bound: $helper" >&2
            exit 1
        }
    done
    (cd "$release" && sha256sum --check --strict artifact-binaries.sha256 >/dev/null) || {
        echo "release artifact checksum verification failed" >&2
        exit 1
    }
}

[[ ${EUID} -eq 0 ]] || { echo "must run as root" >&2; exit 1; }
[[ $# -eq 1 ]] || usage

release_dir=$(realpath -e -- "$1")
case "$release_dir" in
    /opt/jarvis/releases/*) ;;
    *) echo "release must be beneath /opt/jarvis/releases" >&2; exit 1 ;;
esac
release_tag=${release_dir##*/}
[[ $release_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "release must be a verified stable tag directory" >&2
    exit 1
}

find "$release_dir" -xdev -type l -print -quit | grep -q . && {
    echo "release must not contain symlinks" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-api" ]] || {
    echo "missing executable: $release_dir/jarvis-api" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-config-broker" ]] || {
    echo "missing executable: $release_dir/jarvis-config-broker" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-codex-broker" ]] || {
    echo "missing executable: $release_dir/jarvis-codex-broker" >&2
    exit 1
}
[[ -x "$release_dir/jarvis-agent-bundle" ]] || {
    echo "missing agent-bundle validator: $release_dir/jarvis-agent-bundle" >&2
    exit 1
}
[[ -x "$release_dir/jarvis" ]] || {
    echo "missing executable: $release_dir/jarvis" >&2
    exit 1
}
[[ -x "$release_dir/update-core-release" ]] || {
    echo "missing versioned updater helper: $release_dir/update-core-release" >&2
    exit 1
}
[[ -f /etc/jarvis/Jarvis.md && ! -L /etc/jarvis/Jarvis.md ]] || {
    echo "missing protected persona: /etc/jarvis/Jarvis.md" >&2
    exit 1
}
[[ -f "$release_dir/release.json" && ! -L "$release_dir/release.json" ]] || {
    echo "missing verified release manifest" >&2
    exit 1
}
[[ -f "$release_dir/release.verification" && ! -L "$release_dir/release.verification" ]] || {
    echo "release was not staged by the verified-release helper" >&2
    exit 1
}
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
jq -e --arg tag "$release_tag" '
    .tag == $tag and
    (.revision | strings | test("^[0-9a-f]{40}$")) and
    (.schema_sha256 | strings | test("^[0-9a-f]{64}$"))
' "$release_dir/release.json" >/dev/null || {
    echo "release manifest is invalid or mismatched" >&2
    exit 1
}
release_has_core_admin=false
release_has_private_agent_tooling=false
release_has_managed_systemd=false
if jq -e '.tooling.private_agents? == 1' "$release_dir/release.json" >/dev/null 2>&1; then
    release_has_private_agent_tooling=true
    for helper in install-agent-bundle private-agent-poll jarvis-private-update; do
        [[ -x $release_dir/$helper && ! -L $release_dir/$helper ]] || {
            echo "versioned private-agent tooling is incomplete" >&2
            exit 1
        }
    done
fi
validate_admin_helper_tooling "$release_dir"
if jq -e '((.tooling? | type) == "object") and (.tooling | has("systemd_units"))' \
    "$release_dir/release.json" >/dev/null 2>&1; then
    jq -e '(.tooling.systemd_units | type) == "number" and .tooling.systemd_units == 1' \
        "$release_dir/release.json" >/dev/null || {
        echo "unsupported managed-systemd capability" >&2
        exit 1
    }
    [[ -x $release_dir/manage-systemd-units && ! -L $release_dir/manage-systemd-units ]] || {
        echo "managed-systemd helper is missing or unsafe" >&2
        exit 1
    }
    "$release_dir/manage-systemd-units" validate-release "$release_dir"
    release_has_managed_systemd=true
fi
if jq -e '.components? != null' "$release_dir/release.json" >/dev/null; then
    jq -e '.components | [.core, .cli, .core_admin] | all(test("^[0-9]+\\.[0-9]+\\.[0-9]+$"))' \
        "$release_dir/release.json" >/dev/null || {
        echo "release component versions are invalid" >&2
        exit 1
    }
    release_has_core_admin=true
    [[ -x $release_dir/jarvis-core-admin && ! -L $release_dir/jarvis-core-admin ]] || {
        echo "missing graphical administrator binary" >&2
        exit 1
    }
    for app_file in jarvis-core-admin.desktop jarvis-core-admin.png jarvis-core-admin.version; do
        [[ -f $release_dir/$app_file && ! -L $release_dir/$app_file ]] || {
            echo "graphical administrator packaging is incomplete" >&2
            exit 1
        }
    done
    read -r packaged_app_version extra < "$release_dir/jarvis-core-admin.version" || {
        echo "graphical administrator version file is invalid" >&2
        exit 1
    }
    [[ -z ${extra:-} && $packaged_app_version == \
        "$(jq -r '.components.core_admin' "$release_dir/release.json")" ]] || {
        echo "graphical administrator version does not match release manifest" >&2
        exit 1
    }
    [[ $("$release_dir/jarvis-core-admin" --component-version) == "$packaged_app_version" ]] || {
        echo "graphical administrator executable version does not match release manifest" >&2
        exit 1
    }
fi
[[ -f /etc/jarvis/core.env && ! -L /etc/jarvis/core.env ]] || {
    echo "missing /etc/jarvis/core.env; create it from deploy/systemd/README.md first" >&2
    exit 1
}

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /var/lib/jarvis --shell /usr/sbin/nologin jarvis
fi
install -d -o jarvis -g jarvis -m 0750 /var/lib/jarvis

[[ -f /etc/jarvis/surrealdb.env && ! -L /etc/jarvis/surrealdb.env ]] || {
    echo "missing root-only /etc/jarvis/surrealdb.env" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis/surrealdb.env) == root:root:600 ]] || {
    echo "/etc/jarvis/surrealdb.env must be root:root mode 0600" >&2
    exit 1
}

# Do not "repair" a surprising secret file. The generator creates exactly this
# ownership/mode; accepting a symlink or broad mode here would weaken Core.
[[ $(stat -c '%U:%G:%a' /etc/jarvis/core.env) == root:jarvis:640 ]] || {
    echo "/etc/jarvis/core.env must be root:jarvis mode 0640" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis/Jarvis.md) == root:jarvis:640 ]] || {
    echo "/etc/jarvis/Jarvis.md must be root:jarvis mode 0640" >&2
    exit 1
}
[[ $(stat -c '%U:%G:%a' /etc/jarvis) == root:jarvis:750 ]] || {
    echo "/etc/jarvis must be root:jarvis mode 0750 so Core can traverse protected inputs" >&2
    exit 1
}

grep -qx 'JARVIS_ENVIRONMENT=production' /etc/jarvis/core.env || {
    echo "JARVIS_ENVIRONMENT must be production" >&2
    exit 1
}
grep -Eq '^JARVIS_BIND_ADDR=(127\.0\.0\.1|\[::1\]):[0-9]+$' /etc/jarvis/core.env || {
    echo "production JARVIS_BIND_ADDR must bind only 127.0.0.1 or [::1]" >&2
    exit 1
}
grep -qx 'JARVIS_AGENT_ENABLED=false' /etc/jarvis/core.env || {
    echo "agent execution must remain disabled for the initial deployment" >&2
    exit 1
}
grep -qx 'JARVIS_AGENT_CLAUDE_CODE_ENABLED=false' /etc/jarvis/core.env || {
    echo "Claude Code execution must remain disabled for the initial deployment" >&2
    exit 1
}

for required in JARVIS_SURREAL_ENDPOINT JARVIS_SURREAL_NAMESPACE JARVIS_SURREAL_DATABASE JARVIS_SURREAL_USERNAME JARVIS_SURREAL_PASSWORD; do
    value=$(sed -n "s/^${required}=//p" /etc/jarvis/core.env | tail -n 1)
    [[ -n ${value:-} ]] || { echo "$required must be set" >&2; exit 1; }
done

hops=$(sed -n 's/^JARVIS_TRUSTED_PROXY_HOPS=//p' /etc/jarvis/core.env | tail -n 1)
ips=$(sed -n 's/^JARVIS_TRUSTED_PROXY_IPS=//p' /etc/jarvis/core.env | tail -n 1)
if [[ ${hops:-0} != 0 && -z ${ips:-} ]]; then
    echo "trusted proxy hops require JARVIS_TRUSTED_PROXY_IPS" >&2
    exit 1
fi
if [[ ${hops:-0} != 0 && ${hops:-0} != 1 ]]; then
    echo "the single local Caddy ingress requires JARVIS_TRUSTED_PROXY_HOPS=1" >&2
    exit 1
fi

unit_backup=
if [[ $release_has_managed_systemd == true ]]; then
    unit_backup=$(mktemp -d /run/jarvis-systemd-install-rollback.XXXXXXXX)
    chmod 0700 "$unit_backup"
    "$release_dir/manage-systemd-units" install "$release_dir" "$unit_backup"
else
    # Compatibility only for verified historical releases. New releases are
    # self-contained and never source production units from this checkout.
    for unit in jarvis-core.service jarvis-config-broker.service jarvis-codex-broker.service \
        jarvis-codex.service jarvis-opensandbox.service jarvis-surrealdb.service \
        jarvis-updater.service jarvis-updater.timer \
        jarvis-private-agent-updater.service jarvis-private-agent-updater.timer; do
        install -o root -g root -m 0644 "$repo_dir/deploy/systemd/$unit" "/etc/systemd/system/$unit"
    done
fi
install -d -o root -g root -m 0755 /usr/local/libexec/jarvis
if [[ $release_has_managed_systemd == true ]]; then
    install -o root -g root -m 0644 "$release_dir/ui.sh" /usr/local/libexec/jarvis/ui.sh
else
    install -o root -g root -m 0644 "$repo_dir/deploy/lib/ui.sh" /usr/local/libexec/jarvis/ui.sh
fi
install -o root -g root -m 0755 \
    "$release_dir/jarvis-agent-bundle" \
    /usr/local/libexec/jarvis/jarvis-agent-bundle
install -o root -g root -m 0755 \
    "$release_dir/update-core-release" \
    /usr/local/libexec/jarvis/update-core-release
# The administrative surface is shipped with the verified release.  Do not
# install a checkout-owned shell dispatcher at this canonical owner path.
install -o root -g root -m 0755 \
    "$release_dir/jarvis" \
    /usr/local/sbin/jarvis
if [[ $release_has_private_agent_tooling == true ]]; then
    install -o root -g root -m 0755 "$release_dir/install-agent-bundle" \
        /usr/local/libexec/jarvis/install-agent-bundle
    install -o root -g root -m 0755 "$release_dir/private-agent-poll" \
        /usr/local/libexec/jarvis/private-agent-poll
    install -o root -g root -m 0755 "$release_dir/jarvis-private-update" \
        /usr/local/sbin/jarvis-private-update
fi
if [[ $release_has_core_admin == true ]]; then
    install -d -o root -g root -m 0755 /usr/share/jarvis-core-admin \
        /usr/share/applications /usr/share/icons/hicolor/128x128/apps
    install -o root -g root -m 0755 "$release_dir/jarvis-core-admin" /usr/bin/jarvis-core-admin
    install -o root -g root -m 0644 "$release_dir/jarvis-core-admin.desktop" \
        /usr/share/applications/com.hawkeynl.jarvis.core.admin.desktop
    install -o root -g root -m 0644 "$release_dir/jarvis-core-admin.png" \
        /usr/share/icons/hicolor/128x128/apps/jarvis-core-admin.png
    install -o root -g root -m 0644 "$release_dir/jarvis-core-admin.version" \
        /usr/share/jarvis-core-admin/version
    if [[ -f /usr/share/applications/jarvis-core-admin.desktop && \
        ! -L /usr/share/applications/jarvis-core-admin.desktop ]]; then
        rm -f -- /usr/share/applications/jarvis-core-admin.desktop
    fi
    /usr/bin/update-desktop-database /usr/share/applications >/dev/null 2>&1 || true
    /usr/bin/gtk-update-icon-cache --force --ignore-theme-index \
        /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
if [[ $release_has_managed_systemd == true ]]; then
    install -o root -g root -m 0755 "$release_dir/manage-systemd-units" \
        /usr/local/libexec/jarvis/manage-systemd-units
    install -o root -g root -m 0755 "$release_dir/verify-home-node" \
        /usr/local/libexec/jarvis/verify-home-node
else
    install -o root -g root -m 0755 "$repo_dir/deploy/systemd/verify-home-node.sh" \
        /usr/local/libexec/jarvis/verify-home-node
fi

# A release is immutable: the unprivileged service cannot modify its binary or
# Core persona even if an application-level control were bypassed.
chown -R root:root "$release_dir"
chmod -R go-w "$release_dir"

previous_release=
if [[ -L /opt/jarvis/current ]]; then
    previous_release=$(readlink -f /opt/jarvis/current)
    [[ $previous_release == /opt/jarvis/releases/* && -d $previous_release ]] || {
        echo "existing active release is outside the managed release root" >&2
        exit 1
    }
fi
activate_release() {
    local target=$1 temporary=/opt/jarvis/.current.new
    rm -f -- "$temporary"
    ln -s "$target" "$temporary"
    mv -Tf "$temporary" /opt/jarvis/current
}
restore_previous_release() {
    ui_warning "Restoring previous release and managed unit policy"
    if [[ -n $previous_release ]]; then
        activate_release "$previous_release"
    else
        rm -f -- /opt/jarvis/current
    fi
    if [[ $release_has_managed_systemd == true && -n $unit_backup ]]; then
        "$release_dir/manage-systemd-units" restore "$release_dir" "$unit_backup"
        systemctl daemon-reload
    fi
    systemctl try-restart jarvis-config-broker.service >/dev/null 2>&1 || true
    systemctl try-restart jarvis-core.service >/dev/null 2>&1 || true
}

systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/jarvis-core.service
systemd-analyze verify /etc/systemd/system/jarvis-config-broker.service
systemd-analyze verify /etc/systemd/system/jarvis-codex-broker.service
systemd-analyze verify /etc/systemd/system/jarvis-codex.service
systemd-analyze verify /etc/systemd/system/jarvis-opensandbox.service
systemd-analyze verify /etc/systemd/system/jarvis-surrealdb.service
systemd-analyze verify /etc/systemd/system/jarvis-updater.service
systemd-analyze verify /etc/systemd/system/jarvis-updater.timer
systemd-analyze verify /etc/systemd/system/jarvis-private-agent-updater.service
systemd-analyze verify /etc/systemd/system/jarvis-private-agent-updater.timer
ui_detail "Starting SurrealDB and Jarvis Core …"
if ! systemctl enable --now jarvis-surrealdb.service; then
    ui_error "SurrealDB could not start; active release was not changed"
    if [[ $release_has_managed_systemd == true && -n $unit_backup ]]; then
        "$release_dir/manage-systemd-units" restore "$release_dir" "$unit_backup"
        systemctl daemon-reload
        rm -rf -- "$unit_backup"
    fi
    exit 1
fi

# The symlink transition is atomic. From here every failed required service or
# readiness check restores the previous known-good release before returning.
activate_release "$release_dir"
if ! systemctl enable --now jarvis-config-broker.service; then
    ui_error "Config broker could not start"
    restore_previous_release
    echo "Hint: sudo jarvis logs config-broker" >&2
    exit 1
fi
if ! systemctl enable --now jarvis-core.service; then
    ui_error "Jarvis Core could not start"
    restore_previous_release
    echo "Hint: sudo jarvis logs core" >&2
    exit 1
fi

core_ready=false
for _attempt in $(seq 1 15); do
    if curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/livez >/dev/null \
        && curl --fail --silent --show-error --max-time 2 http://127.0.0.1:8080/readyz >/dev/null; then
        core_ready=true
        break
    fi
    if systemctl is-failed --quiet jarvis-core.service; then
        break
    fi
    sleep 1
done
if [[ $core_ready != true ]]; then
    ui_error "Jarvis Core did not become ready; recent service diagnostics follow:"
    systemctl --no-pager --full status jarvis-core.service || true
    journalctl --no-pager -u jarvis-core.service -n 80 || true
    restore_previous_release
    echo "Hint: sudo jarvis logs core" >&2
    exit 1
fi
ui_success "Jarvis Core active"
ui_success "/livez ready"
ui_success "/readyz ready"
if [[ $release_has_managed_systemd == true ]]; then
    "$release_dir/manage-systemd-units" check-installed "$release_dir"
    rm -rf -- "$unit_backup"
fi
[[ ${JARVIS_VERBOSE:-0} == 1 ]] && systemctl --no-pager --full status jarvis-core
ui_detail "Jarvis Core is running from $release_dir"
