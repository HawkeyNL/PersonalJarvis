#!/usr/bin/env python3
"""PTY smoke test for the exact staged Jarvis admin release binary.

This test performs only the read-only `status` operation. Run the script as
root (for example with `sudo -n` in CI); it never installs the binary or
changes production services.
"""

from __future__ import annotations

import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path


ALT_SCREEN_ENTER = b"\x1b[?1049h"
ALT_SCREEN_LEAVE = b"\x1b[?1049l"
CURSOR_SHOW = b"\x1b[?25h"
PREVIEW_SCENARIOS = (
    "healthy-status",
    "degraded-status",
    "models",
    "credentials",
    "agents",
    "update-running",
    "update-success",
    "update-failure-rollback",
    "logs",
    "narrow-long",
)
EXIT_KEYS = (("q", b"q"), ("Escape", b"\x1b"), ("Ctrl-C", b"\x03"))


def set_size(fd: int, rows: int, columns: int) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def read_available(fd: int, timeout: float) -> bytes:
    chunks: list[bytes] = []
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([fd], [], [], min(0.05, deadline - time.monotonic()))
        if not ready:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        chunks.append(chunk)
    return b"".join(chunks)


def terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=2)


def pty_status_smoke(
    binary: Path,
    minimum_lifetime: float,
    fixture_preview: bool,
    preview_scenario: str,
    exit_name: str,
    exit_bytes: bytes,
) -> None:
    master, slave = pty.openpty()
    set_size(master, 24, 80)
    before = termios.tcgetattr(master)
    command = (
        [str(binary), "--tui-trace", "tui-preview", preview_scenario]
        if fixture_preview
        else [str(binary), "status"]
    )
    process = subprocess.Popen(
        command,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
        start_new_session=True,
        env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "TERM": "xterm-256color"},
    )
    os.close(slave)
    output = bytearray()
    useful_frame_marker = b"Jarvis" if fixture_preview else b"Jarvis Home Node"
    try:
        deadline = time.monotonic() + 8
        while useful_frame_marker not in output and time.monotonic() < deadline:
            output.extend(read_available(master, 0.1))
            if process.poll() is not None:
                raise AssertionError(
                    f"TUI exited before its first useful frame: code={process.returncode}; "
                    f"output={bytes(output)!r}"
                )
        if useful_frame_marker not in output:
            raise AssertionError(f"TUI did not render its dashboard: output={bytes(output)!r}")
        if ALT_SCREEN_ENTER not in output:
            raise AssertionError("TUI did not enter the alternate screen")

        alive_until = time.monotonic() + minimum_lifetime
        while time.monotonic() < alive_until:
            output.extend(read_available(master, 0.05))
            if process.poll() is not None:
                raise AssertionError(
                    f"persistent TUI exited without an exit key: code={process.returncode}"
                )

        set_size(master, 30, 100)
        time.sleep(0.15)
        if process.poll() is not None:
            raise AssertionError("TUI exited while handling terminal resize")

        os.write(master, exit_bytes)
        process.wait(timeout=5)
        output.extend(read_available(master, 0.5))
        if process.returncode != 0:
            raise AssertionError(
                f"TUI failed after documented {exit_name} exit: code={process.returncode}; "
                f"output={bytes(output)!r}"
            )
        after = termios.tcgetattr(master)
        restored_flags = termios.ICANON | termios.ECHO
        if before[3] & restored_flags != after[3] & restored_flags:
            raise AssertionError("TUI did not restore canonical input and terminal echo")
        if ALT_SCREEN_LEAVE not in output:
            raise AssertionError("TUI did not leave the alternate screen")
        if CURSOR_SHOW not in output:
            raise AssertionError("TUI did not restore the cursor")
    finally:
        terminate_process_group(process)
        os.close(master)


def json_never_uses_tui(binary: Path, include_status: bool) -> None:
    commands = [["terminal-diagnostics"]]
    if include_status:
        commands.append(["status"])
    for command in commands:
        result = subprocess.run(
            [str(binary), "--json", *command],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=10,
            env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "TERM": "xterm-256color"},
        )
        combined = result.stdout + result.stderr
        label = " ".join(command)
        if result.returncode != 0:
            raise AssertionError(
                f"JSON {label} failed: code={result.returncode}; output={combined!r}"
            )
        if ALT_SCREEN_ENTER in combined or ALT_SCREEN_LEAVE in combined:
            raise AssertionError(
                f"JSON {label} emitted alternate-screen control sequences"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--minimum-lifetime", type=float, default=0.75)
    parser.add_argument(
        "--fixture-preview",
        action="store_true",
        help="test a tui-preview feature build without root or system reads",
    )
    parser.add_argument(
        "--preview-scenario",
        choices=PREVIEW_SCENARIOS,
        default="healthy-status",
        help="fixture scenario used with --fixture-preview",
    )
    args = parser.parse_args()

    if os.geteuid() != 0 and not args.fixture_preview:
        parser.error("run as root; the smoke test executes only read-only `jarvis status`")
    binary = args.binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"not an executable regular file: {binary}")

    for exit_name, exit_bytes in EXIT_KEYS:
        pty_status_smoke(
            binary,
            args.minimum_lifetime,
            args.fixture_preview,
            args.preview_scenario,
            exit_name,
            exit_bytes,
        )
    json_never_uses_tui(binary, include_status=not args.fixture_preview)
    print(f"Jarvis PTY smoke passed for exact binary: {binary}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, OSError, subprocess.SubprocessError) as error:
        print(f"Jarvis PTY smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1)
