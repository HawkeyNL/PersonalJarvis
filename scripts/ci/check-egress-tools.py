#!/usr/bin/env python3
"""Keep web egress tools where they were reviewed.

Fails when tracked Rust source:
- spawns the claude or codex CLI (`Command::new(...)` naming it) outside the
  two subscription worker binaries;
- has a string literal ending in `/claude` or `/codex` (a CLI path) outside
  the worker binaries and the reviewed account-linking and runtime installer;
- names WebFetch or Bash inside any string literal ("Bash(git:*)",
  "WebSearch,WebFetch"), or an `mcp__` tool, outside the known deny-lists
  and their tests;
- uses `SandboxProfile::Browser` beyond its existing profile definitions.

The allowlists are explicit on purpose: a new use needs a review and an
update here. String literals are found across lines (plain, byte and raw
strings; comments and char literals are skipped). Names built at run time
(`concat!`, a bare "claude" found through PATH, an enum variant) are not
caught: this guard is a tripwire, not a proof.
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
CLI_PATH = re.compile(r"/(?:claude|codex)\Z")
# Account linking runs the CLI through systemd-run as its own identity; the
# runtime installers download, verify and install it (and test that).
CLI_PATH_FILES = WORKERS | {
    "crates/admin/src/ai_accounts.rs",
    "crates/admin/src/ai_accounts/runtime.rs",
    "crates/admin/src/ai_accounts/runtime/codex.rs",
}
TOOLS = re.compile(r"\b(?:WebFetch|Bash)\b")
MCP = re.compile(r"mcp__")
# A line or block comment, a char literal, or a (byte, C, raw) string
# literal. A quote left over after removing these means the source was not
# understood, and fails the scan rather than hiding later literals.
TOKENS = re.compile(
    r"//[^\n]*"
    r"|/\*.*?\*/"
    r"|'(?:[^'\\\n]|\\u\{[0-9A-Fa-f_]+\}|\\x[0-9A-Fa-f]{2}|\\.)'"
    r'|(?<![A-Za-z0-9_])[bc]?r(#*)"(.*?)"\1'
    r'|(?<![A-Za-z0-9_])[bc]?"((?:[^"\\]|\\.)*)"',
    re.DOTALL,
)
# The Claude Code executor deny-list and its test; the Claude worker's
# `--disallowedTools mcp__*` and its argv and stream tests.
TOOL_FILES = {
    "crates/agent/src/lib.rs",
    "jarvis-api/src/bin/jarvis-claude-worker.rs",
}
BROWSER = re.compile(r"SandboxProfile::(?:Browser\b|\*|\{[^}]*\bBrowser\b)")
# The profile's own limits, network policy, config, name and test: no caller.
BROWSER_USES = {"crates/sandbox/src/lib.rs": 5}


def literals(text):
    """(line, content) of every string literal in Rust source."""
    for match in TOKENS.finditer(text):
        content = match.group(2) if match.group(2) is not None else match.group(3)
        if content is not None:
            yield text.count("\n", 0, match.start()) + 1, content


def violations(name, text):
    line_of = lambda offset: text.count("\n", 0, offset) + 1
    found = set()
    leftover = TOKENS.sub(lambda match: "\n" * match.group(0).count("\n"), text)
    if '"' in leftover:
        found.add((leftover.count("\n", 0, leftover.index('"')) + 1,
                   "unparsed quote: string literals could not be scanned"))
    # Rust block comments nest; this tokenizer does not, so fail closed.
    for match in TOKENS.finditer(text):
        if match.group(0).startswith("/*") and "/*" in match.group(0)[2:]:
            found.add((line_of(match.start()), "nested block comment: could not be scanned"))
    if name not in WORKERS:
        found |= {(line_of(match.start()), "claude/codex CLI spawned outside the worker binaries")
                  for match in SPAWN.finditer(text)}
    for number, content in literals(text):
        if CLI_PATH.search(content) and name not in CLI_PATH_FILES:
            found.add((number, "claude/codex CLI path outside the reviewed files"))
        if TOOLS.search(content) and name not in TOOL_FILES:
            found.add((number, "WebFetch, Bash or mcp__ tool outside a reviewed deny-list"))
    if name not in TOOL_FILES:
        found |= {(line_of(match.start()), "WebFetch, Bash or mcp__ tool outside a reviewed deny-list")
                  for match in MCP.finditer(text)}
    yield from sorted(found)
    lines = text.splitlines()
    browser = [number for number, line in enumerate(lines, 1) if BROWSER.search(line)]
    if len(browser) != BROWSER_USES.get(name, 0):
        yield (browser or [1])[0], f"SandboxProfile::Browser used {len(browser)} times, reviewed {BROWSER_USES.get(name, 0)}"


def audit(root):
    names = sorted(filter(None, subprocess.check_output(
        ["git", "-C", str(root), "ls-files", "-z", "--", "*.rs"]).split(b"\0")))
    if not names:
        print("Egress tool scan: no Rust files found")
        return 1
    found = 0
    for name in names:
        name = name.decode("utf-8")
        for line, rule in violations(name, (root / name).read_text(encoding="utf-8")):
            print(f"{name}:{line}: {rule}")
            found += 1
    print(f"Egress tool scan: {len(names)} Rust files, {found} violations")
    return found


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    sys.exit(bool(audit(args.root.resolve())))
