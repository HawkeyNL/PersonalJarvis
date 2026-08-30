#!/usr/bin/env python3
"""Create the five-platform descriptor consumed by build_manifest.py."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

from manifest import SEMVER_RE


DESKTOP = (
    ("windows", "x86_64", ".nsis.zip"),
    ("macos", "arm64", ".app.tar.gz"),
    ("linux", "x86_64", ".AppImage.tar.gz"),
)


def descriptor(assets: Path, version: str, released_at: str, ios_build_number: str) -> dict:
    if not SEMVER_RE.fullmatch(version):
        raise ValueError("version is not SemVer")
    artifacts = []
    for platform, architecture, suffix in DESKTOP:
        filename = f"Jarvis_{version}_{platform}_{architecture}{suffix}"
        signature = f"{filename}.sig"
        if not (assets / filename).is_file() or not (assets / signature).is_file():
            raise ValueError(f"{platform} updater bundle or signature is missing")
        artifacts.append({
            "platform": platform,
            "architecture": architecture,
            "distribution": "home-node-updater",
            "source": filename,
            "published_path": f"releases/v{version}/{platform}-{architecture}/{filename}",
            "signature": {"scheme": "tauri-minisign", "source": signature},
        })
    android = f"Jarvis_{version}_android_universal.apk"
    fingerprint_file = assets / f"{android}.cert-sha256"
    if not (assets / android).is_file() or not fingerprint_file.is_file():
        raise ValueError("signed Android APK or certificate fingerprint is missing")
    fingerprint = fingerprint_file.read_text(encoding="utf-8").strip().lower()
    artifacts.append({
        "platform": "android",
        "architecture": "universal",
        "distribution": "home-node-apk",
        "source": android,
        "published_path": f"releases/v{version}/android-universal/{android}",
        "signature": {"scheme": "android-apk-signing-certificate-sha256", "value": fingerprint},
    })
    artifacts.append({
        "platform": "ios",
        "architecture": "arm64",
        "distribution": "testflight",
        "external": {"bundle_id": "com.hawkeynl.jarvis", "build_number": ios_build_number},
        "signature": {"scheme": "apple-code-signing", "value": "app-store-connect"},
    })
    return {
        "release": {
            "version": version,
            "channel": "stable",
            "released_at": released_at,
            "minimum_client_protocol": 1,
            "notes": f"Jarvis application {version}",
        },
        "artifacts": artifacts,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--released-at", required=True)
    parser.add_argument("--ios-build-number", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        value = descriptor(args.assets, args.version, args.released_at, args.ios_build_number)
        args.output.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"release descriptor failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

