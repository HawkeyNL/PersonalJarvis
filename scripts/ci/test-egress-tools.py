import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("egress", Path(__file__).with_name("check-egress-tools.py"))
egress = importlib.util.module_from_spec(spec)
spec.loader.exec_module(egress)

WORKER = "jarvis-api/src/bin/jarvis-claude-worker.rs"
OTHER = "crates/llm/src/router.rs"


class EgressToolTests(unittest.TestCase):
    def rules(self, name, text):
        return [rule for _, rule in egress.violations(name, text)]

    def test_cli_spawn_only_in_the_workers(self):
        for text in ['Command::new("/usr/local/bin/claude")', "Command::new(CODEX)",
                     "std::process::Command::new( claude_bin )"]:
            self.assertIn("claude/codex CLI spawned outside the worker binaries",
                          self.rules(OTHER, text), text)
            self.assertEqual(self.rules(WORKER, text), [], text)
        self.assertEqual(self.rules(OTHER, 'Command::new("/usr/bin/git")'), [])

    def test_fetch_shell_and_mcp_tools_only_in_reviewed_files(self):
        for text in ['tools: ["WebFetch"]', 'allow("Bash")', '"mcp__github__create_issue"']:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
            self.assertEqual(self.rules("crates/agent/src/lib.rs", text), [], text)
        self.assertEqual(self.rules(OTHER, '"WebSearch" bash Bash'), [])
        for text in ['allow: "Bash(git:*)"', '"WebFetch(domain:example.org)"',
                     '"--tools", "WebSearch,WebFetch"', 'r#"{"name":"Bash"}"#']:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
            self.assertEqual(self.rules(WORKER, text), [], text)
        self.assertEqual(self.rules(OTHER, '"BashScript WebFetcher"'), [])

    def test_literals_spanning_lines_are_scanned(self):
        for text in ['let s = "allow\n Bash(git:*)";', 'r#"{\n  tools: Bash, WebFetch\n}"#',
                     'br##"x\n"WebFetch"\n"##', 'Command::new(\n    CLAUDE_BIN,\n)',
                     '"/opt/runtime\\\n/claude"']:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
        # A quote in a char literal or a comment does not start a literal.
        self.assertEqual(self.rules(OTHER, "let q = '\"'; // \"Bash\nlet b = Bash;"), [])
        self.assertEqual(self.rules(OTHER, 'let u = "https://example.org"; // Bash'), [])
        # An escaped quote does not end a literal.
        self.assertEqual(len(self.rules(OTHER, '"x \\" WebFetch"')), 1)
        self.assertEqual(self.rules(OTHER, 'r"WebSearch"; "a \\" b"; WebFetch'), [])
        self.assertEqual([line for line, _ in egress.violations(OTHER, 'a\nb\n"x\nBash"')], [3])

    def test_block_comments_c_strings_and_unparsed_quotes(self):
        for text in ['/* a " b */\nlet x = "Bash";', '/** "\n */\nlet y = "Bash";',
                     'let c = cr"C:\\"; let y = "Bash";', 'let s = c"x"; let y = "WebFetch";']:
            self.assertEqual(self.rules(OTHER, text),
                             ["WebFetch, Bash or mcp__ tool outside a reviewed deny-list"], text)
        # A quote the tokenizer cannot place fails closed, with its line.
        self.assertEqual(list(egress.violations(OTHER, 'let a = 1;\nlet b = x"y;')),
                         [(2, "unparsed quote: string literals could not be scanned")])
        self.assertEqual(self.rules(WORKER, 'let b = x"y;'),
                         ["unparsed quote: string literals could not be scanned"])
        # Nested block comments fail closed; unicode and hex char escapes parse.
        self.assertIn("nested block comment: could not be scanned",
                      self.rules(OTHER, '/* /* x */ *//* y */ let y = "Bash";'))
        for escape in ["'\\u{22}'", "'\\x22'"]:
            text = escape + '"\'""/*" let y = "Bash"; /* */'
            self.assertNotEqual(self.rules(OTHER, text), [], text)
            self.assertEqual(self.rules(OTHER, f'let q = {escape}; let s = "ok";'), [], escape)

    def test_cli_paths_only_in_reviewed_files(self):
        for text in ['const BIN: &str = "/usr/local/bin/claude";', '"/opt/x/codex"',
                     'format!("{home}/bin/claude")']:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
            self.assertEqual(self.rules(WORKER, text), [], text)
            self.assertEqual(self.rules("crates/admin/src/ai_accounts.rs", text), [], text)
        self.assertEqual(self.rules(OTHER, '"/usr/local/bin/claude-helper" "codex.md"'), [])

    def test_browser_profile_gains_no_caller(self):
        for text in ["profile: SandboxProfile::Browser,", "use SandboxProfile::*;",
                     "use jarvis_sandbox::SandboxProfile::{Coding, Browser};"]:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
        self.assertEqual(self.rules(OTHER, "SandboxProfile::Research"), [])
        definitions = "SandboxProfile::Browser =>\n" * 5
        self.assertEqual(self.rules("crates/sandbox/src/lib.rs", definitions), [])
        self.assertEqual(len(self.rules("crates/sandbox/src/lib.rs", definitions * 2)), 1)

    def test_current_tree_is_clean(self):
        self.assertEqual(egress.audit(Path(__file__).resolve().parents[2]), 0)

    def test_no_rust_files_fails(self):
        with tempfile.TemporaryDirectory() as empty:
            subprocess.run(["git", "init", "-q", empty], check=True)
            self.assertNotEqual(egress.audit(Path(empty)), 0)


if __name__ == "__main__":
    unittest.main()
