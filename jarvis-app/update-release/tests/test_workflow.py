from __future__ import annotations

from pathlib import Path
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
        ):
            self.assertIn(required, self.workflow)

    def test_manifest_publication_waits_for_every_mandatory_platform(self) -> None:
        self.assertIn("needs: [validate, desktop, android, ios]", self.workflow)
        publish = self.workflow.index("publish-private-release:")
        upload_manifest = self.workflow.index('gh release upload "$RELEASE_TAG"', publish)
        publish_draft = self.workflow.index("--draft=false --latest", upload_manifest)
        self.assertLess(upload_manifest, publish_draft)


if __name__ == "__main__":
    unittest.main()
