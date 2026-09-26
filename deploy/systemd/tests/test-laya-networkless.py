#!/usr/bin/env python3
"""Exercise the provisioner's actual unshare/privilege-drop helper without pip."""

import os
import pathlib
import shutil
import socket
import subprocess
import tempfile


PROVISIONER = pathlib.Path(__file__).resolve().parents[1] / "provision-laya.sh"


def listener(family, address):
    server = socket.socket(family, socket.SOCK_STREAM)
    server.bind(address)
    server.listen(1)
    return server


def main():
    if os.geteuid() != 0 or os.environ.get("GITHUB_ACTIONS") != "true":
        raise SystemExit("run only as root in isolated CI")
    with tempfile.TemporaryDirectory(prefix="jarvis-laya-networkless-", dir="/tmp") as temp:
        fixture = pathlib.Path(temp)
        fixture.chmod(0o755)
        candidate = fixture / "candidate"
        candidate.mkdir(mode=0o700)
        shutil.chown(candidate, user="nobody", group="nogroup")
        (fixture / "input").write_text("reviewed fixture bytes\n", encoding="ascii")
        (fixture / "input").chmod(0o644)
        (fixture / "host-netns").write_text(os.readlink("/proc/self/ns/net"), encoding="ascii")
        (fixture / "host-netns").chmod(0o644)
        (fixture / "probe.py").write_text(
            """import os, pathlib, socket, sys
root = pathlib.Path(sys.argv[1])
assert os.geteuid() != 0
assert os.readlink('/proc/self/ns/net') != (root / 'host-netns').read_text()
assert (root / 'input').read_text() == 'reviewed fixture bytes\\n'
(root / 'candidate' / 'write-proof').write_text('writable\\n')
assert set(os.environ) == {'PATH', 'HOME', 'PIP_NO_CACHE_DIR', 'PYTHONDONTWRITEBYTECODE'}
for family, host, port in ((socket.AF_INET, '127.0.0.1', int(sys.argv[2])),
                           (socket.AF_INET6, '::1', int(sys.argv[3]))):
    try:
        with socket.socket(family, socket.SOCK_STREAM) as client:
            client.settimeout(1)
            client.connect((host, port))
    except OSError:
        pass
    else:
        raise AssertionError('host-network listener was reachable')
print('IPv4 and IPv6 host listeners isolated; unprivileged input/read and venv/write succeeded')
""",
            encoding="ascii",
        )
        (fixture / "probe.py").chmod(0o644)
        with listener(socket.AF_INET, ("127.0.0.1", 0)) as ipv4, listener(
            socket.AF_INET6, ("::1", 0)
        ) as ipv6:
            port4 = ipv4.getsockname()[1]
            port6 = ipv6.getsockname()[1]
            for family, host, port in (
                (socket.AF_INET, "127.0.0.1", port4),
                (socket.AF_INET6, "::1", port6),
            ):
                with socket.socket(family, socket.SOCK_STREAM) as client:
                    client.settimeout(1)
                    client.connect((host, port))
            command = ["bash", str(PROVISIONER), "--fixture-networkless", temp, str(port4), str(port6)]
            result = subprocess.run(command, text=True, capture_output=True, timeout=20, check=False)
            if result.returncode:
                raise AssertionError(f"networkless child failed closed: {result.stderr.strip()}")
            assert (candidate / "write-proof").read_text() == "writable\n"
            print(result.stdout.strip())
            (candidate / "write-proof").unlink()
            environment = dict(os.environ, JARVIS_LAYA_TEST_ISOLATION_FAIL="true")
            result = subprocess.run(
                command, env=environment, text=True, capture_output=True, timeout=20, check=False
            )
            assert result.returncode != 0
            assert not (candidate / "write-proof").exists()
            print("isolation setup failure aborted before child execution")


if __name__ == "__main__":
    main()
