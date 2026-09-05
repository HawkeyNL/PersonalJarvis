"""Verify Tauri base64 Minisign documents using an externally pinned public key."""
import base64
import os
from pathlib import Path
import subprocess
import sys
import tempfile

MINISIGN = "/usr/bin/minisign"


def verify(artifact: Path, signature: str, public_key: str) -> None:
    if artifact.is_symlink() or not artifact.is_file() or len(signature) > 16384 or len(public_key) > 16384:
        raise ValueError("unsafe signature input")
    with tempfile.TemporaryDirectory(prefix="jarvis-signature-") as directory:
        key = Path(directory) / "key.pub"
        sig = Path(directory) / "artifact.minisig"
        key.write_bytes(base64.b64decode(public_key.strip(), validate=True))
        sig.write_bytes(base64.b64decode(signature.strip(), validate=True))
        result = subprocess.run([MINISIGN, "-Vm", str(artifact.resolve()), "-p", str(key), "-x", str(sig)],
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                timeout=120, check=False)
        if result.returncode:
            raise ValueError("Tauri updater signature verification failed")


def main() -> None:
    path = Path(sys.argv[1])
    signatures = list(path.glob("*.sig")) if path.is_dir() else [Path(str(path) + ".sig")]
    if not signatures:
        raise ValueError("no signatures to verify")
    for signature in signatures:
        if signature.is_symlink() or signature.stat().st_size > 16384:
            raise ValueError("unsafe signature document")
        verify(Path(str(signature)[:-4]), signature.read_text(), os.environ["JARVIS_TAURI_UPDATER_PUBKEY"])


if __name__ == "__main__":
    main()
