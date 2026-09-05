#!/usr/bin/env python3
"""Reject stable releases that do not advance every platform version."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

from manifest import ManifestError, validate_manifest


STABLE_SEMVER_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def _stable_version(value: str) -> tuple[int, int, int]:
    match = STABLE_SEMVER_RE.fullmatch(value)
    if match is None:
        raise ValueError("trusted stable releases require plain SemVer")
    return tuple(map(int, match.groups()))


def validate_progression(
    previous: dict,
    version: str,
    android_version_code: int,
    ios_build_number: int,
) -> None:
    validate_manifest(previous)
    if _stable_version(version) <= _stable_version(previous["release"]["version"]):
        raise ValueError("application version must increase over the private latest release")

    android = next(entry for entry in previous["artifacts"] if entry["platform"] == "android")
    if android_version_code <= android["metadata"]["version_code"]:
        raise ValueError("Android versionCode must increase over the private latest release")

    ios = next(entry for entry in previous["artifacts"] if entry["platform"] == "ios")
    if ios_build_number <= int(ios["external"]["build_number"]):
        raise ValueError("iOS build number must increase over the private latest release")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--android-version-code", required=True, type=int)
    parser.add_argument("--ios-build-number", required=True, type=int)
    args = parser.parse_args()
    try:
        document = json.loads(args.manifest.read_text(encoding="utf-8"))
        validate_progression(document, args.version, args.android_version_code, args.ios_build_number)
    except (OSError, TypeError, ValueError, json.JSONDecodeError, ManifestError) as error:
        print(f"previous release validation failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
