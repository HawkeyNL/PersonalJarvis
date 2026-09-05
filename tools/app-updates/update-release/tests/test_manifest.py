from __future__ import annotations

import copy
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from manifest import ManifestError, validate_manifest


def manifest(version: str = "1.2.3") -> dict:
    artifacts = []
    for platform, architecture in (("windows", "x86_64"), ("macos", "arm64"), ("linux", "x86_64")):
        artifacts.append({
            "platform": platform,
            "architecture": architecture,
            "distribution": "home-node-updater",
            "artifact": {
                "path": f"releases/v{version}/{platform}-{architecture}/Jarvis_{version}_{platform}_{architecture}.bundle",
                "sha256": "a" * 64,
                "size": 123,
            },
            "signature": {"scheme": "tauri-minisign", "value": "trusted signature"},
        })
    artifacts.append({
        "platform": "android",
        "architecture": "universal",
        "distribution": "home-node-apk",
        "artifact": {
            "path": f"releases/v{version}/android-universal/Jarvis_{version}_android_universal.apk",
            "sha256": "b" * 64,
            "size": 456,
        },
        "signature": {"scheme": "android-apk-signing-certificate-sha256", "value": "c" * 64},
        "metadata": {"version_code": 12},
    })
    artifacts.append({
        "platform": "ios",
        "architecture": "arm64",
        "distribution": "testflight",
        "external": {"bundle_id": "com.hawkeynl.jarvis", "build_number": "17"},
        "signature": {"scheme": "apple-code-signing", "value": "app-store-connect"},
    })
    return {
        "schema_version": 1,
        "release": {
            "version": version,
            "channel": "stable",
            "released_at": "2026-08-30T12:00:00Z",
            "minimum_client_protocol": 1,
            "notes": "Private stable release",
        },
        "artifacts": artifacts,
    }


class ManifestTests(unittest.TestCase):
    def test_complete_manifest_is_accepted(self) -> None:
        self.assertEqual(validate_manifest(manifest())["release"]["version"], "1.2.3")

    def test_path_traversal_is_rejected(self) -> None:
        value = manifest()
        value["artifacts"][0]["artifact"]["path"] = "releases/v1.2.3/windows-x86_64/../evil"
        with self.assertRaises(ManifestError):
            validate_manifest(value)

    def test_wrong_distribution_is_rejected(self) -> None:
        value = manifest()
        value["artifacts"][4]["distribution"] = "home-node-updater"
        with self.assertRaises(ManifestError):
            validate_manifest(value)

    def test_stable_channel_rejects_prerelease_versions(self) -> None:
        with self.assertRaises(ManifestError):
            validate_manifest(manifest("1.2.4-beta.1"))

    def test_unified_client_manifest_requires_every_client_and_installer(self) -> None:
        value = manifest()
        value["release"].update(
            product="clients",
            tag="app-v1.2.3",
            source_revision="a" * 40,
            client_protocol=1,
        )
        value["installers"] = [
            {
                "platform": "macos",
                "architecture": "arm64",
                "distribution": "home-node-installer",
                "artifact": {
                    "path": "releases/v1.2.3/macos-arm64/Jarvis_1.2.3_macos_arm64.dmg",
                    "sha256": "d" * 64,
                    "size": 10,
                },
            },
            {
                "platform": "android",
                "architecture": "universal",
                "distribution": "app-store-bundle",
                "artifact": {
                    "path": "releases/v1.2.3/android-universal/Jarvis_1.2.3_android_universal.aab",
                    "sha256": "e" * 64,
                    "size": 10,
                },
            },
        ]
        validate_manifest(value)
        for section in ("artifacts", "installers"):
            incomplete = copy.deepcopy(value)
            incomplete[section].pop()
            with self.subTest(section=section), self.assertRaises(ManifestError):
                validate_manifest(incomplete)


if __name__ == "__main__":
    unittest.main()
