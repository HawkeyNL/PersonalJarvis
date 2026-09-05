"""Strict Jarvis application release manifest validation.

The manifest is storage-neutral. Artifact paths are relative object keys; they
must never contain a host name, credentials, or local filesystem paths.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import PurePosixPath
import re
from typing import Any


SCHEMA_VERSION = 1
MAX_MANIFEST_BYTES = 1024 * 1024
SEMVER_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")

PLATFORMS = {
    ("windows", "x86_64"): ("home-node-updater", "tauri-minisign", True),
    ("macos", "arm64"): ("home-node-updater", "tauri-minisign", True),
    ("linux", "x86_64"): ("home-node-updater", "tauri-minisign", True),
    ("android", "universal"): ("home-node-apk", "android-apk-signing-certificate-sha256", True),
    ("ios", "arm64"): ("testflight", "apple-code-signing", False),
}


class ManifestError(ValueError):
    """The release manifest is malformed or violates the release policy."""


def _exact_keys(value: dict[str, Any], required: set[str], optional: set[str], label: str) -> None:
    missing = required - value.keys()
    unexpected = value.keys() - required - optional
    if missing:
        raise ManifestError(f"{label} is missing: {', '.join(sorted(missing))}")
    if unexpected:
        raise ManifestError(f"{label} contains unexpected fields: {', '.join(sorted(unexpected))}")


def _string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise ManifestError(f"{label} is invalid")
    return value


def _artifact_path(value: Any, version: str, platform: str, architecture: str) -> str:
    path = _string(value, "artifact.path", 512)
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or "\\" in path or any(part in ("", ".", "..") for part in parsed.parts):
        raise ManifestError("artifact.path is unsafe")
    expected_parent = PurePosixPath("releases", f"v{version}", f"{platform}-{architecture}")
    if parsed.parent != expected_parent:
        raise ManifestError("artifact.path does not match its release platform")
    expected_prefix = f"Jarvis_{version}_{platform}_{architecture}"
    if not parsed.name.startswith(expected_prefix):
        raise ManifestError("artifact filename is not canonical")
    return path


def validate_manifest(document: Any, *, require_complete: bool = True) -> dict[str, Any]:
    if not isinstance(document, dict):
        raise ManifestError("manifest must be an object")
    _exact_keys(document, {"schema_version", "release", "artifacts"}, {"installers"}, "manifest")
    if document["schema_version"] != SCHEMA_VERSION:
        raise ManifestError("unsupported schema_version")

    release = document["release"]
    if not isinstance(release, dict):
        raise ManifestError("release must be an object")
    _exact_keys(
        release,
        {"version", "channel", "released_at", "minimum_client_protocol"},
        {"notes", "product", "tag", "source_revision", "client_protocol"},
        "release",
    )
    version = _string(release["version"], "release.version", 64)
    if not SEMVER_RE.fullmatch(version):
        raise ManifestError("release.version is not SemVer")
    product = release.get("product")
    if product not in (None, "desktop", "mobile"):
        raise ManifestError("release.product is invalid")
    if product is None and any(field in release for field in ("tag", "source_revision", "client_protocol")):
        raise ManifestError("release provenance requires an explicit product")
    if product is not None:
        if release.get("tag") != f"app-v{version}":
            raise ManifestError("release.tag does not match its version")
        if not isinstance(release.get("source_revision"), str) or not re.fullmatch(r"[0-9a-f]{40}", release["source_revision"]):
            raise ManifestError("release.source_revision must be an exact SHA")
        if type(release.get("client_protocol")) is not int or not 1 <= release["client_protocol"] <= 65535:
            raise ManifestError("release.client_protocol is invalid")
    if release["channel"] not in ("stable", "beta", "development"):
        raise ManifestError("release.channel is invalid")
    if release["channel"] == "stable" and ("-" in version or "+" in version):
        raise ManifestError("stable release.version must be plain SemVer")
    released_at = _string(release["released_at"], "release.released_at", 64)
    try:
        parsed_time = datetime.fromisoformat(released_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise ManifestError("release.released_at is not RFC 3339") from error
    if parsed_time.tzinfo is None:
        raise ManifestError("release.released_at must include a timezone")
    protocol = release["minimum_client_protocol"]
    if not isinstance(protocol, int) or isinstance(protocol, bool) or not 1 <= protocol <= 65535:
        raise ManifestError("release.minimum_client_protocol is invalid")
    if product is not None and protocol > release["client_protocol"]:
        raise ManifestError("release cannot require a protocol newer than its own client")
    if "notes" in release:
        _string(release["notes"], "release.notes", 8192)

    artifacts = document["artifacts"]
    if not isinstance(artifacts, list) or not artifacts:
        raise ManifestError("artifacts must be a non-empty array")
    seen: set[tuple[str, str]] = set()
    for index, entry in enumerate(artifacts):
        label = f"artifacts[{index}]"
        if not isinstance(entry, dict):
            raise ManifestError(f"{label} must be an object")
        _exact_keys(
            entry,
            {"platform", "architecture", "distribution", "signature"},
            {"artifact", "external", "metadata"},
            label,
        )
        platform = _string(entry["platform"], f"{label}.platform", 32)
        architecture = _string(entry["architecture"], f"{label}.architecture", 32)
        key = (platform, architecture)
        policy = PLATFORMS.get(key)
        if policy is None:
            raise ManifestError(f"{label} has an unsupported platform target")
        if key in seen:
            raise ManifestError(f"{label} duplicates a platform target")
        seen.add(key)
        expected_distribution, expected_signature, mirrored = policy
        if entry["distribution"] != expected_distribution:
            raise ManifestError(f"{label}.distribution is invalid")

        signature = entry["signature"]
        if not isinstance(signature, dict):
            raise ManifestError(f"{label}.signature must be an object")
        _exact_keys(signature, {"scheme", "value"}, set(), f"{label}.signature")
        if signature["scheme"] != expected_signature:
            raise ManifestError(f"{label}.signature.scheme is invalid")
        signature_value = _string(signature["value"], f"{label}.signature.value", 16384)
        if expected_signature.endswith("sha256") and not SHA256_RE.fullmatch(signature_value):
            raise ManifestError(f"{label}.signature.value is not a SHA-256 digest")

        if mirrored:
            if "external" in entry or "artifact" not in entry:
                raise ManifestError(f"{label} requires exactly one mirrored artifact")
            artifact = entry["artifact"]
            if not isinstance(artifact, dict):
                raise ManifestError(f"{label}.artifact must be an object")
            _exact_keys(artifact, {"path", "sha256", "size"}, set(), f"{label}.artifact")
            _artifact_path(artifact["path"], version, platform, architecture)
            if not isinstance(artifact["sha256"], str) or not SHA256_RE.fullmatch(artifact["sha256"]):
                raise ManifestError(f"{label}.artifact.sha256 is invalid")
            if not isinstance(artifact["size"], int) or isinstance(artifact["size"], bool) or artifact["size"] <= 0:
                raise ManifestError(f"{label}.artifact.size is invalid")
            if platform == "android":
                metadata = entry.get("metadata")
                if not isinstance(metadata, dict):
                    raise ManifestError(f"{label}.metadata is required")
                _exact_keys(metadata, {"version_code"}, set(), f"{label}.metadata")
                version_code = metadata["version_code"]
                if (
                    not isinstance(version_code, int)
                    or isinstance(version_code, bool)
                    or not 1 <= version_code <= 2_100_000_000
                ):
                    raise ManifestError(f"{label}.metadata.version_code is invalid")
            elif "metadata" in entry:
                raise ManifestError(f"{label}.metadata is not allowed")
        else:
            if "metadata" in entry:
                raise ManifestError(f"{label}.metadata is not allowed")
            if "artifact" in entry or "external" not in entry:
                raise ManifestError(f"{label} must use external distribution")
            external = entry["external"]
            if not isinstance(external, dict):
                raise ManifestError(f"{label}.external must be an object")
            _exact_keys(external, {"bundle_id", "build_number"}, set(), f"{label}.external")
            bundle_id = _string(external["bundle_id"], f"{label}.external.bundle_id", 255)
            if bundle_id != "com.hawkeynl.jarvis":
                raise ManifestError(f"{label}.external.bundle_id is invalid")
            build_number = _string(external["build_number"], f"{label}.external.build_number", 64)
            if not build_number.isdigit() or int(build_number) < 1:
                raise ManifestError(f"{label}.external.build_number is invalid")

    required = set(PLATFORMS)
    if product == "desktop":
        required = {key for key in required if key[0] not in ("android", "ios")}
    elif product == "mobile":
        required = {key for key in required if key[0] in ("android", "ios")}
    if require_complete and seen != required:
        missing = required - seen
        rendered = ", ".join(f"{platform}-{architecture}" for platform, architecture in sorted(missing))
        raise ManifestError(f"manifest is missing release targets: {rendered}")
    installers = document.get("installers", [])
    if not isinstance(installers, list) or len(installers) > 1:
        raise ManifestError("installers must be a bounded list")
    for entry in installers:
        if product != "desktop" or not isinstance(entry, dict):
            raise ManifestError("supplemental installers are desktop-only")
        _exact_keys(entry, {"platform", "architecture", "distribution", "artifact"}, set(), "installer")
        if (entry["platform"], entry["architecture"], entry["distribution"]) != ("macos", "arm64", "home-node-installer"):
            raise ManifestError("unsupported installer target")
        artifact = entry["artifact"]
        if not isinstance(artifact, dict):
            raise ManifestError("installer artifact is invalid")
        _exact_keys(artifact, {"path", "sha256", "size"}, set(), "installer artifact")
        expected = f"releases/v{version}/macos-arm64/Jarvis_{version}_macos_arm64.dmg"
        if artifact["path"] != expected or not isinstance(artifact["sha256"], str) or not SHA256_RE.fullmatch(artifact["sha256"]):
            raise ManifestError("installer identity is invalid")
        if type(artifact["size"]) is not int or not 0 < artifact["size"] <= 2 * 1024**3:
            raise ManifestError("installer size is invalid")
    return document


def mirrored_artifacts(document: dict[str, Any]) -> list[dict[str, Any]]:
    """Return only artifacts that belong in the Home Node mirror."""

    validate_manifest(document)
    return [entry for entry in document["artifacts"] if "artifact" in entry] + document.get("installers", [])
