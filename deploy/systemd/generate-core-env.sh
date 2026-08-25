#!/usr/bin/env bash
# Generate the initial Core environment file. It is deliberately interactive
# for the one-time bootstrap secret and consumes the scoped DB password from a
# root-only file in /run so neither persistent secret enters shell history.
set -euo pipefail

readonly env_file=/etc/jarvis/core.env
bootstrap_cidr=
password_file=
namespace=jarvis
database=core
username=core

usage() {
    cat >&2 <<'EOF'
Usage: sudo generate-core-env.sh --bootstrap-cidr 192.168.1.0/24 --surreal-password-file /run/jarvis-core-db-password [--namespace jarvis] [--database core] [--username core]
EOF
    exit 64
}
fail() { echo "Core configuration: $*" >&2; exit 1; }

while (($#)); do
    case "$1" in
        --bootstrap-cidr) bootstrap_cidr=${2:-}; shift 2 ;;
        --surreal-password-file) password_file=${2:-}; shift 2 ;;
        --namespace) namespace=${2:-}; shift 2 ;;
        --database) database=${2:-}; shift 2 ;;
        --username) username=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done

[[ ${EUID} -eq 0 ]] || fail "must run as root"
[[ -t 0 && -t 1 ]] || fail "requires an interactive terminal so the bootstrap secret is not logged"
[[ -n $bootstrap_cidr && -n $password_file ]] || usage
[[ $password_file == /run/* && -f $password_file && ! -L $password_file ]] || fail "password file must be a regular file below /run"
[[ $(stat -c '%U:%G:%a' "$password_file") == root:root:600 ]] || fail "password file must be root:root mode 0600"
[[ ! -e $env_file ]] || fail "$env_file already exists; refusing to rotate credentials or bootstrap state"
command -v openssl >/dev/null 2>&1 || fail "openssl is required"
command -v python3 >/dev/null 2>&1 || fail "python3 is required for CIDR validation"

python3 - "$bootstrap_cidr" <<'PY'
import ipaddress
import sys
network = ipaddress.ip_network(sys.argv[1], strict=False)
if not network.is_private or network.is_loopback or network.is_link_local:
    raise SystemExit("bootstrap CIDR must be an explicit private LAN range")
PY

db_password=$(<"$password_file")
[[ ${#db_password} -ge 32 && $db_password != *$'\n'* ]] || fail "scoped database password is malformed"
bootstrap_secret=$(openssl rand -hex 32)
bootstrap_hash=$(printf %s "$bootstrap_secret" | sha256sum | awk '{print $1}')
tmp=$(mktemp /etc/jarvis/.core.env.XXXXXX)
trap 'rm -f -- "$tmp"' EXIT
umask 077
cat > "$tmp" <<EOF
JARVIS_ENVIRONMENT=production
JARVIS_LOG_JSON=true
JARVIS_BIND_ADDR=127.0.0.1:8080
JARVIS_SURREAL_ENDPOINT=127.0.0.1:8000
JARVIS_SURREAL_NAMESPACE=$namespace
JARVIS_SURREAL_DATABASE=$database
JARVIS_SURREAL_USERNAME=$username
JARVIS_SURREAL_PASSWORD=$db_password
JARVIS_AGENT_ENABLED=false
JARVIS_AGENT_CLAUDE_CODE_ENABLED=false
JARVIS_AGENT_WORKSPACE_ROOT=
JARVIS_TRUSTED_PROXY_HOPS=0
JARVIS_TRUSTED_PROXY_IPS=
JARVIS_BOOTSTRAP_SECRET_SHA256=$bootstrap_hash
JARVIS_BOOTSTRAP_ALLOWED_CIDRS=$bootstrap_cidr
JARVIS_AUTHENTICATED_RATE_PER_MIN=300
JARVIS_LLM_RATE_PER_MIN=20
EOF
chown root:jarvis "$tmp"
chmod 0640 "$tmp"
mv -f -- "$tmp" "$env_file"
rm -f -- "$password_file"
trap - EXIT

# The raw secret is deliberately written once to the operator's terminal only.
printf '\nFIRST-OWNER BOOTSTRAP SECRET (show/store once; never paste into logs):\n%s\n\n' "$bootstrap_secret" >/dev/tty
echo "Core configuration: created $env_file with a verifier only; local-first mode has no public hostname or trusted proxy."
