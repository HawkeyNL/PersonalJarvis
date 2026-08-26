#!/usr/bin/env bash
# Owner-controlled update of private agent profiles. This intentionally does
# not update /etc/jarvis/Jarvis.md; the persona has a stricter separate path.
set -euo pipefail

usage() { echo "Usage: sudo jarvis-private-update --source /path/to/PersonalJarvisAgents" >&2; exit 64; }
source_root=
while (($#)); do
    case "$1" in
        --source) source_root=${2:-}; shift 2 ;;
        *) usage ;;
    esac
done
[[ ${EUID} -eq 0 && -n $source_root ]] || usage
exec /usr/local/libexec/jarvis/install-agent-bundle --source "$source_root"
