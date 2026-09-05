from io import BytesIO
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from sync import GitHubReleaseSource, SyncError, _safe_config, _prepare_mirror, sync_release
import signatures
from test_sync import config, release_source

MINISIGN = os.environ.get("JARVIS_TEST_MINISIGN") or shutil.which("minisign")


class PublicSourceTests(unittest.TestCase):
    def test_draft_prerelease_and_malformed_github_metadata_are_rejected(self):
        valid = {"draft": False, "prerelease": False, "tag_name": "app-v1.0.0", "assets": []}
        for invalid in (dict(valid, draft=True), dict(valid, prerelease=True),
                        dict(valid, assets={}), dict(valid, tag_name=None), [], None):
            source = GitHubReleaseSource("owner/app", "", 5)
            source._open = Mock(return_value=BytesIO(json.dumps(invalid).encode()))
            with self.subTest(invalid=invalid), self.assertRaises(SyncError):
                source.open_manifest()

    def test_rate_limit_keeps_the_current_generation(self):
        with tempfile.TemporaryDirectory() as temporary:
            settings = config(Path(temporary) / "mirror")
            _, prior = release_source("1.0.0")
            sync_release(settings, prior, apk_verifier=lambda *args: None)
            current = (Path(temporary) / "mirror/current").resolve()
            source = GitHubReleaseSource("owner/app", "", 5)
            error = HTTPError("https://api.github.com/", 429, "rate limit", {}, None)
            source._open = Mock(side_effect=error)
            with self.assertRaises(HTTPError), error:
                sync_release(settings, source)
            self.assertEqual((Path(temporary) / "mirror/current").resolve(), current)

    def test_lock_symlink_and_parent_symlink_fail_before_opening(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / "mirror"
            root.mkdir(mode=0o750)
            outside = base / "sentinel"
            outside.write_bytes(b"unchanged")
            (root / "sync.lock").symlink_to(outside)
            with self.assertRaises(SyncError):
                _prepare_mirror(root)
            self.assertEqual(outside.read_bytes(), b"unchanged")
            alias = base / "alias"
            alias.symlink_to(root, target_is_directory=True)
            with self.assertRaises(SyncError):
                _prepare_mirror(alias / "new")
            self.assertFalse((root / "new").exists())

    def test_public_request_omits_authorization(self):
        source = GitHubReleaseSource("HawkeyNL/PersonalJarvisApp", "", 5)
        source._opener = Mock()
        source._open("https://api.github.com/repos/HawkeyNL/PersonalJarvisApp/releases/latest", "application/json")
        request = source._opener.open.call_args.args[0]
        self.assertIsNone(request.get_header("Authorization"))

    def test_optional_authentication_is_a_header_only(self):
        source = GitHubReleaseSource("owner/app", "test-secret", 5)
        source._opener = Mock()
        source._open("https://api.github.com/repos/owner/app/releases/latest", "application/json")
        request = source._opener.open.call_args.args[0]
        self.assertEqual(request.get_header("Authorization"), "Bearer test-secret")
        self.assertNotIn("test-secret", request.full_url)

    def test_repository_injection_is_rejected(self):
        for repository in ("../app", "owner/..", "owner/app/extra", "owner/app?token=x", "owner/app\n", "https://evil.invalid"):
            with self.subTest(repository=repository), self.assertRaises(SyncError):
                GitHubReleaseSource(repository, "", 5)

    def test_public_config_needs_no_token(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            value = config(root / "mirror")
            value.pop("android_signing_certificate_sha256")
            value.pop("android_apksigner_path")
            value["source"] = {"kind": "github-releases", "repository": "HawkeyNL/PersonalJarvisApp"}
            value["tauri_signing_public_key"] = base64.b64encode(b"test-public-document").decode()
            path = root / "config.json"
            path.write_text(json.dumps(value))
            path.chmod(0o600)
            self.assertNotIn("bearer_token_file", _safe_config(path)["source"])

    def test_android_artifact_requires_explicit_verifier_configuration(self):
        with tempfile.TemporaryDirectory() as temporary:
            settings = config(Path(temporary) / "mirror")
            settings.pop("android_signing_certificate_sha256")
            settings.pop("android_apksigner_path")
            _, source = release_source("1.0.0")
            with self.assertRaisesRegex(SyncError, "explicitly configured APK verification"):
                sync_release(settings, source)
            self.assertFalse((Path(temporary) / "mirror/current").exists())

    def test_symlink_mirror_cannot_write_outside_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            outside = base / "outside"
            outside.mkdir()
            root = base / "mirror"
            root.mkdir()
            (root / ".staging").symlink_to(outside, target_is_directory=True)
            _, source = release_source("1.0.0")
            with self.assertRaises(SyncError):
                sync_release(config(root), source)
            self.assertEqual(list(outside.iterdir()), [])


@unittest.skipUnless(MINISIGN, "minisign required for cryptographic fixture")
class SignedClientFlowTests(unittest.TestCase):
    def test_signed_public_release_activation_and_failures(self):
        with tempfile.TemporaryDirectory() as temporary, patch.object(signatures, "MINISIGN", MINISIGN):
            base = Path(temporary)
            private, public = base / "test.key", base / "test.pub"
            subprocess.run([MINISIGN, "-G", "-W", "-s", str(private), "-p", str(public)], check=True, capture_output=True)
            public_key = base64.b64encode(public.read_bytes()).decode()

            def sign(content):
                payload = base / "payload"
                payload.write_bytes(content)
                signature = base / "payload.minisig"
                subprocess.run([MINISIGN, "-S", "-s", str(private), "-m", str(payload), "-x", str(signature)],
                               check=True, capture_output=True)
                return base64.b64encode(signature.read_bytes()).decode()

            class SignedSource(GitHubReleaseSource):
                def __init__(self, version):
                    super().__init__("owner/app", "", 5)
                    self.version = version
                    manifest, _ = release_source(version, product="clients")
                    self.payloads = {}
                    for entry in manifest["artifacts"]:
                        if "artifact" not in entry:
                            continue
                        payload = f"signed fixture {version} {entry['platform']}".encode()
                        entry["artifact"]["size"] = len(payload)
                        entry["artifact"]["sha256"] = hashlib.sha256(payload).hexdigest()
                        if entry["distribution"] == "home-node-updater":
                            entry["signature"]["value"] = sign(payload)
                        self.payloads[entry["artifact"]["path"]] = payload
                    for installer in manifest["installers"]:
                        payload = f"installer fixture {version} {installer['platform']}".encode()
                        installer["artifact"]["size"] = len(payload)
                        installer["artifact"]["sha256"] = hashlib.sha256(payload).hexdigest()
                        self.payloads[installer["artifact"]["path"]] = payload
                    self.manifest = json.dumps(manifest).encode()
                    self.signature = sign(self.manifest)

                def open_manifest(self):
                    return BytesIO(self.manifest)

                def open_manifest_signature(self):
                    return BytesIO(self.signature.encode())

                def validate_identity(self, version):
                    if version != self.version:
                        raise SyncError("release mismatch")

                def open_artifact(self, version, path):
                    return BytesIO(self.payloads[path])

            settings = config(base / "mirror")
            settings["tauri_signing_public_key"] = public_key
            self.assertEqual(
                sync_release(settings, SignedSource("1.0.0"), apk_verifier=lambda *args: None),
                "1.0.0",
            )
            current = (base / "mirror/current").resolve()
            for failure in ("manifest", "hash", "signature", "identity", "installer"):
                source = SignedSource("1.0.1")
                if failure == "manifest":
                    source.manifest += b" "
                elif failure == "hash":
                    source.payloads[next(iter(source.payloads))] = b"corrupt"
                elif failure == "signature":
                    document = json.loads(source.manifest)
                    document["artifacts"][0]["signature"]["value"] = sign(b"wrong artifact")
                    source.manifest = json.dumps(document).encode()
                    source.signature = sign(source.manifest)
                elif failure == "installer":
                    source.payloads[next(path for path in source.payloads if path.endswith(".dmg"))] = b"corrupt installer"
                else:
                    source.version = "9.0.0"
                with self.subTest(failure=failure), self.assertRaises((SyncError, ValueError)):
                    sync_release(settings, source, apk_verifier=lambda *args: None)
                self.assertEqual((base / "mirror/current").resolve(), current)
            self.assertEqual(
                sync_release(settings, SignedSource("1.0.1"), apk_verifier=lambda *args: None),
                "1.0.1",
            )
            self.assertTrue(current.is_dir())
            with self.assertRaises(SyncError):
                sync_release(settings, SignedSource("1.0.0"), apk_verifier=lambda *args: None)
