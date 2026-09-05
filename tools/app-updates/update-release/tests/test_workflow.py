from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[4]

class MobileReleaseWorkflowTests(unittest.TestCase):
    def test_core_does_not_own_an_editable_desktop_or_depend_on_its_assets(self):
        self.assertFalse((ROOT / "jarvis-app/package.json").exists())
        self.assertFalse((ROOT / "jarvis-app/src-tauri/Cargo.toml").exists())
        for path in ("Cargo.toml", "scripts/release/build-linux.sh", "jarvis-core-admin/src-tauri/tauri.conf.json"):
            self.assertNotIn("jarvis-app/", (ROOT / path).read_text())
        self.assertTrue((ROOT / "jarvis-android/app/build.gradle.kts").is_file())
        self.assertTrue((ROOT / "jarvis-ios/Jarvis.xcodeproj/project.pbxproj").is_file())

    def test_mobile_platforms_keep_signing_and_distribution(self):
        workflow = (ROOT / ".github/workflows/mobile-release.yml").read_text()
        for value in ("workflow_dispatch:", "github.ref == 'refs/heads/main'",
                      "apksigner verify", "ANDROID_RELEASE_KEYSTORE_BASE64", "assembleRelease bundleRelease",
                      "xcodebuild", "-exportArchive", "app-store-connect", "actions/upload-artifact@",
                      "ANDROID_SIGNING_CERTIFICATE_SHA256", 'test "$fingerprint" = "$EXPECTED_ANDROID_SIGNER"'):
            self.assertIn(value, workflow)
        self.assertNotIn("PRIVATE_RELEASE_REPO", workflow)
        self.assertNotIn("jarvis-app/", workflow)
        actions = re.findall(r"uses:\s+([^\s#]+)", workflow)
        self.assertTrue(actions)
        for action in actions:
            self.assertRegex(action, r"@[0-9a-f]{40}$")

    def test_mobile_secrets_are_scoped_to_steps(self):
        workflow = (ROOT / ".github/workflows/mobile-release.yml").read_text()
        for job in ("validate", "android", "ios"):
            start = workflow.index(f"  {job}:")
            steps = workflow.index("    steps:", start)
            self.assertNotIn("secrets.", workflow[start:steps])
