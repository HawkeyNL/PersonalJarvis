#!/usr/bin/env python3
"""Collect one Tauri updater bundle into canonical private-release names."""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import sys


POLICY = {
    "windows": {
        "architecture": "x86_64",
        "updater": "*.nsis.zip",
        "installer": "*-setup.exe",
        "updater_suffix": ".nsis.zip",
        "installer_suffix": ".exe",
    },
    "macos": {
        "architecture": "arm64",
        "updater": "*.app.tar.gz",
        "installer": "*.dmg",
        "updater_suffix": ".app.tar.gz",
        "installer_suffix": ".dmg",
    },
    "linux": {
        "architecture": "x86_64",
        "updater": "*.AppImage.tar.gz",
        "installer": "*.AppImage",
        "updater_suffix": ".AppImage.tar.gz",
        "installer_suffix": ".AppImage",
    },
}


def _one(root: Path, pattern: str) -> Path:
    matches = [path for path in root.rglob(pattern) if path.is_file()]
    if len(matches) != 1:
        raise ValueError(f"expected one {pattern} artifact, found {len(matches)}")
    return matches[0]


def collect(root: Path, output: Path, platform: str, version: str) -> list[Path]:
    policy = POLICY[platform]
    updater = _one(root, policy["updater"])
    signature = Path(f"{updater}.sig")
    if not signature.is_file():
        raise ValueError("Tauri updater signature is missing")
    installer = _one(root, policy["installer"])
    output.mkdir(parents=True, exist_ok=True)
    base = f"Jarvis_{version}_{platform}_{policy['architecture']}"
    destinations = [
        output / f"{base}{policy['updater_suffix']}",
        output / f"{base}{policy['updater_suffix']}.sig",
        output / f"{base}{policy['installer_suffix']}",
    ]
    for source, destination in zip((updater, signature, installer), destinations, strict=True):
        shutil.copy2(source, destination)
    return destinations


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--platform", required=True, choices=sorted(POLICY))
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        for path in collect(args.bundle_root, args.output, args.platform, args.version):
            print(path)
    except (OSError, ValueError) as error:
        print(f"Tauri release collection failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

