"""Pinned local-only Laya service entry point; no model download at startup.

Provision the pinned snapshot and reviewed Python wheelhouse explicitly first.
This file is release-owned and never reads secrets or owner prompts.
"""

import os
import socket
from pathlib import Path

from laya.router import Router
from laya.serve import create_app
import uvicorn


MODEL_REVISION = "55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851"
ROOT = Path("/var/lib/jarvis-laya/models") / MODEL_REVISION


def _threads() -> int:
    raw = os.environ.get("LAYA_THREADS", "2")
    value = int(raw)
    if not 1 <= value <= 64:
        raise ValueError("LAYA_THREADS must be between 1 and 64")
    return value


def main() -> None:
    # Enforce offline behavior regardless of the optional administrator config.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
    os.environ.pop("HF_TOKEN", None)
    os.environ.pop("LAYA_API_KEY", None)
    # systemd owns /run/jarvis-laya.sock as root:jarvis 0660. Verify the
    # inherited listener before loading expensive model weights.
    if os.environ.get("LISTEN_PID") != str(os.getpid()) or os.environ.get("LISTEN_FDS") != "1":
        raise SystemExit("Laya requires one systemd-owned Unix listening socket")
    with socket.socket(fileno=os.dup(3)) as inherited:
        if inherited.family != socket.AF_UNIX or inherited.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN) != 1 or inherited.getsockname() != "/run/jarvis-laya.sock":
            raise SystemExit("Laya received an unexpected listening socket")
    import torch

    torch.set_num_threads(_threads())
    english = ROOT
    multilingual = ROOT / "multilingual"
    for folder in (english, multilingual):
        if not folder.is_dir() or not (folder / "model.safetensors").is_file():
            raise SystemExit("Laya model snapshot is absent or incomplete")
    router = Router(
        models={"english": str(english), "multilingual": str(multilingual)},
        device="cpu",
        default="multilingual",
        max_loaded=2,
        auto_task_detection=False,
    )
    router.preload(["english", "multilingual"])
    app = create_app(router=router)
    uvicorn.run(app, fd=3, workers=1, access_log=False)


if __name__ == "__main__":
    main()
