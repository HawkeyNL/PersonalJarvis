#!/usr/bin/env python3
"""Fail a trusted release when checked-in platform versions drift."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
import tomllib

from manifest import SEMVER_RE


def validate(repository: Path, version: str, android_code: int, ios_build: int) -> None:
    if not SEMVER_RE.fullmatch(version) or not 1 <= android_code <= 2_100_000_000 or ios_build < 1:
        raise ValueError("release versions are invalid")
    gradle = (repository / "jarvis-android/app/build.gradle.kts").read_text()
    android_name = re.search(r'JARVIS_APP_VERSION"\)\.orNull \?: "([^"]+)"', gradle)
    android_default_code = re.search(r'JARVIS_ANDROID_VERSION_CODE"\)\.orNull\?\.toIntOrNull\(\) \?: ([0-9]+)', gradle)
    if android_name is None or android_name.group(1) != version:
        raise ValueError("Android default versionName must match the requested release")
    if android_default_code is None or int(android_default_code.group(1)) > android_code:
        raise ValueError("Android versionCode must never move backwards")

    xcode = (repository / "jarvis-ios/Jarvis.xcodeproj/project.pbxproj").read_text()
    marketing_versions = set(re.findall(r"MARKETING_VERSION = ([^;]+);", xcode))
    build_numbers = {int(value) for value in re.findall(r"CURRENT_PROJECT_VERSION = ([0-9]+);", xcode)}
    if marketing_versions != {version}:
        raise ValueError("iOS MARKETING_VERSION must match the requested release")
    if build_numbers and max(build_numbers) > ios_build:
        raise ValueError("iOS build number must never move backwards")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--version", required=True)
    parser.add_argument("--android-version-code", required=True, type=int)
    parser.add_argument("--ios-build-number", required=True, type=int)
    args = parser.parse_args()
    try:
        validate(args.repository.resolve(), args.version, args.android_version_code, args.ios_build_number)
    except (OSError, KeyError, TypeError, ValueError, json.JSONDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"release version validation failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
