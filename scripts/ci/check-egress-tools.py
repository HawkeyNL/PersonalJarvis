#!/usr/bin/env python3
"""Keep web egress tools where they were reviewed.

Fails when tracked Rust source:
- spawns the claude or codex CLI (`Command::new(...)` naming it) outside the
  two subscription worker binaries;
- names "WebFetch", "Bash" or an `mcp__` tool outside the known deny-lists
  and their tests;
- uses `SandboxProfile::Browser` beyond its existing profile definitions.

The allowlists are explicit on purpose: a new use needs a review and an
update here.
"""
import argparse
from pathlib import Path
import re
import subprocess
import sys

WORKERS = {
    "jarvis-api/src/bin/jarvis-claude-worker.rs",
    "jarvis-api/src/bin/jarvis-codex-chat-worker.rs",
}
SPAWN = re.compile(r"Command::new\(\s*[^)]*(?:claude|codex)", re.IGNORECASE)
TOOLS = re.compile(r'"WebFetch"|"Bash"|mcp__')
# The Claude Code executor deny-list and its test; the Claude worker's
# `--disallowedTools mcp__*` and its argv and stream tests.
TOOL_FILES = {
    "crates/agent/src/lib.rs",
    "jarvis-api/src/bin/jarvis-claude-worker.rs",
}
BROWSER = re.compile(r"SandboxProfile::(?:Browser\b|\*|\{[^}]*\bBrowser\b)")
# The profile's own limits, network policy, config, name and test: no caller.
BROWSER_USES = {"crates/sandbox/src/lib.rs": 5}


def violations(name, text):
    lines = text.splitlines()
    for number, line in enumerate(lines, 1):
        if SPAWN.search(line) and name not in WORKERS:
            yield number, "claude/codex CLI spawned outside the worker binaries"
        if TOOLS.search(line) and name not in TOOL_FILES:
            yield number, "WebFetch, Bash or mcp__ tool outside a reviewed deny-list"
    browser = [number for number, line in enumerate(lines, 1) if BROWSER.search(line)]
    if len(browser) != BROWSER_USES.get(name, 0):
        yield (browser or [1])[0], f"SandboxProfile::Browser used {len(browser)} times, reviewed {BROWSER_USES.get(name, 0)}"


def audit(root):
    names = subprocess.check_output(["git", "-C", str(root), "ls-files", "-z", "--", "*.rs"]).split(b"\0")
    found = 0
    for name in sorted(filter(None, names)):
        name = name.decode("utf-8")
        for line, rule in violations(name, (root / name).read_text(encoding="utf-8")):
            print(f"{name}:{line}: {rule}")
            found += 1
    print(f"Egress tool scan: {len(names) - 1} Rust files, {found} violations")
    return found


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    sys.exit(bool(audit(args.root.resolve())))
