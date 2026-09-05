#!/usr/bin/env python3
"""Reject deployment identifiers without storing their plaintext in the guard.

Fingerprints represent owner-supplied denylist entries, not signing material.
No documentation exception is configured: currently no file needs one.
Scans tracked and non-ignored untracked files; --history audits reachable blobs
and commit/tag messages without changing Git history. Output redacts matches.
"""
import argparse
import hashlib
from pathlib import Path
import re
import subprocess
import sys

FORBIDDEN = {
    "749ba9d5addaa5cb5f24d57b447a190278543c0afcd1a5c7c62b7b45cce57ee7": "owner-public-ip",
    "bb466c99efaf56afc5efea8b5d3d024358b51c615d2d30dcf9cca5cba1109984": "owner-lan-ip",
    "5e143390a6ee09da11f823d60622b526f844df412306b3a351e5b50e55df1d69": "owner-hostname",
    "77f7011d702742405713dace324113f10efa14794320ae520d29ab252029ed96": "owner-domain",
}
TOKENS = re.compile(rb"[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)+")


def matches(data, fingerprints=FORBIDDEN):
    for match in TOKENS.finditer(data):
        token = match.group().lower()
        labels = set()
        parts = token.split(b".")
        for index in range(len(parts) - 1):
            value = b".".join(parts[index:])
            label = fingerprints.get(hashlib.sha256(value).hexdigest())
            if label:
                labels.add(label)
        if labels:
            yield data.count(b"\n", 0, match.start()) + 1, ",".join(sorted(labels))


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def audit(root, history=False, extra=()):
    violations = 0
    paths = set(git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")) - {b""}
    for name in sorted(paths):
        path = root / name.decode("utf-8")
        if path.is_symlink() or not path.is_file():
            continue
        for line, label in matches(path.read_bytes()):
            print(f"WORKTREE {name.decode()}:{line}: {label}")
            violations += 1
    for path in extra:
        if not path.is_file() or path.is_symlink():
            raise ValueError("extra artifact must be a regular file")
        for line, label in matches(path.read_bytes()):
            print(f"ARTIFACT {path.name}:{line}: {label}")
            violations += 1
    objects = {}
    if history:
        for line in git(root, "rev-list", "--objects", "--all", "--reflog").splitlines():
            oid, _, path = line.partition(b" ")
            objects[oid] = path.decode("utf-8", errors="replace")
        refs = git(root, "for-each-ref", "--format=%(objectname)").splitlines()
        for oid in refs:
            objects.setdefault(oid, "ref-object")
        process = subprocess.Popen(["git", "-C", str(root), "cat-file", "--batch"],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            for oid, path in objects.items():
                process.stdin.write(oid + b"\n")
                process.stdin.flush()
                header = process.stdout.readline().split()
                if len(header) != 3:
                    raise ValueError("cannot read Git object")
                data = process.stdout.read(int(header[2]))
                process.stdout.read(1)
                if header[1] not in (b"blob", b"commit", b"tag"):
                    continue
                for line, label in matches(data):
                    print(f"HISTORY {oid.decode()} {path}:{line}: {label}")
                    violations += 1
        finally:
            process.stdin.close()
            process.wait()
    print(f"Privacy scan: {len(paths)} source paths, {len(objects)} history objects, {violations} violations")
    return violations


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--history", action="store_true")
    parser.add_argument("--artifact", action="append", type=Path, default=[])
    args = parser.parse_args()
    sys.exit(bool(audit(args.root.resolve(), args.history, args.artifact)))
