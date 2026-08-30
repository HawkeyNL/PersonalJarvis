#!/usr/bin/env python3
"""Pull and atomically activate a verified private Jarvis application release."""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import tempfile
from typing import Any, BinaryIO, Protocol
from urllib.parse import urljoin, urlparse
from urllib.request import Request, urlopen
import uuid

UPDATE_RELEASE = Path(__file__).resolve().parents[1] / "update-release"
sys.path.insert(0, str(UPDATE_RELEASE))

from manifest import MAX_MANIFEST_BYTES, ManifestError, mirrored_artifacts, validate_manifest  # noqa: E402


class SyncError(RuntimeError):
    """The remote release could not be safely activated."""


class Source(Protocol):
    def open(self, url: str) -> BinaryIO: ...


class HttpSource:
    def __init__(self, token: str, timeout_seconds: int) -> None:
        self._token = token
        self._timeout = timeout_seconds

    def open(self, url: str) -> BinaryIO:
        request = Request(url, headers={"Authorization": f"Bearer {self._token}", "User-Agent": "jarvis-app-update-sync/1"})
        return urlopen(request, timeout=self._timeout)


def _read_limited(source: BinaryIO, limit: int) -> bytes:
    value = source.read(limit + 1)
    if len(value) > limit:
        raise SyncError("remote manifest exceeds the size limit")
    return value


def _safe_config(path: Path) -> dict[str, Any]:
    metadata = path.stat()
    if metadata.st_mode & (stat.S_IWGRP | stat.S_IRWXO):
        raise SyncError("configuration permissions must be 0640 or stricter")
    if os.geteuid() == 0 and metadata.st_uid != 0:
        raise SyncError("configuration must be root-owned")
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise SyncError("configuration must be an object")
    expected = {"schema_version", "source", "mirror_root", "channel", "retention_previous", "timeout_seconds", "max_artifact_bytes"}
    if set(document) != expected or document["schema_version"] != 1:
        raise SyncError("configuration fields are invalid")
    source = document["source"]
    if not isinstance(source, dict) or set(source) != {"manifest_url", "artifact_base_url", "bearer_token_file"}:
        raise SyncError("source configuration is invalid")
    for field in ("manifest_url", "artifact_base_url"):
        parsed = urlparse(source[field])
        if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password or parsed.fragment:
            raise SyncError(f"source.{field} must be a credential-free HTTPS URL")
    if document["channel"] not in ("stable", "beta", "development"):
        raise SyncError("channel is invalid")
    if document["retention_previous"] != 1:
        raise SyncError("retention_previous must be exactly 1")
    if not isinstance(document["timeout_seconds"], int) or not 1 <= document["timeout_seconds"] <= 600:
        raise SyncError("timeout_seconds is invalid")
    if not isinstance(document["max_artifact_bytes"], int) or not 1 <= document["max_artifact_bytes"] <= 8 * 1024**3:
        raise SyncError("max_artifact_bytes is invalid")
    root = Path(document["mirror_root"])
    if not root.is_absolute() or root == Path("/"):
        raise SyncError("mirror_root must be a bounded absolute path")
    token_path = Path(source["bearer_token_file"])
    if not token_path.is_absolute():
        raise SyncError("bearer_token_file must be absolute")
    return document


def _read_token(path: Path) -> str:
    metadata = path.stat()
    if metadata.st_mode & stat.S_IRWXG or metadata.st_mode & stat.S_IRWXO:
        raise SyncError("artifact credential permissions must be 0600")
    if os.geteuid() == 0 and metadata.st_uid != 0:
        raise SyncError("artifact credential must be root-owned")
    token = path.read_text(encoding="utf-8").strip()
    if not token or len(token) > 4096 or any(character.isspace() for character in token):
        raise SyncError("artifact credential is invalid")
    return token


def _download(source: Source, url: str, destination: Path, expected_hash: str, expected_size: int, maximum: int) -> None:
    if expected_size > maximum:
        raise SyncError("artifact exceeds the configured size limit")
    digest = hashlib.sha256()
    size = 0
    destination.parent.mkdir(parents=True, exist_ok=True)
    with contextlib.closing(source.open(url)) as response, destination.open("xb") as output:
        while True:
            chunk = response.read(1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum or size > expected_size:
                raise SyncError("artifact size does not match the manifest")
            digest.update(chunk)
            output.write(chunk)
        output.flush()
        os.fsync(output.fileno())
    if size != expected_size or digest.hexdigest() != expected_hash:
        raise SyncError("artifact hash or size does not match the manifest")


def _version_key(name: str) -> tuple[int, int, int, str] | None:
    if not name.startswith("v"):
        return None
    base, _, suffix = name[1:].partition("-")
    parts = base.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        return None
    return int(parts[0]), int(parts[1]), int(parts[2]), suffix


def _atomic_write(path: Path, value: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_name, path)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temporary_name)
        raise


def _activate(root: Path, version: str, staged_release: Path, manifest_bytes: bytes) -> None:
    releases = root / "releases"
    releases.mkdir(parents=True, exist_ok=True)
    destination = releases / f"v{version}"
    if destination.exists():
        existing_manifest = destination / "manifest.json"
        if not existing_manifest.is_file() or existing_manifest.read_bytes() != manifest_bytes:
            raise SyncError("an immutable release version already exists with different contents")
        shutil.rmtree(staged_release)
    else:
        os.replace(staged_release, destination)

    temporary_link = root / f".current.{uuid.uuid4().hex}"
    os.symlink(PurePosixPath("releases", f"v{version}"), temporary_link)
    try:
        os.replace(temporary_link, root / "current")
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary_link.unlink()
    _atomic_write(root / "manifests" / "latest.json", manifest_bytes)


def _apply_retention(root: Path, current_version: str) -> None:
    releases = root / "releases"
    candidates: list[tuple[tuple[int, int, int, str], Path]] = []
    for child in releases.iterdir():
        if not child.is_dir() or child.is_symlink():
            continue
        key = _version_key(child.name)
        if key is not None and (child / "verified.json").is_file() and (child / "manifest.json").is_file():
            candidates.append((key, child))
    candidates.sort(reverse=True)
    current = releases / f"v{current_version}"
    keep = {current}
    previous = next((path for _, path in candidates if path != current), None)
    if previous is not None:
        keep.add(previous)
    for _, path in candidates:
        if path not in keep:
            shutil.rmtree(path)


def sync_release(config: dict[str, Any], source: Source) -> str:
    root = Path(config["mirror_root"])
    root.mkdir(parents=True, exist_ok=True)
    for directory in (root / ".staging", root / "releases", root / "manifests"):
        directory.mkdir(mode=0o750, parents=True, exist_ok=True)

    with source.open(config["source"]["manifest_url"]) as response:
        manifest_bytes = _read_limited(response, MAX_MANIFEST_BYTES)
    try:
        manifest = validate_manifest(json.loads(manifest_bytes))
    except (json.JSONDecodeError, ManifestError) as error:
        raise SyncError(f"release manifest rejected: {error}") from error
    release = manifest["release"]
    if release["channel"] != config["channel"]:
        raise SyncError("release channel does not match the configured channel")
    version = release["version"]

    staging = root / ".staging" / uuid.uuid4().hex
    staged_release = staging / f"v{version}"
    staged_release.mkdir(parents=True)
    try:
        for entry in mirrored_artifacts(manifest):
            artifact = entry["artifact"]
            filename = PurePosixPath(artifact["path"]).name
            relative = Path(f"{entry['platform']}-{entry['architecture']}") / filename
            url = urljoin(config["source"]["artifact_base_url"].rstrip("/") + "/", artifact["path"])
            _download(source, url, staged_release / relative, artifact["sha256"], artifact["size"], config["max_artifact_bytes"])
        canonical = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"
        (staged_release / "manifest.json").write_bytes(canonical)
        verification = {
            "schema_version": 1,
            "version": version,
            "manifest_sha256": hashlib.sha256(canonical).hexdigest(),
        }
        (staged_release / "verified.json").write_text(json.dumps(verification, sort_keys=True) + "\n", encoding="utf-8")
        _activate(root, version, staged_release, canonical)
        _apply_retention(root, version)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, default=Path("/etc/jarvis/app-updates/config.json"))
    args = parser.parse_args()
    try:
        config = _safe_config(args.config)
        token = _read_token(Path(config["source"]["bearer_token_file"]))
        root = Path(config["mirror_root"])
        root.mkdir(parents=True, exist_ok=True)
        with (root / "sync.lock").open("a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            version = sync_release(config, HttpSource(token, config["timeout_seconds"]))
        print(f"activated Jarvis application release v{version}")
    except BlockingIOError:
        print("another application update sync is already running", file=sys.stderr)
        return 3
    except (OSError, ValueError, json.JSONDecodeError, SyncError) as error:
        print(f"application update sync failed: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
