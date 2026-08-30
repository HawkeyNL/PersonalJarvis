from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from validate_staged_assets import expected_assets, validate


class StagedAssetTests(unittest.TestCase):
    def test_exact_platform_asset_set_is_required(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in expected_assets("1.2.3"):
                (root / name).write_bytes(b"asset")
            validate(root, "1.2.3")
            (root / "unexpected.bin").write_bytes(b"unexpected")
            with self.assertRaises(ValueError):
                validate(root, "1.2.3")

    def test_final_set_requires_both_manifests(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in expected_assets("1.2.3", include_manifests=True):
                (root / name).write_bytes(b"asset")
            validate(root, "1.2.3", include_manifests=True)
            (root / "latest.json").unlink()
            with self.assertRaises(ValueError):
                validate(root, "1.2.3", include_manifests=True)


if __name__ == "__main__":
    unittest.main()
