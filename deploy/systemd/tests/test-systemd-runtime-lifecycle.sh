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
grep -Fq 'ReadWritePaths=/etc/jarvis/model-policy.json /var/lib/jarvis/config-broker /run/jarvis-config-broker' "$broker"
if grep -Fq 'mkdir /run/jarvis-config-broker' "$prepare"; then
    echo "config broker runtime directory must be systemd-managed" >&2
    exit 1
fi

for unit in "$repo_dir"/deploy/systemd/*.service "$repo_dir"/deploy/systemd/*.timer; do
    systemd-analyze verify "$unit"
done
echo "Systemd runtime lifecycle checks passed"
