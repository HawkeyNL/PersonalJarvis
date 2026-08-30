from __future__ import annotations

from pathlib import Path
import re
import unittest


REPOSITORY = Path(__file__).resolve().parents[3]
WORKFLOW = REPOSITORY / ".github/workflows/private-app-release.yml"


class PrivateReleaseWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_release_is_manual_main_only_and_uses_a_protected_environment(self) -> None:
        self.assertIn("workflow_dispatch:", self.workflow)
        self.assertNotIn("pull_request:", self.workflow)
        self.assertIn("github.ref == 'refs/heads/main'", self.workflow)
        self.assertIn("environment: private-app-release", self.workflow)

    def test_github_never_receives_home_node_deployment_coordinates(self) -> None:
        for forbidden in (
            "HOME_NODE_HOST",
            "HOME_NODE_IP",
            "HOME_NODE_DNS",
            "HOME_NODE_SSH",
            "HOME_NODE_USER",
        ):
            self.assertNotIn(forbidden, self.workflow)

    def test_macos_release_requires_both_os_and_updater_trust_layers(self) -> None:
        for required in (
            "MACOS_DEVELOPER_ID_CERTIFICATE_P12_BASE64",
            "APPLE_SIGNING_IDENTITY",
            "APPLE_API_KEY_PATH",
            "codesign --verify --deep --strict",
            "flags=.*runtime",
            'xcrun notarytool submit "$dmg"',
            'xcrun stapler staple "$dmg"',
            'xcrun stapler validate "$app"',
            'xcrun stapler validate "$dmg"',
            "TAURI_SIGNING_PRIVATE_KEY",
            "verify_update_signature",
        ):
            self.assertIn(required, self.workflow)

    def test_manifest_publication_waits_for_every_mandatory_platform(self) -> None:
        self.assertIn("needs: [validate, desktop, android, ios]", self.workflow)
        publish = self.workflow.index("publish-private-release:")
        upload_manifest = self.workflow.index('gh release upload "$RELEASE_TAG"', publish)
        publish_draft = self.workflow.index("--draft=false --latest", upload_manifest)
        self.assertLess(upload_manifest, publish_draft)

    def test_release_actions_are_immutably_pinned(self) -> None:
        actions = re.findall(r"uses:\s+([^\s#]+)", self.workflow)
        self.assertTrue(actions)
        for action in actions:
            with self.subTest(action=action):
                self.assertRegex(action, r"@[0-9a-f]{40}$")

    def test_job_environment_never_exposes_release_secrets(self) -> None:
        for job in ("stage-private-release", "desktop", "android", "ios", "publish-private-release"):
            start = self.workflow.index(f"  {job}:")
            steps = self.workflow.index("    steps:", start)
            with self.subTest(job=job):
                self.assertNotIn("secrets.", self.workflow[start:steps])

    def test_android_sdk_and_signer_are_pinned(self) -> None:
        self.assertIn('sdkmanager "platforms;android-37"', self.workflow)
        self.assertNotIn("platforms;android-37.0", self.workflow)
        self.assertIn("ANDROID_SIGNING_CERTIFICATE_SHA256", self.workflow)
        self.assertIn('test "$fingerprint" = "$EXPECTED_ANDROID_SIGNER"', self.workflow)

    def test_final_draft_is_revalidated_before_publish(self) -> None:
        publish = self.workflow.index("publish-private-release:")
        section = self.workflow[publish:]
        self.assertGreaterEqual(section.count("private draft source revision changed"), 2)
        self.assertIn("validate_staged_assets.py", section)
        self.assertIn("final-latest.json", section)
        self.assertLess(section.index("final-latest.json"), section.index("--draft=false --latest"))


if __name__ == "__main__":
    unittest.main()
