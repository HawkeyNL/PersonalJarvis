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
import re
import shutil
import stat
import subprocess
import sys
from typing import Any, BinaryIO, Callable, Protocol
from urllib.parse import quote, urlparse
from urllib.request import HTTPRedirectHandler, Request, build_opener
import uuid

UPDATE_RELEASE = Path(__file__).resolve().parents[1] / "update-release"
sys.path.insert(0, str(UPDATE_RELEASE))

from manifest import MAX_MANIFEST_BYTES, ManifestError, mirrored_artifacts, validate_manifest  # noqa: E402


class SyncError(RuntimeError):
    """The remote release could not be safely activated."""


class Source(Protocol):
    def open_manifest(self) -> BinaryIO: ...

    def open_artifact(self, version: str, path: str) -> BinaryIO: ...


def _origin(url: str) -> tuple[str, str, int]:
    parsed = urlparse(url)
    scheme = parsed.scheme.lower()
    hostname = parsed.hostname
    try:
        port = parsed.port
    except ValueError as error:
        raise SyncError("redirect URL has an invalid origin") from error
    if not scheme or hostname is None:
        raise SyncError("redirect URL has an invalid origin")
    if port is None:
        port = {"http": 80, "https": 443}.get(scheme)
    if port is None:
        raise SyncError("redirect URL has an invalid origin")
    return scheme, hostname.lower(), port


class _SafeRedirectHandler(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, new_url):  # type: ignore[no-untyped-def]
        source_origin = _origin(request.full_url)
        target_origin = _origin(new_url)
        if target_origin[0] != "https":
            raise SyncError("refusing to follow a non-HTTPS redirect")
        redirected = super().redirect_request(request, fp, code, message, headers, new_url)
        if redirected is not None and source_origin != target_origin:
            redirected.remove_header("Authorization")
        return redirected


class HttpTemplateSource:
    def __init__(self, config: dict[str, Any], token: str, timeout_seconds: int) -> None:
        self._manifest_url = config["manifest_url"]
        self._artifact_url_template = config["artifact_url_template"]
        self._token = token
        self._timeout = timeout_seconds
        self._opener = build_opener(_SafeRedirectHandler())

    def _open(self, url: str) -> BinaryIO:
        request = Request(
            url,
            headers={
                "Accept": "application/octet-stream",
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "jarvis-app-update-sync/1",
            },
        )
        return self._opener.open(request, timeout=self._timeout)

    def open_manifest(self) -> BinaryIO:
        return self._open(self._manifest_url)

    def open_artifact(self, version: str, path: str) -> BinaryIO:
        filename = PurePosixPath(path).name
        url = self._artifact_url_template
        url = url.replace("{{version}}", version)
        url = url.replace("{{filename}}", filename)
        url = url.replace("{{path}}", path)
        return self._open(url)


class GitHubReleaseSource:
    _MAX_RELEASE_METADATA_BYTES = 2 * 1024 * 1024

    def __init__(self, repository: str, token: str, timeout_seconds: int) -> None:
        self._repository = repository
        self._token = token
        self._timeout = timeout_seconds
        self._opener = build_opener(_SafeRedirectHandler())
        self._releases: dict[str, dict[str, Any]] = {}

    def _open(self, url: str, accept: str) -> BinaryIO:
        request = Request(
            url,
            headers={
                "Accept": accept,
                "Authorization": f"Bearer {self._token}",
                "User-Agent": "jarvis-app-update-sync/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        return self._opener.open(request, timeout=self._timeout)

    def _release(self, tag: str) -> dict[str, Any]:
        if tag in self._releases:
            return self._releases[tag]
        suffix = "latest" if tag == "latest" else f"tags/{quote(tag, safe='')}"
        url = f"https://api.github.com/repos/{self._repository}/releases/{suffix}"
        with contextlib.closing(self._open(url, "application/vnd.github+json")) as response:
            try:
                release = json.loads(_read_limited(response, self._MAX_RELEASE_METADATA_BYTES))
            except json.JSONDecodeError as error:
                raise SyncError("GitHub release metadata is malformed") from error
        if (
            not isinstance(release, dict)
            or release.get("draft") is not False
            or release.get("prerelease") is not False
            or not isinstance(release.get("tag_name"), str)
            or not isinstance(release.get("assets"), list)
        ):
            raise SyncError("GitHub release metadata violates the stable release policy")
        if tag != "latest" and release["tag_name"] != tag:
            raise SyncError("GitHub returned an unexpected release tag")
        self._releases[tag] = release
        return release

    def _asset(self, tag: str, name: str) -> BinaryIO:
        assets = self._release(tag)["assets"]
        matches = [asset for asset in assets if isinstance(asset, dict) and asset.get("name") == name]
        if len(matches) != 1:
            raise SyncError("GitHub release does not contain exactly one expected asset")
        url = matches[0].get("url")
        expected_prefix = f"https://api.github.com/repos/{self._repository}/releases/assets/"
        if not isinstance(url, str) or not url.startswith(expected_prefix) or not url.removeprefix(expected_prefix).isdigit():
            raise SyncError("GitHub release asset URL is invalid")
        return self._open(url, "application/octet-stream")

    def open_manifest(self) -> BinaryIO:
        return self._asset("latest", "latest.json")

    def open_artifact(self, version: str, path: str) -> BinaryIO:
        return self._asset(f"app-v{version}", PurePosixPath(path).name)


def _read_limited(source: BinaryIO, limit: int) -> bytes:
    chunks: list[bytes] = []
    size = 0
    while True:
        chunk = source.read(min(64 * 1024, limit + 1 - size))
        if not chunk:
            return b"".join(chunks)
        chunks.append(chunk)
        size += len(chunk)
        if size > limit:
            raise SyncError("remote metadata exceeds the size limit")


def _safe_config(path: Path) -> dict[str, Any]:
    metadata = path.stat()
    if metadata.st_mode & (stat.S_IWGRP | stat.S_IRWXO):
        raise SyncError("configuration permissions must be 0640 or stricter")
    if os.geteuid() == 0 and metadata.st_uid != 0:
        raise SyncError("configuration must be root-owned")
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise SyncError("configuration must be an object")
    expected = {
        "schema_version",
        "source",
        "mirror_root",
        "channel",
        "retention_previous",
        "timeout_seconds",
        "max_artifact_bytes",
        "android_signing_certificate_sha256",
        "android_apksigner_path",
    }
    if set(document) != expected or document["schema_version"] != 1:
        raise SyncError("configuration fields are invalid")
    source = document["source"]
    if not isinstance(source, dict) or "kind" not in source or "bearer_token_file" not in source:
        raise SyncError("source configuration is invalid")
    if source["kind"] == "github-releases":
        if set(source) != {"kind", "repository", "bearer_token_file"}:
            raise SyncError("GitHub source configuration is invalid")
        repository = source["repository"]
        if not isinstance(repository, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise SyncError("source.repository is invalid")
    elif source["kind"] == "http-template":
        if set(source) != {"kind", "manifest_url", "artifact_url_template", "bearer_token_file"}:
            raise SyncError("HTTP source configuration is invalid")
        for field in ("manifest_url", "artifact_url_template"):
            if not isinstance(source[field], str):
                raise SyncError(f"source.{field} must be a credential-free HTTPS URL")
            parsed = urlparse(source[field])
            if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password or parsed.fragment:
                raise SyncError(f"source.{field} must be a credential-free HTTPS URL")
        template = source["artifact_url_template"]
        if "{{filename}}" not in template and "{{path}}" not in template:
            raise SyncError("source.artifact_url_template must identify an artifact")
        scrubbed = template.replace("{{version}}", "version").replace("{{filename}}", "artifact").replace("{{path}}", "path")
        if "{{" in scrubbed or "}}" in scrubbed:
            raise SyncError("source.artifact_url_template contains an unsupported placeholder")
        if _origin(source["manifest_url"]) != _origin(scrubbed):
            raise SyncError("HTTP manifest and artifact URLs must use the same origin")
    else:
        raise SyncError("source.kind is unsupported")
    if document["channel"] != "stable":
        raise SyncError("only the stable application update channel is currently enabled")
    if isinstance(document["retention_previous"], bool) or document["retention_previous"] != 1:
        raise SyncError("retention_previous must be exactly 1")
    fingerprint = document["android_signing_certificate_sha256"]
    if not isinstance(fingerprint, str) or len(fingerprint) != 64 or any(character not in "0123456789abcdef" for character in fingerprint):
        raise SyncError("android_signing_certificate_sha256 is invalid")
    apksigner_value = document["android_apksigner_path"]
    if not isinstance(apksigner_value, str) or not apksigner_value:
        raise SyncError("android_apksigner_path is invalid")
    apksigner_path = Path(apksigner_value)
    if not apksigner_path.is_absolute():
        raise SyncError("android_apksigner_path must be absolute")
    if (
        not isinstance(document["timeout_seconds"], int)
        or isinstance(document["timeout_seconds"], bool)
        or not 1 <= document["timeout_seconds"] <= 600
    ):
        raise SyncError("timeout_seconds is invalid")
    if (
        not isinstance(document["max_artifact_bytes"], int)
        or isinstance(document["max_artifact_bytes"], bool)
        or not 1 <= document["max_artifact_bytes"] <= 8 * 1024**3
    ):
        raise SyncError("max_artifact_bytes is invalid")
    if not isinstance(document["mirror_root"], str):
        raise SyncError("mirror_root must be a bounded absolute path")
    root = Path(document["mirror_root"])
    if not root.is_absolute() or root == Path("/"):
        raise SyncError("mirror_root must be a bounded absolute path")
    if not isinstance(source["bearer_token_file"], str):
        raise SyncError("bearer_token_file must be absolute")
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


def _download(response: BinaryIO, destination: Path, expected_hash: str, expected_size: int, maximum: int) -> None:
    if expected_size > maximum:
        raise SyncError("artifact exceeds the configured size limit")
    digest = hashlib.sha256()
    size = 0
    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("xb") as output:
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


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _fsync_file(path: Path) -> None:
    with path.open("rb") as source:
        os.fsync(source.fileno())


def _version_key(name: str) -> tuple[int, int, int, str] | None:
    if not name.startswith("v"):
        return None
    base, _, suffix = name[1:].partition("-")
    parts = base.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        return None
    return int(parts[0]), int(parts[1]), int(parts[2]), suffix


def _verify_android_apk(path: Path, expected_fingerprint: str, apksigner_path: Path, timeout: int) -> None:
    try:
        result = subprocess.run(
            [apksigner_path, "verify", "--print-certs", path],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise SyncError("Android APK signature verification could not run") from error
    if result.returncode != 0:
        raise SyncError("Android APK signature verification failed")
    prefix = "Signer #1 certificate SHA-256 digest: "
    fingerprints = {
        line.removeprefix(prefix).strip().lower()
        for line in result.stdout.splitlines()
        if line.startswith(prefix)
    }
    if fingerprints != {expected_fingerprint}:
        raise SyncError("Android APK signer does not match the pinned release identity")


def _reject_stale_release(root: Path, incoming_version: str) -> None:
    current_manifest = root / "current" / "manifest.json"
    if not current_manifest.exists():
        return
    try:
        current = validate_manifest(json.loads(current_manifest.read_bytes()))
    except (OSError, json.JSONDecodeError, ManifestError) as error:
        raise SyncError("the active release metadata is invalid") from error
    current_key = _version_key(f"v{current['release']['version']}")
    incoming_key = _version_key(f"v{incoming_version}")
    if current_key is None or incoming_key is None:
        raise SyncError("stable mirror versions must use comparable SemVer")
    if incoming_key < current_key:
        raise SyncError("refusing to activate a release older than the active release")


def _ensure_latest_manifest_link(root: Path) -> None:
    manifests = root / "manifests"
    manifests.mkdir(parents=True, exist_ok=True)
    latest = manifests / "latest.json"
    expected = PurePosixPath("..", "current", "manifest.json")
    if latest.is_symlink() and Path(os.readlink(latest)) == expected:
        return
    temporary = manifests / f".latest.{uuid.uuid4().hex}"
    os.symlink(expected, temporary)
    try:
        os.replace(temporary, latest)
        _fsync_directory(manifests)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()


def _active_release_path(root: Path, releases: Path) -> Path | None:
    try:
        current = (root / "current").resolve(strict=True)
    except OSError:
        return None
    if current.parent != releases.resolve() or not current.is_dir():
        raise SyncError("the active release link is invalid")
    return current


def _activate(root: Path, version: str, staged_release: Path, manifest_bytes: bytes) -> None:
    releases = root / "releases"
    releases.mkdir(parents=True, exist_ok=True)
    previous = _active_release_path(root, releases)
    destination = releases / f"v{version}"
    if destination.exists():
        existing_manifest = destination / "manifest.json"
        if not existing_manifest.is_file() or existing_manifest.read_bytes() != manifest_bytes:
            raise SyncError("an immutable release version already exists with different contents")
        shutil.rmtree(staged_release)
    else:
        os.replace(staged_release, destination)
        _fsync_directory(releases)

    # Retention happens while the previous generation is still active. A
    # cleanup failure therefore cannot report failure after switching clients
    # to the new release, and the retained rollback is the actual old current.
    _apply_retention(root, destination, previous)
    temporary_link = root / f".current.{uuid.uuid4().hex}"
    os.symlink(PurePosixPath("releases", f"v{version}"), temporary_link)
    try:
        os.replace(temporary_link, root / "current")
        _fsync_directory(root)
    finally:
        with contextlib.suppress(FileNotFoundError):
            temporary_link.unlink()
    _ensure_latest_manifest_link(root)


def _apply_retention(root: Path, current: Path, previous: Path | None) -> None:
    releases = root / "releases"
    candidates: list[tuple[tuple[int, int, int, str], Path]] = []
    for child in releases.iterdir():
        if not child.is_dir() or child.is_symlink():
            continue
        key = _version_key(child.name)
        if key is not None and (child / "verified.json").is_file() and (child / "manifest.json").is_file():
            candidates.append((key, child))
    keep = {current}
    if previous is not None and previous != current:
        keep.add(previous)
    elif previous == current:
        # An idempotent sync of the active version must not discard its
        # rollback generation. Under the retention invariant, the actual
        # predecessor is the greatest verified version below current; a
        # dormant newer generation from an earlier failed activation is never
        # promoted to rollback status.
        current_key = _version_key(current.name)
        rollback = max(
            (
                (key, path)
                for key, path in candidates
                if path != current and current_key is not None and key < current_key
            ),
            default=None,
        )
        if rollback is not None:
            keep.add(rollback[1])
    for _, path in candidates:
        if path not in keep:
            shutil.rmtree(path)
    _fsync_directory(releases)


def sync_release(
    config: dict[str, Any],
    source: Source,
    apk_verifier: Callable[[Path, str, Path, int], None] = _verify_android_apk,
) -> str:
    root = Path(config["mirror_root"])
    root.mkdir(parents=True, exist_ok=True)
    for directory in (root / ".staging", root / "releases", root / "manifests"):
        directory.mkdir(mode=0o750, parents=True, exist_ok=True)

    with source.open_manifest() as response:
        manifest_bytes = _read_limited(response, MAX_MANIFEST_BYTES)
    try:
        manifest = validate_manifest(json.loads(manifest_bytes))
    except (json.JSONDecodeError, ManifestError) as error:
        raise SyncError(f"release manifest rejected: {error}") from error
    release = manifest["release"]
    if release["channel"] != config["channel"]:
        raise SyncError("release channel does not match the configured channel")
    version = release["version"]
    _reject_stale_release(root, version)
    android = next(entry for entry in manifest["artifacts"] if entry["platform"] == "android")
    if android["signature"]["value"] != config["android_signing_certificate_sha256"]:
        raise SyncError("Android signing certificate does not match the pinned release identity")

    staging = root / ".staging" / uuid.uuid4().hex
    staged_release = staging / f"v{version}"
    staged_release.mkdir(parents=True)
    try:
        android_path: Path | None = None
        for entry in mirrored_artifacts(manifest):
            artifact = entry["artifact"]
            filename = PurePosixPath(artifact["path"]).name
            relative = Path(f"{entry['platform']}-{entry['architecture']}") / filename
            destination = staged_release / relative
            with contextlib.closing(source.open_artifact(version, artifact["path"])) as response:
                _download(response, destination, artifact["sha256"], artifact["size"], config["max_artifact_bytes"])
            if entry["platform"] == "android":
                android_path = destination
        if android_path is None:
            raise SyncError("release manifest does not contain an Android APK")
        apk_verifier(
            android_path,
            config["android_signing_certificate_sha256"],
            Path(config["android_apksigner_path"]),
            config["timeout_seconds"],
        )
        canonical = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n"
        manifest_path = staged_release / "manifest.json"
        manifest_path.write_bytes(canonical)
        verification = {
            "schema_version": 1,
            "version": version,
            "manifest_sha256": hashlib.sha256(canonical).hexdigest(),
        }
        verified_path = staged_release / "verified.json"
        verified_path.write_text(json.dumps(verification, sort_keys=True) + "\n", encoding="utf-8")
        _fsync_file(manifest_path)
        _fsync_file(verified_path)
        _fsync_directory(staged_release)
        _activate(root, version, staged_release, canonical)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    return version


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, default=Path("/etc/jarvis/app-updates/config.json"))
    args = parser.parse_args()
    try:
        config = _safe_config(args.config)
        source_config = config["source"]
        token = _read_token(Path(source_config["bearer_token_file"]))
        if source_config["kind"] == "github-releases":
            source: Source = GitHubReleaseSource(source_config["repository"], token, config["timeout_seconds"])
        else:
            source = HttpTemplateSource(source_config, token, config["timeout_seconds"])
        root = Path(config["mirror_root"])
        root.mkdir(parents=True, exist_ok=True)
        with (root / "sync.lock").open("a+b") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            version = sync_release(config, source)
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
