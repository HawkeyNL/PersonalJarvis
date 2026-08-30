from __future__ import annotations

from io import BytesIO
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from urllib.request import Request

MIRROR_ROOT = Path(__file__).resolve().parents[1]
RELEASE_ROOT = MIRROR_ROOT.parent / "update-release"
sys.path.insert(0, str(MIRROR_ROOT))
sys.path.insert(0, str(RELEASE_ROOT / "tests"))

from sync import GitHubReleaseSource, SyncError, _SafeRedirectHandler, _verify_android_apk, sync_release
from test_manifest import manifest


class FakeSource:
    def __init__(self, values: dict[str, bytes], version: str) -> None:
        self.values = values
        self.version = version

    def _open(self, url: str) -> BytesIO:
        if url not in self.values:
            raise OSError(f"missing fixture: {url}")
        return BytesIO(self.values[url])

    def open_manifest(self) -> BytesIO:
        return self._open("https://storage.invalid/channels/stable/latest.json")

    def open_artifact(self, version: str, path: str) -> BytesIO:
        if version != self.version:
            raise OSError("unexpected release version")
        return self._open(f"https://storage.invalid/{path}")


class FakeGitHubSource(GitHubReleaseSource):
    def __init__(self) -> None:
        super().__init__("owner/releases", "secret", 5)
        self.requests: list[tuple[str, str]] = []

    def _open(self, url: str, accept: str) -> BytesIO:
        self.requests.append((url, accept))
        api = "https://api.github.com/repos/owner/releases"
        if url == f"{api}/releases/latest":
            return BytesIO(json.dumps({
                "tag_name": "app-v1.2.3",
                "draft": False,
                "prerelease": False,
                "assets": [{"name": "latest.json", "url": f"{api}/releases/assets/1"}],
            }).encode())
        if url == f"{api}/releases/tags/app-v1.2.3":
            return BytesIO(json.dumps({
                "tag_name": "app-v1.2.3",
                "draft": False,
                "prerelease": False,
                "assets": [{"name": "Jarvis.apk", "url": f"{api}/releases/assets/2"}],
            }).encode())
        if url == f"{api}/releases/assets/1":
            return BytesIO(b"manifest")
        if url == f"{api}/releases/assets/2":
            return BytesIO(b"apk")
        raise OSError(f"unexpected GitHub fixture URL: {url}")


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
    return value, FakeSource(values, version)


def config(root: Path) -> dict:
    return {
        "schema_version": 1,
        "source": {
            "kind": "http-template",
            "manifest_url": "https://storage.invalid/channels/stable/latest.json",
            "artifact_url_template": "https://storage.invalid/{{path}}",
            "bearer_token_file": "/unused-in-unit-test",
        },
        "mirror_root": str(root),
        "channel": "stable",
        "retention_previous": 1,
        "timeout_seconds": 30,
        "max_artifact_bytes": 1024 * 1024,
        "android_signing_certificate_sha256": "c" * 64,
        "android_apksigner_path": "/usr/bin/apksigner",
    }


def accept_apk(path: Path, fingerprint: str, tool: Path, timeout: int) -> None:
    assert path.suffix == ".apk"
    assert fingerprint == "c" * 64
    assert tool == Path("/usr/bin/apksigner")
    assert timeout == 30


class SafeRedirectTests(unittest.TestCase):
    def setUp(self) -> None:
        self.handler = _SafeRedirectHandler()

    def redirect(self, request: Request, url: str) -> Request:
        redirected = self.handler.redirect_request(request, None, 302, "Found", {}, url)
        self.assertIsNotNone(redirected)
        return redirected

    def request(self, url: str) -> Request:
        return Request(url, headers={"Authorization": "Bearer secret"})

    def test_same_https_origin_preserves_authorization(self) -> None:
        request = self.request("https://UPDATES.example:443/releases/latest")
        redirected = self.redirect(request, "https://updates.example/artifacts/app")
        self.assertEqual(redirected.get_header("Authorization"), "Bearer secret")

    def test_different_host_strips_authorization(self) -> None:
        request = self.request("https://updates.example/releases/latest")
        redirected = self.redirect(request, "https://cdn.example/artifacts/app")
        self.assertIsNone(redirected.get_header("Authorization"))

    def test_different_port_strips_authorization(self) -> None:
        request = self.request("https://updates.example/releases/latest")
        redirected = self.redirect(request, "https://updates.example:8443/artifacts/app")
        self.assertIsNone(redirected.get_header("Authorization"))

    def test_https_to_http_redirect_is_rejected(self) -> None:
        request = self.request("https://updates.example/releases/latest")
        with self.assertRaisesRegex(SyncError, "non-HTTPS redirect"):
            self.redirect(request, "http://updates.example/artifacts/app")

    def test_redirect_chain_never_restores_stripped_authorization(self) -> None:
        request = self.request("https://updates.example/releases/latest")
        same_origin = self.redirect(request, "https://updates.example/releases/next")
        self.assertEqual(same_origin.get_header("Authorization"), "Bearer secret")

        different_origin = self.redirect(same_origin, "https://cdn.example/artifacts/app")
        self.assertIsNone(different_origin.get_header("Authorization"))

        back_to_source = self.redirect(different_origin, "https://updates.example/artifacts/app")
        self.assertIsNone(back_to_source.get_header("Authorization"))


class SyncTests(unittest.TestCase):
    def test_private_github_adapter_resolves_assets_through_the_api(self) -> None:
        source = FakeGitHubSource()
        with source.open_manifest() as response:
            self.assertEqual(response.read(), b"manifest")
        with source.open_artifact("1.2.3", "releases/v1.2.3/android-universal/Jarvis.apk") as response:
            self.assertEqual(response.read(), b"apk")
        self.assertEqual(source.requests[-1][1], "application/octet-stream")

    def test_sync_activates_only_after_all_artifacts_verify(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, source = release_source("1.0.0")
            self.assertEqual(sync_release(config(root), source, accept_apk), "1.0.0")
            self.assertEqual((root / "current").resolve(), root / "releases" / "v1.0.0")
            self.assertTrue((root / "current" / "verified.json").is_file())
            self.assertEqual(
                (root / "manifests" / "latest.json").resolve(),
                (root / "current" / "manifest.json").resolve(),
            )
            self.assertFalse(any((root / "current").rglob("*.ipa")))

    def test_failed_sync_preserves_previous_current_release(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, first = release_source("1.0.0")
            sync_release(config(root), first, accept_apk)
            _, corrupt = release_source("1.1.0", corrupt=True)
            with self.assertRaises(SyncError):
                sync_release(config(root), corrupt, accept_apk)
            self.assertEqual((root / "current").resolve(), root / "releases" / "v1.0.0")
            self.assertFalse((root / "releases" / "v1.1.0").exists())

    def test_retention_keeps_current_and_one_previous(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for version in ("1.0.0", "1.1.0", "1.2.0"):
                _, source = release_source(version)
                sync_release(config(root), source, accept_apk)
            releases = sorted(path.name for path in (root / "releases").iterdir())
            self.assertEqual(releases, ["v1.1.0", "v1.2.0"])

    def test_android_signing_identity_change_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, source = release_source("1.0.0")
            changed = config(root)
            changed["android_signing_certificate_sha256"] = "d" * 64
            with self.assertRaises(SyncError):
                sync_release(changed, source, accept_apk)
            self.assertFalse((root / "current").exists())

    def test_stale_release_cannot_replace_current(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, current = release_source("1.1.0")
            sync_release(config(root), current, accept_apk)
            _, stale = release_source("1.0.0")
            with self.assertRaises(SyncError):
                sync_release(config(root), stale, accept_apk)
            self.assertEqual((root / "current").resolve(), root / "releases" / "v1.1.0")

    def test_apksigner_output_is_bound_to_the_pinned_certificate(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "apksigner"
            tool.write_text(
                "#!/bin/sh\nprintf '%s\\n' 'Signer #1 certificate SHA-256 digest: " + "c" * 64 + "'\n",
                encoding="utf-8",
            )
            tool.chmod(0o700)
            apk = root / "Jarvis.apk"
            apk.write_bytes(b"fixture")
            _verify_android_apk(apk, "c" * 64, tool, 5)
            with self.assertRaises(SyncError):
                _verify_android_apk(apk, "d" * 64, tool, 5)


if __name__ == "__main__":
    unittest.main()
