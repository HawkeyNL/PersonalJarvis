#!/usr/bin/env python3
"""Pull and atomically activate a verified Jarvis application release."""

from __future__ import annotations

import argparse
import base64
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
from signatures import verify as verify_signature  # noqa: E402
from mobile_import import MobileArchiveSource  # noqa: E402


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
    if parsed.username is not None or parsed.password is not None:
        raise SyncError("credential-bearing redirect refused")
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
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,99}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", repository):
            raise SyncError("invalid GitHub repository")
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
                "User-Agent": "jarvis-app-update-sync/1",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        if self._token:
            request.add_header("Authorization", f"Bearer {self._token}")
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

    def open_manifest_signature(self) -> BinaryIO:
        return self._asset("latest", "latest.json.sig")

    def validate_identity(self, version: str) -> None:
        if self._release("latest")["tag_name"] != f"app-v{version}":
            raise SyncError("GitHub release tag and signed manifest disagree")

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
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise SyncError("configuration must be a regular file, not a symlink")
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
    }
    optional = {"tauri_signing_public_key", "android_signing_certificate_sha256", "android_apksigner_path"}
    if set(document) - optional != expected or document["schema_version"] != 1:
        raise SyncError("configuration fields are invalid")
    source = document["source"]
    if not isinstance(source, dict) or "kind" not in source:
        raise SyncError("source configuration is invalid")
    if source["kind"] == "github-releases":
        if set(source) - {"bearer_token_file"} != {"kind", "repository"}:
            raise SyncError("GitHub source configuration is invalid")
        repository = source["repository"]
        if not isinstance(repository, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,99}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", repository):
            raise SyncError("source.repository is invalid")
        public_key = document.get("tauri_signing_public_key")
        if not isinstance(public_key, str) or not 0 < len(public_key) <= 16384:
            raise SyncError("GitHub sources require a pinned Tauri signing public key")
        try:
            base64.b64decode(public_key, validate=True)
        except ValueError as error:
            raise SyncError("Tauri signing public key is malformed") from error
    elif source["kind"] == "owner-import":
        if set(source) != {"kind"}:
            raise SyncError("owner import takes its archive only from the explicit CLI option")
        if not {"android_signing_certificate_sha256", "android_apksigner_path"} <= document.keys():
            raise SyncError("owner mobile import requires Android signature verification")
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
    android_fields = {"android_signing_certificate_sha256", "android_apksigner_path"}
    if android_fields & document.keys():
        if not android_fields <= document.keys():
            raise SyncError("Android mirroring requires both signer fingerprint and apksigner path")
        fingerprint = document["android_signing_certificate_sha256"]
        if not isinstance(fingerprint, str) or not re.fullmatch(r"[0-9a-f]{64}", fingerprint):
            raise SyncError("android_signing_certificate_sha256 is invalid")
        apksigner_value = document["android_apksigner_path"]
        if not isinstance(apksigner_value, str) or not Path(apksigner_value).is_absolute():
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
    if "bearer_token_file" in source:
        if not isinstance(source["bearer_token_file"], str) or not Path(source["bearer_token_file"]).is_absolute():
            raise SyncError("bearer_token_file must be absolute")
    return document


def _read_token(path: Path) -> str:
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode):
        raise SyncError("artifact credential must be a regular file")
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


def _reject_stale_release(root: Path, incoming_version: str, incoming_manifest: dict | None = None) -> None:
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
    if incoming_manifest is not None:
        old_product = current["release"].get("product")
        new_product = incoming_manifest["release"].get("product")
        if old_product == "clients" and new_product != "clients":
            raise SyncError("a unified client generation cannot be replaced by a partial product")
        if old_product not in (None, "desktop", "mobile", "clients") or new_product not in (
            None,
            "desktop",
            "mobile",
            "clients",
        ):
            raise SyncError("application release product is unsupported")
        if old_product not in (None, "clients", new_product) and new_product != "clients":
            raise SyncError("desktop and mobile products require separate mirror generations")
        old_android = next((entry for entry in current["artifacts"] if entry["platform"] == "android"), None)
        new_android = next((entry for entry in incoming_manifest["artifacts"] if entry["platform"] == "android"), None)
        if old_android is not None:
            if new_android is None:
                raise SyncError("desktop-only sync must use a separate mirror; refusing to remove active Android updates")
            old_code = old_android["metadata"]["version_code"]
            new_code = new_android["metadata"]["version_code"]
            if new_code < old_code or (incoming_key > current_key and new_code <= old_code):
                raise SyncError("Android versionCode must advance for a new mobile release")


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
        if destination.is_symlink() or not destination.is_dir():
            raise SyncError("immutable release destination is unsafe")
        existing_manifest = destination / "manifest.json"
        if not existing_manifest.is_file() or existing_manifest.read_bytes() != manifest_bytes:
            raise SyncError("an immutable release version already exists with different contents")
        for staged in staged_release.rglob("*"):
            if staged.is_file():
                existing = destination / staged.relative_to(staged_release)
                if existing.is_symlink() or not existing.is_file() or existing.stat().st_size != staged.stat().st_size:
                    raise SyncError("existing immutable release artifact is unsafe")
                with existing.open("rb") as old, staged.open("rb") as new:
                    if hashlib.file_digest(old, "sha256").digest() != hashlib.file_digest(new, "sha256").digest():
                        raise SyncError("existing immutable release artifact is corrupt")
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


def _prepare_mirror(root: Path) -> None:
    # Inspect the root before opening the lock or following any child path.
    # The production parent directories and this root are administrator-owned.
    for ancestor in (root, *root.parents):
        if ancestor.is_symlink():
            raise SyncError("mirror path must not contain symlinks")
    root.mkdir(mode=0o750, parents=True, exist_ok=True)
    metadata = root.stat()
    if not stat.S_ISDIR(metadata.st_mode):
        raise SyncError("mirror root must be a directory")
    if os.geteuid() == 0 and metadata.st_uid != 0:
        raise SyncError("mirror root must be root-owned")
    if metadata.st_mode & 0o022:
        raise SyncError("mirror root must not be group/world writable")
    for existing in root.rglob("*"):
        relative = existing.relative_to(root).as_posix()
        if existing.is_symlink() and relative not in ("current", "manifests/latest.json"):
            raise SyncError("unexpected symlink in mirror")
        if not existing.is_symlink() and existing.stat().st_mode & 0o022:
            raise SyncError("unsafe writable mirror entry")
        if os.geteuid() == 0 and existing.lstat().st_uid != 0:
            raise SyncError("mirror entries must be root-owned")
    if (root / "current").exists() or (root / "current").is_symlink():
        _active_release_path(root, root / "releases")
    for directory in (root / ".staging", root / "releases", root / "manifests"):
        directory.mkdir(mode=0o750, parents=True, exist_ok=True)


def sync_release(
    config: dict[str, Any],
    source: Source,
    apk_verifier: Callable[[Path, str, Path, int], None] = _verify_android_apk,
) -> str:
    root = Path(config["mirror_root"])
    _prepare_mirror(root)

    with source.open_manifest() as response:
        manifest_bytes = _read_limited(response, MAX_MANIFEST_BYTES)
    if isinstance(source, GitHubReleaseSource):
        import tempfile
        with source.open_manifest_signature() as response:
            signature = _read_limited(response, 16384).decode("ascii")
        with tempfile.TemporaryDirectory(prefix="jarvis-manifest-") as directory:
            signed = Path(directory) / "latest.json"
            signed.write_bytes(manifest_bytes)
            verify_signature(signed, signature, config["tauri_signing_public_key"])
    try:
        manifest = validate_manifest(json.loads(manifest_bytes))
    except (json.JSONDecodeError, ManifestError) as error:
        raise SyncError(f"release manifest rejected: {error}") from error
    release = manifest["release"]
    if release["channel"] != config["channel"]:
        raise SyncError("release channel does not match the configured channel")
    version = release["version"]
    if isinstance(source, GitHubReleaseSource):
        source.validate_identity(version)
    _reject_stale_release(root, version, manifest)
    android = next((entry for entry in manifest["artifacts"] if entry["platform"] == "android"), None)
    if android is not None:
        if not config.get("android_apksigner_path") or not config.get("android_signing_certificate_sha256"):
            raise SyncError("Android artifact requires explicitly configured APK verification")
        if android["signature"]["value"] != config["android_signing_certificate_sha256"]:
            raise SyncError("Android signing certificate does not match the pinned release identity")

    staging = root / ".staging" / uuid.uuid4().hex
    staged_release = staging / f"v{version}"
    staged_release.mkdir(mode=0o750, parents=True)
    try:
        android_path: Path | None = None
        for entry in mirrored_artifacts(manifest):
            artifact = entry["artifact"]
            filename = PurePosixPath(artifact["path"]).name
            relative = Path(f"{entry['platform']}-{entry['architecture']}") / filename
            destination = staged_release / relative
            with contextlib.closing(source.open_artifact(version, artifact["path"])) as response:
                _download(response, destination, artifact["sha256"], artifact["size"], config["max_artifact_bytes"])
            if entry["platform"] == "android" and entry["distribution"] == "home-node-apk":
                android_path = destination
            elif isinstance(source, GitHubReleaseSource) and entry["distribution"] == "home-node-updater":
                verify_signature(destination, entry["signature"]["value"], config["tauri_signing_public_key"])
        if android_path is not None:
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
        for entry in staging.rglob("*"):
            entry.chmod(0o750 if entry.is_dir() else 0o640)
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
    parser.add_argument("--import-mobile", type=Path, help="explicit owner-only import of a decrypted mobile handoff")
    args = parser.parse_args()
    source = None
    try:
        config = _safe_config(args.config)
        source_config = config["source"]
        token = _read_token(Path(source_config["bearer_token_file"])) if "bearer_token_file" in source_config else ""
        if args.import_mobile is not None and source_config["kind"] != "owner-import":
            raise SyncError("mobile import requires a separate owner-import mirror configuration")
        if source_config["kind"] == "owner-import":
            if args.import_mobile is None:
                raise SyncError("owner import requires --import-mobile; it cannot run unattended")
            source = MobileArchiveSource(args.import_mobile)
        elif source_config["kind"] == "github-releases":
            source = GitHubReleaseSource(source_config["repository"], token, config["timeout_seconds"])
        else:
            source = HttpTemplateSource(source_config, token, config["timeout_seconds"])
        root = Path(config["mirror_root"])
        _prepare_mirror(root)
        descriptor = os.open(root / "sync.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o640)
        with os.fdopen(descriptor, "a+b") as lock:
            metadata = os.fstat(lock.fileno())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1 or metadata.st_mode & 0o022:
                raise SyncError("mirror lock must be a private regular file")
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            version = sync_release(config, source)
        print(f"activated Jarvis application release v{version}")
    except BlockingIOError:
        print("another application update sync is already running", file=sys.stderr)
        return 3
    except (OSError, ValueError, json.JSONDecodeError, SyncError) as error:
        print(f"application update sync failed: {error}", file=sys.stderr)
        return 2
    finally:
        if isinstance(source, MobileArchiveSource):
            source.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
