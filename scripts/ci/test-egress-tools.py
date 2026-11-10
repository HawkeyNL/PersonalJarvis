import importlib.util
from pathlib import Path
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
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
            self.assertEqual(self.rules(WORKER, text), [], text)
        self.assertEqual(self.rules(OTHER, 'Command::new("/usr/bin/git")'), [])

    def test_fetch_shell_and_mcp_tools_only_in_reviewed_files(self):
        for text in ['tools: ["WebFetch"]', 'allow("Bash")', '"mcp__github__create_issue"']:
            self.assertEqual(len(self.rules(OTHER, text)), 1, text)
            self.assertEqual(self.rules("crates/agent/src/lib.rs", text), [], text)
        self.assertEqual(self.rules(OTHER, '"WebSearch" bash Bash'), [])

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


if __name__ == "__main__":
    unittest.main()
