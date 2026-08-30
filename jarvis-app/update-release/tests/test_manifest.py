from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from build_manifest import build
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

    def test_builder_hashes_local_artifacts_and_inlines_signature(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            artifact = base / "Jarvis_1.2.3_windows_x86_64.bundle"
            artifact.write_bytes(b"signed bundle")
            signature = base / "bundle.sig"
            signature.write_text("minisign payload\n", encoding="utf-8")
            descriptor = manifest()
            item = descriptor["artifacts"][0]
            item.pop("artifact")
            item["source"] = artifact.name
            item["published_path"] = "releases/v1.2.3/windows-x86_64/Jarvis_1.2.3_windows_x86_64.bundle"
            item["signature"] = {"scheme": "tauri-minisign", "source": signature.name}
            for other in descriptor["artifacts"][1:4]:
                local = base / Path(other["artifact"]["path"]).name
                local.write_bytes(b"other")
                other.pop("artifact")
                other["source"] = local.name
                other["published_path"] = f"releases/v1.2.3/{other['platform']}-{other['architecture']}/{local.name}"
                other["signature"] = {"scheme": other["signature"]["scheme"], "value": other["signature"]["value"]}
            descriptor.pop("schema_version")
            result = build(descriptor, base)
            built = result["artifacts"][0]
            self.assertEqual(built["artifact"]["sha256"], hashlib.sha256(b"signed bundle").hexdigest())
            self.assertEqual(built["artifact"]["size"], len(b"signed bundle"))
            self.assertEqual(built["signature"]["value"], "minisign payload")
            self.assertNotIn("source", json.dumps(result))


if __name__ == "__main__":
    unittest.main()

