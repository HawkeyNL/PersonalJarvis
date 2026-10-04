#!/usr/bin/env bash
# Static Linux fixture for a cold boot: /run is empty before systemd creates
# declared RuntimeDirectory paths. It validates the service contracts without
# attempting to start systemd inside GitHub Actions.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
broker="$repo_dir/deploy/systemd/jarvis-config-broker.service"
prepare="$repo_dir/deploy/systemd/prepare-home-node.sh"

grep -Fq 'RuntimeDirectory=jarvis-config-broker' "$broker"
grep -Fq 'RuntimeDirectoryMode=0750' "$broker"
grep -Fq 'StateDirectory=jarvis/config-broker' "$broker"
grep -Fq 'StateDirectoryMode=0700' "$broker"
grep -Fxq 'ReadWritePaths=/etc/jarvis/model-policy' "$broker"
if grep -Eq '^ReadWritePaths=.*(/run/jarvis-config-broker|/var/lib/jarvis/config-broker)' "$broker"; then
    echo "config broker runtime and state directories must not be ReadWritePaths" >&2
    exit 1
fi
if grep -Fq 'mkdir /run/jarvis-config-broker' "$prepare"; then
    echo "config broker runtime directory must be systemd-managed" >&2
    exit 1
fi

# The daily backup is an owner opt-in: only the timer is installable, and the
# service is static so it never follows Core restarts or reads as enabled.
backup_service="$repo_dir/deploy/systemd/jarvis-backup.service"
backup_timer="$repo_dir/deploy/systemd/jarvis-backup.timer"
grep -Fxq '[Install]' "$backup_timer"
grep -Fxq 'WantedBy=timers.target' "$backup_timer"
grep -Fxq 'Persistent=true' "$backup_timer"
if grep -Eq '^(\[Install\]|PartOf=|WantedBy=)' "$backup_service"; then
    echo "jarvis-backup.service must stay static and independent of Core" >&2
    exit 1
fi
grep -Eq '^TimeoutStartSec=' "$backup_service"
grep -Fxq 'Restart=on-failure' "$backup_service"
grep -Fxq 'StartLimitBurst=3' "$backup_service"
# Housekeeping follows the same opt-in shape, runs at idle priority and is
# inert (not failed) after a rollback to a release without its helper.
housekeeping_service="$repo_dir/deploy/systemd/jarvis-housekeeping.service"
housekeeping_timer="$repo_dir/deploy/systemd/jarvis-housekeeping.timer"
grep -Fxq 'WantedBy=timers.target' "$housekeeping_timer"
grep -Fxq 'Persistent=true' "$housekeeping_timer"
if grep -Eq '^(\[Install\]|PartOf=|WantedBy=)' "$housekeeping_service"; then
    echo "jarvis-housekeeping.service must stay static and independent of Core" >&2
    exit 1
fi
for directive in 'ConditionPathExists=/opt/jarvis/current/jarvis-housekeeping' 'IOSchedulingClass=best-effort' \
    'Nice=19' 'ProtectSystem=strict' 'PrivateNetwork=true' 'RestrictAddressFamilies=AF_UNIX' \
    'CapabilityBoundingSet=CAP_DAC_READ_SEARCH' 'Restart=on-failure' 'RestartPreventExitStatus=1' 'StartLimitBurst=3'; do
    grep -Fxq "$directive" "$housekeeping_service" || { echo "jarvis-housekeeping.service lacks $directive" >&2; exit 1; }
done
# `is-enabled --quiet` also succeeds for static units; the updater must use
# unit_enabled, which accepts only enabled|enabled-runtime.
if grep -Fq 'is-enabled --quiet' "$repo_dir/deploy/systemd/update-core-release.sh"; then
    echo "update-core-release.sh must use unit_enabled instead of is-enabled --quiet" >&2
    exit 1
fi
# jarvis-backup.service is static by design: asking whether it is enabled,
# with any flags or through unit_enabled, is always a mistake.
if grep -Eq '(is-enabled( +--?[a-z-]+)*|unit_enabled) +"?jarvis-backup[.]service' \
    "$repo_dir/deploy/systemd/update-core-release.sh"; then
    echo "update-core-release.sh must never query whether jarvis-backup.service is enabled" >&2
    exit 1
fi

fixture_dir=$(mktemp -d)
trap 'rm -rf -- "$fixture_dir"' EXIT
stub="$fixture_dir/jarvis-stub"
printf '#!/usr/bin/env bash\nexit 0\n' > "$stub"
chmod 0755 "$stub"

# `systemd-analyze verify` validates that ExecStart exists.  Production units
# deliberately point at the atomically activated release, which is absent in a
# clean CI runner.  Verify copies with only those fixed binary paths replaced;
# this still catches unit syntax and hardening regressions without mutating
# /opt, /usr/local, or the runner's service state.
for unit in "$repo_dir"/deploy/systemd/*.service "$repo_dir"/deploy/systemd/*.timer "$repo_dir"/deploy/systemd/*.socket; do
    candidate="$fixture_dir/${unit##*/}"
    sed -E \
        -e "s#^(ExecStart|ExecStartPre)=/opt/jarvis/current/[^[:space:]]+#\\1=$stub#" \
        -e "s#^(ExecStart|ExecStartPre)=/usr/local/(sbin|libexec)/jarvis[^[:space:]]*#\\1=$stub#" \
        -e "s#^(ExecStart|ExecStartPre)=/usr/local/bin/codex#\\1=$stub#" \
        -e "s#^(ExecStart|ExecStartPre)=/opt/jarvis/laya/current/bin/python.*#\\1=$stub#" \
        "$unit" > "$candidate"
done
for candidate in "$fixture_dir"/*.service "$fixture_dir"/*.timer "$fixture_dir"/*.socket; do
    systemd-analyze verify "$candidate"
done
echo "Systemd runtime lifecycle checks passed"
