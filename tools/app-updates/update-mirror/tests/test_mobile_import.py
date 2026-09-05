import io
import json
import os
import shutil
import subprocess
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from sync import sync_release, SyncError
from mobile_import import MobileArchiveSource
from mobile_bundle import bundle
from test_sync import config


class MobileHandoffTests(unittest.TestCase):
    def fixture(self, root, version="1.0.0", code=10):
        assets = root / version
        assets.mkdir()
        base = f"Jarvis_{version}_android_universal"
        (assets / (base + ".apk")).write_bytes(b"signed APK fixture")
        (assets / (base + ".aab")).write_bytes(b"signed AAB fixture")
        (assets / (base + ".apk.cert-sha256")).write_text("c" * 64)
        archive = root / (version + ".tar")
        bundle(assets, archive, version, "a" * 40, "2026-09-01T00:00:00Z", code, "17")
        return archive

    def test_private_handoff_reuses_atomic_apk_verification_and_rollback(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            settings = config(root / "mobile-mirror")
            calls = []
            def verify(path, signer, *_):
                self.assertEqual(path.read_bytes(), b"signed APK fixture")
                self.assertEqual(signer, "c" * 64)
                calls.append(path.name)
            for version, code in (("1.0.0", 10), ("1.0.1", 11)):
                source = MobileArchiveSource(self.fixture(root, version, code))
                try:
                    self.assertEqual(sync_release(settings, source, apk_verifier=verify), version)
                finally:
                    source.close()
            self.assertEqual(len(calls), 2)
            self.assertTrue((root / "mobile-mirror/releases/v1.0.0/verified.json").is_file())
            before = (root / "mobile-mirror/current").resolve()
            source = MobileArchiveSource(self.fixture(root, "1.0.2", 12))
            try:
                def refuse(*_):
                    raise SyncError("APK signer invalid")
                with self.assertRaises(SyncError):
                    sync_release(settings, source, apk_verifier=refuse)
            finally:
                source.close()
            self.assertEqual((root / "mobile-mirror/current").resolve(), before)

    @unittest.skipUnless(os.environ.get("JARVIS_TEST_AGE") or shutil.which("age"), "age required for encrypted handoff fixture")
    def test_owner_encrypted_handoff_round_trip_and_tampering(self):
        age = os.environ.get("JARVIS_TEST_AGE") or shutil.which("age")
        keygen = str(Path(age).with_name("age-keygen"))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            key = root / "test-only-identity"
            subprocess.run([keygen, "-o", str(key)], check=True, capture_output=True)
            recipient = subprocess.run([keygen, "-y", str(key)], check=True, capture_output=True, text=True).stdout.strip()
            archive = self.fixture(root)
            encrypted, decrypted = root / "handoff.age", root / "handoff.tar"
            subprocess.run([age, "--encrypt", "--recipient", recipient, "--output", str(encrypted), str(archive)], check=True, capture_output=True)
            self.assertNotIn(b"signed APK fixture", encrypted.read_bytes())
            subprocess.run([age, "--decrypt", "--identity", str(key), "--output", str(decrypted), str(encrypted)], check=True, capture_output=True)
            source = MobileArchiveSource(decrypted)
            try:
                self.assertEqual(sync_release(config(root / "mirror"), source, apk_verifier=lambda *_: None), "1.0.0")
            finally:
                source.close()
            damaged = bytearray(encrypted.read_bytes())
            damaged[-1] ^= 1
            encrypted.write_bytes(damaged)
            result = subprocess.run([age, "--decrypt", "--identity", str(key), str(encrypted)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)

    def test_no_extraction_or_symlink_traversal(self):
        for kind in ("traversal", "symlink", "duplicate", "pax", "oversized"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                path = root / "bad.tar"
                with tarfile.open(path, "w:", format=tarfile.USTAR_FORMAT) as archive:
                    info = tarfile.TarInfo("../escaped" if kind == "traversal" else "latest.json")
                    info.size = 1
                    if kind == "symlink":
                        info.type = tarfile.SYMTYPE
                        info.linkname = "../escaped"
                    if kind == "pax":
                        info.type = tarfile.XHDTYPE
                    archive.addfile(info, io.BytesIO(b"x"))
                    if kind == "duplicate":
                        archive.addfile(info, io.BytesIO(b"x"))
                if kind == "oversized":
                    with path.open("r+b") as raw:
                        info.size = 2 * 1024**3 + 1
                        raw.write(info.tobuf(format=tarfile.USTAR_FORMAT))
                with self.assertRaises((ValueError, OSError)):
                    MobileArchiveSource(path)
                self.assertFalse((root.parent / "escaped").exists())

    def test_archive_symlink_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            real = self.fixture(root)
            alias = root / "alias"
            alias.symlink_to(real)
            with self.assertRaises(OSError):
                MobileArchiveSource(alias)

    def test_new_mobile_semver_cannot_reuse_an_old_android_code(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            settings = config(root / "mirror")
            first = MobileArchiveSource(self.fixture(root, "1.0.0", 10))
            second = MobileArchiveSource(self.fixture(root, "1.0.1", 10))
            try:
                sync_release(settings, first, apk_verifier=lambda *_: None)
                with self.assertRaisesRegex(SyncError, "versionCode"):
                    sync_release(settings, second, apk_verifier=lambda *_: None)
            finally:
                first.close()
                second.close()
