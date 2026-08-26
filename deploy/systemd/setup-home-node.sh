#!/usr/bin/env bash
# One root-operated, idempotent Home Node setup entry point. It deliberately
# composes the narrowly scoped helpers; it never fetches the private repository
# and never receives a GitHub token for it.
set -euo pipefail

release_tag=
private_agents=
bootstrap_cidr=
usage() {
    cat >&2 <<'EOF'
Usage: sudo ./deploy/systemd/setup-home-node.sh \
  --release vMAJOR.MINOR.PATCH --private-agents /path/to/PersonalJarvisAgents \
  --bootstrap-cidr 192.168.1.0/24
EOF
    exit 64
}
fail() { echo "ERROR $*" >&2; exit 1; }
status() { printf '%s %s\n' "$1" "$2"; }
while (($#)); do
    case "$1" in
        --release) release_tag=${2:-}; shift 2 ;;
        --private-agents) private_agents=${2:-}; shift 2 ;;
        --bootstrap-cidr) bootstrap_cidr=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ $release_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "--release must be vMAJOR.MINOR.PATCH"
[[ -n $private_agents && -n $bootstrap_cidr ]] || usage
for command in docker systemctl curl jq openssl; do command -v "$command" >/dev/null 2>&1 || fail "missing prerequisite: $command"; done
systemctl is-active --quiet docker.service || fail "Docker is not active"

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
status CREATE_OR_UNCHANGED "Home Node identities and directories"
bash "$repo_dir/deploy/systemd/prepare-home-node.sh"
status CREATE_OR_UNCHANGED "SurrealDB root configuration"
bash "$repo_dir/deploy/surrealdb/initialize-production-surrealdb.sh"
status UPDATE "private SurrealDB container"
bash /usr/local/libexec/jarvis/start-production-surrealdb

password_file=/run/jarvis-core-db-password
if [[ ! -e /etc/jarvis/core.env ]]; then
    [[ ! -e /etc/jarvis/surrealdb-core-provisioned ]] || \
        fail "scoped database account exists but core.env is absent; recover explicitly rather than rotating credentials"
    status CREATE "scoped SurrealDB Core account and Core environment"
    bash /usr/local/libexec/jarvis/provision-core-user --password-file "$password_file"
    bash /usr/local/libexec/jarvis/generate-core-env --bootstrap-cidr "$bootstrap_cidr" --surreal-password-file "$password_file"
else
    status UNCHANGED "existing Core environment and scoped database credentials"
fi

if [[ ! -e /etc/jarvis/updater.env ]]; then
    install -o root -g root -m 0600 /dev/null /etc/jarvis/updater.env
    printf '%s\n' 'JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis' > /etc/jarvis/updater.env
    status CREATE "public release updater configuration"
else
    status UNCHANGED "public release updater configuration"
fi

if [[ ! -d /opt/jarvis/releases/$release_tag ]]; then
    status CREATE "verified public release $release_tag"
    bash /usr/local/libexec/jarvis/stage-core-release "$release_tag"
else
    status UNCHANGED "verified public release $release_tag"
fi
status UPDATE "protected owner persona"
bash /usr/local/libexec/jarvis/install-private-config --source "$private_agents"
status UPDATE "private agent bundle"
JARVIS_AGENT_BUNDLE_VALIDATOR="/opt/jarvis/releases/$release_tag/jarvis-agent-bundle" \
    bash /usr/local/libexec/jarvis/install-agent-bundle --source "$private_agents"
status UPDATE "systemd services"
bash "$repo_dir/deploy/systemd/install-home-node-core.sh" "/opt/jarvis/releases/$release_tag"
systemctl enable --now jarvis-updater.timer
status VERIFY "Home Node"
bash /usr/local/libexec/jarvis/verify-home-node
status DONE "Home Node setup completed"
