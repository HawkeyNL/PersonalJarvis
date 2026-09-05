"""Read four regular USTAR members without extraction or unbounded metadata."""
import io
import json
import os
from pathlib import Path, PurePosixPath
import stat
import tarfile

from manifest import MAX_MANIFEST_BYTES, validate_manifest


class _Member(io.IOBase):
    def __init__(self, archive, offset, size):
        self._archive = archive
        self._offset = offset
        self._remaining = size

    def read(self, size=-1):
        if size < 0:
            raise ValueError("mobile archive reads must be explicitly bounded")
        self._archive.seek(self._offset)
        data = self._archive.read(min(size, self._remaining))
        self._offset += len(data)
        self._remaining -= len(data)
        return data


class MobileArchiveSource:
    def __init__(self, path: Path):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        self._archive = os.fdopen(fd, "rb")
        try:
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode) or not 1024 <= metadata.st_size <= 4 * 1024**3 + 32768:
                raise ValueError("mobile archive must be a bounded regular file")
            self._members = {}
            position = 0
            while True:
                self._archive.seek(position)
                header = self._archive.read(512)
                if header == bytes(512):
                    tail = self._archive.read(32769)
                    if len(tail) > 32768 or any(tail):
                        raise ValueError("invalid mobile archive trailer")
                    break
                member = tarfile.TarInfo.frombuf(header, "utf-8", "strict")
                if (not member.isreg() or member.name != PurePosixPath(member.name).name
                        or member.name in self._members or len(self._members) >= 4
                        or not 0 < member.size <= 2 * 1024**3):
                    raise ValueError("mobile archive has unsafe or duplicate members")
                if position + 512 + member.size > metadata.st_size:
                    raise ValueError("truncated mobile archive")
                self._members[member.name] = (position + 512, member.size)
                position += 512 + ((member.size + 511) // 512) * 512
            offset, size = self._members.get("latest.json", (0, 0))
            if not 0 < size <= MAX_MANIFEST_BYTES:
                raise ValueError("mobile archive manifest missing or oversized")
            self._archive.seek(offset)
            manifest = validate_manifest(json.loads(self._archive.read(size)))
            if manifest["release"].get("product") != "mobile":
                raise ValueError("owner import accepts only mobile releases")
            self.version = manifest["release"]["version"]
            self._apk = f"Jarvis_{self.version}_android_universal.apk"
            base = self._apk[:-4]
            if set(self._members) != {"latest.json", self._apk, base + ".aab", self._apk + ".cert-sha256"}:
                raise ValueError("mobile archive artifact set is incomplete or unexpected")
        except tarfile.TarError as error:
            self.close()
            raise ValueError("invalid mobile archive header") from error
        except BaseException:
            self.close()
            raise

    def open_manifest(self):
        return _Member(self._archive, *self._members["latest.json"])

    def open_artifact(self, version, path):
        expected = f"releases/v{self.version}/android-universal/{self._apk}"
        if version != self.version or path != expected:
            raise ValueError("mobile artifact is not part of this handoff")
        return _Member(self._archive, *self._members[self._apk])

    def close(self):
        self._archive.close()
