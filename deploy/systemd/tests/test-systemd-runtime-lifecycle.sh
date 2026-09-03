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
grep -Fq 'ReadWritePaths=/etc/jarvis/model-policy.json' "$broker"
! grep -Eq '^ReadWritePaths=.*(/run/jarvis-config-broker|/var/lib/jarvis/config-broker)' "$broker"
if grep -Fq 'mkdir /run/jarvis-config-broker' "$prepare"; then
    echo "config broker runtime directory must be systemd-managed" >&2
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
for unit in "$repo_dir"/deploy/systemd/*.service "$repo_dir"/deploy/systemd/*.timer; do
    candidate="$fixture_dir/${unit##*/}"
    sed -E \
        -e "s#^(ExecStart|ExecStartPre)=/opt/jarvis/current/[^[:space:]]+#\\1=$stub#" \
        -e "s#^(ExecStart|ExecStartPre)=/usr/local/(sbin|libexec)/jarvis[^[:space:]]*#\\1=$stub#" \
        -e "s#^(ExecStart|ExecStartPre)=/usr/local/bin/codex#\\1=$stub#" \
        "$unit" > "$candidate"
    systemd-analyze verify "$candidate"
done
echo "Systemd runtime lifecycle checks passed"
