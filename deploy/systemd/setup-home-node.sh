#!/usr/bin/env bash
# One root-operated, idempotent Home Node setup entry point. It deliberately
# composes the narrowly scoped helpers; it never fetches the private repository
# and never receives a GitHub token for it.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck disable=SC1091 # dynamic repository root
source "$repo_dir/deploy/lib/ui.sh"

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
fail() { ui_error "$*"; exit 1; }
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

ui_heading "Jarvis Home Node Setup — $release_tag"
ui_step "[1/7] Preparing host"
ui_run "Service identity and directories ready" bash "$repo_dir/deploy/systemd/prepare-home-node.sh"
ui_step "[2/7] SurrealDB"
ui_run "Root configuration checked" bash "$repo_dir/deploy/surrealdb/initialize-production-surrealdb.sh"
ui_run "SurrealDB container healthy" bash /usr/local/libexec/jarvis/start-production-surrealdb

password_file=/run/jarvis-core-db-password
if [[ ! -e /etc/jarvis/core.env ]]; then
    [[ ! -e /etc/jarvis/surrealdb-core-provisioned ]] || \
        fail "scoped database account exists but core.env is absent; recover explicitly rather than rotating credentials"
    ui_run "Scoped Core database account created" bash /usr/local/libexec/jarvis/provision-core-user --password-file "$password_file"
    ui_warning "FIRST OWNER BOOTSTRAP SECRET follows; store it now. It is not repeated in the summary."
    bash /usr/local/libexec/jarvis/generate-core-env --bootstrap-cidr "$bootstrap_cidr" --surreal-password-file "$password_file"
else
    ui_warning "Core environment and scoped database credentials unchanged"
fi

if [[ ! -e /etc/jarvis/updater.env ]]; then
    install -o root -g root -m 0600 /dev/null /etc/jarvis/updater.env
    printf '%s\n' 'JARVIS_UPDATE_REPOSITORY=HawkeyNL/PersonalJarvis' > /etc/jarvis/updater.env
    ui_success "Public release updater configuration created"
else
    ui_warning "Public release updater configuration unchanged"
fi

ui_step "[3/7] Public release"
if [[ ! -d /opt/jarvis/releases/$release_tag ]]; then
    ui_run "Release $release_tag checksum verified and staged" bash /usr/local/libexec/jarvis/stage-core-release "$release_tag"
else
    ui_warning "Verified release $release_tag already staged"
fi
ui_step "[4/7] Protected configuration"
ui_run "Protected persona installed" bash /usr/local/libexec/jarvis/install-private-config --source "$private_agents"
ui_run "Private agent bundle activated" env JARVIS_AGENT_BUNDLE_VALIDATOR="/opt/jarvis/releases/$release_tag/jarvis-agent-bundle" bash /usr/local/libexec/jarvis/install-agent-bundle --source "$private_agents"
ui_step "[5/7] Services and health"
ui_run "Jarvis Core installed and ready" bash "$repo_dir/deploy/systemd/install-home-node-core.sh" "/opt/jarvis/releases/$release_tag"
systemctl enable --now jarvis-updater.timer
ui_success "Automatic updater timer enabled"
ui_step "[6/7] Security verification"
ui_run "Home Node security verification passed" bash /usr/local/libexec/jarvis/verify-home-node
agents=$(jq -r '.agents | length' "$(readlink -f /var/lib/jarvis/agents/current)/manifest.json")
ui_step "[7/7] Complete"
ui_success "Jarvis Home Node ready"
printf 'Release: %s\nCore: active\nSurrealDB: healthy\nAgents: %s\nAPI: http://127.0.0.1:8080\nPublic ingress: not configured\nUpdater timer: enabled\n' "$release_tag" "$agents"
