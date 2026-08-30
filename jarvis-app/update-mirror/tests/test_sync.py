from __future__ import annotations

from io import BytesIO
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

MIRROR_ROOT = Path(__file__).resolve().parents[1]
RELEASE_ROOT = MIRROR_ROOT.parent / "update-release"
sys.path.insert(0, str(MIRROR_ROOT))
sys.path.insert(0, str(RELEASE_ROOT / "tests"))

from sync import SyncError, sync_release
from test_manifest import manifest


class FakeSource:
    def __init__(self, values: dict[str, bytes]) -> None:
        self.values = values

    def open(self, url: str) -> BytesIO:
        if url not in self.values:
            raise OSError(f"missing fixture: {url}")
        return BytesIO(self.values[url])


def release_source(version: str, corrupt: bool = False) -> tuple[dict, FakeSource]:
    value = manifest(version)
    values: dict[str, bytes] = {}
    for entry in value["artifacts"]:
        artifact = entry.get("artifact")
        if artifact is None:
            continue
        content = f"{version}:{entry['platform']}:{entry['architecture']}".encode()
        artifact["sha256"] = hashlib.sha256(content).hexdigest()
        artifact["size"] = len(content)
        values[f"https://storage.invalid/{artifact['path']}"] = content
    if corrupt:
        first = value["artifacts"][0]["artifact"]
        values[f"https://storage.invalid/{first['path']}"] = b"corrupt"
    values["https://storage.invalid/channels/stable/latest.json"] = json.dumps(value).encode()
    return value, FakeSource(values)


def config(root: Path) -> dict:
    return {
        "schema_version": 1,
        "source": {
            "manifest_url": "https://storage.invalid/channels/stable/latest.json",
            "artifact_base_url": "https://storage.invalid/",
            "bearer_token_file": "/unused-in-unit-test",
        },
        "mirror_root": str(root),
        "channel": "stable",
        "retention_previous": 1,
        "timeout_seconds": 30,
        "max_artifact_bytes": 1024 * 1024,
    }


class SyncTests(unittest.TestCase):
    def test_sync_activates_only_after_all_artifacts_verify(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, source = release_source("1.0.0")
            self.assertEqual(sync_release(config(root), source), "1.0.0")
            self.assertEqual((root / "current").resolve(), root / "releases" / "v1.0.0")
            self.assertTrue((root / "current" / "verified.json").is_file())
            self.assertFalse(any((root / "current").rglob("*.ipa")))

    def test_failed_sync_preserves_previous_current_release(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, first = release_source("1.0.0")
            sync_release(config(root), first)
            _, corrupt = release_source("1.1.0", corrupt=True)
            with self.assertRaises(SyncError):
                sync_release(config(root), corrupt)
            self.assertEqual((root / "current").resolve(), root / "releases" / "v1.0.0")
            self.assertFalse((root / "releases" / "v1.1.0").exists())

    def test_retention_keeps_current_and_one_previous(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for version in ("1.0.0", "1.1.0", "1.2.0"):
                _, source = release_source(version)
                sync_release(config(root), source)
            releases = sorted(path.name for path in (root / "releases").iterdir())
            self.assertEqual(releases, ["v1.1.0", "v1.2.0"])


if __name__ == "__main__":
    unittest.main()
