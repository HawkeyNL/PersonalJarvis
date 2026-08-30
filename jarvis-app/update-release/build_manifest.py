#!/usr/bin/env python3
"""Build a canonical Jarvis application release manifest from local artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import sys
from typing import Any

from manifest import SCHEMA_VERSION, validate_manifest


def _digest(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    if size == 0:
        raise ValueError(f"artifact is empty: {path}")
    return digest.hexdigest(), size


def build(descriptor: dict[str, Any], base_dir: Path) -> dict[str, Any]:
    release = descriptor.get("release")
    inputs = descriptor.get("artifacts")
    if not isinstance(release, dict) or not isinstance(inputs, list):
        raise ValueError("descriptor requires release and artifacts")
    output: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "release": release,
        "artifacts": [],
    }
    for item in inputs:
        if not isinstance(item, dict):
            raise ValueError("artifact descriptor must be an object")
        entry = {
            "platform": item["platform"],
            "architecture": item["architecture"],
            "distribution": item["distribution"],
        }
        signature = item["signature"]
        signature_value = signature.get("value")
        if "source" in signature:
            signature_value = (base_dir / signature["source"]).read_text(encoding="utf-8").strip()
        entry["signature"] = {"scheme": signature["scheme"], "value": signature_value}
        if "metadata" in item:
            entry["metadata"] = item["metadata"]
        if "source" in item:
            source = (base_dir / item["source"]).resolve()
            if not source.is_file():
                raise ValueError(f"artifact is missing: {source}")
            sha256, size = _digest(source)
            entry["artifact"] = {
                "path": item["published_path"],
                "sha256": sha256,
                "size": size,
            }
        else:
            entry["external"] = item["external"]
        output["artifacts"].append(entry)
    return validate_manifest(output)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--descriptor", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        descriptor_path = args.descriptor.resolve()
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        manifest = build(descriptor, descriptor_path.parent)
        encoded = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
        args.output.parent.mkdir(parents=True, exist_ok=True)
        temporary = args.output.with_suffix(args.output.suffix + ".tmp")
        temporary.write_text(encoded, encoding="utf-8")
        temporary.replace(args.output)
    except (OSError, KeyError, ValueError, json.JSONDecodeError) as error:
        print(f"release manifest rejected: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
