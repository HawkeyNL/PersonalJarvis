#!/usr/bin/env bash
# Presentation regressions must remain independent of a live Home Node.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
ui="$repo_dir/deploy/lib/ui.sh"
setup="$repo_dir/deploy/systemd/setup-home-node.sh"
verify="$repo_dir/deploy/systemd/verify-home-node.sh"
install_core="$repo_dir/deploy/systemd/install-home-node-core.sh"

plain=$(bash -c 'source "$1"; ui_heading heading; ui_success success; ui_warning warning' _ "$ui")
[[ $plain != *$'\033'* ]]
[[ $plain == *heading* && $plain == *success* && $plain == *warning* ]]
no_color=$(NO_COLOR=1 bash -c 'source "$1"; ui_success no-color' _ "$ui")
[[ $no_color != *$'\033'* ]]

grep -Fq 'JARVIS_VERBOSE' "$ui"
grep -Fq 'FIRST OWNER BOOTSTRAP SECRET follows' "$setup"
grep -Fq 'Jarvis Home Node ready' "$setup"
grep -Fq 'Security verification:' "$verify"
grep -Fq '2>/dev/null' "$verify"
grep -Fq 'recent service diagnostics follow' "$install_core"
echo "Home Node output presentation checks passed"
