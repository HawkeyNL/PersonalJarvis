#!/usr/bin/env bash
# Linux CI regression: pretty setup must not capture helpers that intentionally
# require a terminal for secret/bootstrap handling.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
ui="$repo_dir/deploy/lib/ui.sh"
setup="$repo_dir/deploy/systemd/setup-home-node.sh"

command -v script >/dev/null 2>&1 || { echo "script is required" >&2; exit 1; }
bash -n "$ui" "$setup"
grep -Fq 'ui_run_tty "Root configuration checked"' "$setup"
grep -Fq 'requires an interactive terminal' "$ui"

# `script` allocates a pseudo-terminal. Both descriptors must remain TTYs all
# the way through ui_run_tty; ui_run intentionally would not satisfy this.
probe=$(mktemp)
trap 'rm -f -- "$probe"' EXIT
printf 'source %q\nui_run_tty tty-probe bash -c %q\n' "$ui" 'test -t 0 && test -t 1' > "$probe"
if script --version >/dev/null 2>&1; then
    script -qefc "bash $probe" /dev/null >/dev/null
else
    script -q /dev/null bash "$probe" >/dev/null
fi
if bash -c "source \"$ui\"; ui_run captured-probe bash -c 'test -t 0 && test -t 1'" >/dev/null 2>&1; then
    echo "capturing runner unexpectedly preserved a TTY" >&2
    exit 1
fi

echo "TTY-preserving setup runner checks passed"
