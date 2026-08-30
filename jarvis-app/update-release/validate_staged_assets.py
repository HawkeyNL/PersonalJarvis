#!/usr/bin/env python3
"""Reject missing or unexpected files in a private application release draft."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

from manifest import SEMVER_RE


def expected_assets(version: str, include_manifests: bool = False) -> set[str]:
    if not SEMVER_RE.fullmatch(version):
        raise ValueError("version is not SemVer")
    base = f"Jarvis_{version}"
    assets = {
        f"{base}_windows_x86_64.nsis.zip",
        f"{base}_windows_x86_64.nsis.zip.sig",
        f"{base}_windows_x86_64.exe",
        f"{base}_macos_arm64.app.tar.gz",
        f"{base}_macos_arm64.app.tar.gz.sig",
        f"{base}_macos_arm64.dmg",
        f"{base}_linux_x86_64.AppImage.tar.gz",
        f"{base}_linux_x86_64.AppImage.tar.gz.sig",
        f"{base}_linux_x86_64.AppImage",
        f"{base}_android_universal.apk",
        f"{base}_android_universal.aab",
        f"{base}_android_universal.apk.cert-sha256",
        f"{base}_ios_arm64.testflight.json",
    }
    if include_manifests:
        assets.update({"latest.json", f"{base}_manifest_v1.json"})
    return assets


def validate(directory: Path, version: str, include_manifests: bool = False) -> None:
    if not directory.is_dir():
        raise ValueError("staged asset directory is missing")
    actual = {path.name for path in directory.iterdir() if path.is_file()}
    expected = expected_assets(version, include_manifests)
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        raise ValueError(f"private draft asset set is invalid; missing={missing}, unexpected={unexpected}")
    if any((directory / name).stat().st_size == 0 for name in expected):
        raise ValueError("private draft contains an empty asset")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--include-manifests", action="store_true")
    args = parser.parse_args()
    try:
        validate(args.assets, args.version, args.include_manifests)
    except (OSError, ValueError) as error:
        print(f"private draft validation failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
