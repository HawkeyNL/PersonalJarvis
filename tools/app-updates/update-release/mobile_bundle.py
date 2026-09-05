#!/usr/bin/env python3
"""Prepare an owner-encrypted mobile handoff after both mobile jobs succeed."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile

from manifest import validate_manifest


def bundle(assets: Path, output: Path, version: str, revision: str, released_at: str,
           android_code: int, ios_build: str) -> None:
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("mobile version must be plain SemVer")
    base = f"Jarvis_{version}_android_universal"
    names = {base + suffix for suffix in (".apk", ".aab", ".apk.cert-sha256")}
    if {p.name for p in assets.iterdir()} != names:
        raise ValueError("mobile handoff must contain exactly APK, AAB and signer fingerprint")
    for name in names:
        path = assets / name
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= 2 * 1024**3:
            raise ValueError("mobile artifacts must be bounded regular files")
    apk = assets / (base + ".apk")
    with apk.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    fingerprint_file = assets / (base + ".apk.cert-sha256")
    if fingerprint_file.stat().st_size > 65:
        raise ValueError("mobile signer fingerprint exceeds size limit")
    fingerprint = fingerprint_file.read_text().strip()
    manifest = validate_manifest({"schema_version": 1, "release": {
        "version": version, "tag": f"app-v{version}", "product": "mobile",
        "source_revision": revision, "client_protocol": 1, "minimum_client_protocol": 1,
        "released_at": released_at, "channel": "stable",
    }, "artifacts": [
        {"platform": "android", "architecture": "universal", "distribution": "home-node-apk",
         "artifact": {"path": f"releases/v{version}/android-universal/{apk.name}", "sha256": digest, "size": apk.stat().st_size},
         "metadata": {"version_code": android_code},
         "signature": {"scheme": "android-apk-signing-certificate-sha256", "value": fingerprint}},
        {"platform": "ios", "architecture": "arm64", "distribution": "testflight",
         "external": {"bundle_id": "com.hawkeynl.jarvis", "build_number": ios_build},
         "signature": {"scheme": "apple-code-signing", "value": "app-store-connect"}},
    ]})
    data = json.dumps(manifest, sort_keys=True).encode()
    with tarfile.open(output, "x:", format=tarfile.USTAR_FORMAT) as archive:
        metadata = tarfile.TarInfo("latest.json")
        metadata.mode = 0o600
        metadata.size = len(data)
        archive.addfile(metadata, io.BytesIO(data))
        for name in sorted(names):
            archive.add(assets / name, arcname=name, recursive=False)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--released-at", required=True)
    parser.add_argument("--android-code", type=int, required=True)
    parser.add_argument("--ios-build", required=True)
    args = parser.parse_args()
    bundle(args.assets, args.output, args.version, args.revision, args.released_at, args.android_code, args.ios_build)
