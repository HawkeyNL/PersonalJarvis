from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from collect_tauri import collect


class CollectTauriTests(unittest.TestCase):
    def test_linux_bundle_is_renamed_canonically(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle = root / "bundle" / "appimage"
            bundle.mkdir(parents=True)
            (bundle / "Jarvis_0.1.0_amd64.AppImage").write_bytes(b"installer")
            updater = bundle / "Jarvis_0.1.0_amd64.AppImage.tar.gz"
            updater.write_bytes(b"updater")
            Path(f"{updater}.sig").write_text("signature", encoding="utf-8")
            output = root / "output"
            result = collect(root / "bundle", output, "linux", "0.1.0")
            self.assertEqual(
                [path.name for path in result],
                [
                    "Jarvis_0.1.0_linux_x86_64.AppImage.tar.gz",
                    "Jarvis_0.1.0_linux_x86_64.AppImage.tar.gz.sig",
                    "Jarvis_0.1.0_linux_x86_64.AppImage",
                ],
            )

    def test_unsigned_bundle_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Jarvis.AppImage.tar.gz").write_bytes(b"updater")
            (root / "Jarvis.AppImage").write_bytes(b"installer")
            with self.assertRaises(ValueError):
                collect(root, root / "out", "linux", "0.1.0")


if __name__ == "__main__":
    unittest.main()
