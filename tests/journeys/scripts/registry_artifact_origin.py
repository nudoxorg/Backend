"""Bound retained registry metadata and archive bytes to an acquired source tree.

This observer does not extract archives or invent registry acquisition evidence.
"""
from __future__ import annotations

import base64
import hashlib
import io
import json
import re
import tarfile
import zipfile
from email.parser import BytesParser
from pathlib import PurePosixPath
from urllib.parse import quote, urlsplit


class OriginError(ValueError):
    pass


def canonical_json(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def official_url(value, host, path=None):
    if not isinstance(value, str):
        raise OriginError("registry artifact URL is invalid")
    parsed = urlsplit(value)
    if parsed.scheme != "https" or parsed.netloc != host or parsed.query or parsed.fragment or (path is not None and parsed.path != path):
        raise OriginError("registry artifact URL is outside its official origin")


def normalized_pypi(value):
    return re.sub(r"[-_.]+", "-", value).lower() if isinstance(value, str) else None


def archive_members(archive, prefix, maximum_files, maximum_file_bytes, maximum_bytes, check=lambda: None):
    if not isinstance(prefix, str) or not prefix.endswith("/") or not prefix[:-1] or PurePosixPath(prefix[:-1]).as_posix() != prefix[:-1] or prefix.startswith("/") or "\\" in prefix or any(part in {".", ".."} for part in PurePosixPath(prefix).parts):
        raise OriginError("archive extraction prefix is not canonical")
    files, special, total, entries = [], {}, 0, 0

    def record(name, size, regular, directory, open_stream):
        nonlocal total, entries
        entries += 1
        check()
        if entries > 2 * maximum_files:
            raise OriginError("archive traversal exceeds its member bound")
        if not isinstance(name, str) or "\\" in name or name.startswith("/") or any(part in {".", ".."} for part in name.split("/") if part):
            raise OriginError("archive contains an unconfined member")
        if name.rstrip("/") == prefix.rstrip("/") and directory:
            return
        if not name.startswith(prefix):
            raise OriginError("archive contains a member outside its declared extraction root")
        relative = name[len(prefix):]
        if directory:
            return
        if not regular or not relative or PurePosixPath(relative).as_posix() != relative or relative.split("/")[0] == ".git":
            raise OriginError("archive contains a link, special, or noncanonical source member")
        if size < 0 or size > maximum_file_bytes or len(files) >= maximum_files:
            raise OriginError("archive file exceeds its size or member bound")
        total += size
        if total > maximum_bytes:
            raise OriginError("archive exceeds its uncompressed byte bound")
        digest, captured, count = hashlib.sha256(), bytearray(), 0
        with open_stream() as stream:
            while chunk := stream.read(min(65536, maximum_file_bytes + 1 - count)):
                check()
                count += len(chunk)
                if count > size or count > maximum_file_bytes:
                    raise OriginError("archive member exceeds its declared size")
                digest.update(chunk)
                if relative in {"package.json", "PKG-INFO", "go.mod"}:
                    if len(captured) + len(chunk) > 1024 * 1024:
                        raise OriginError("archive identity metadata exceeds its byte bound")
                    captured.extend(chunk)
        if count != size:
            raise OriginError("archive member ended before its declared size")
        files.append({"path": relative, "bytes": size, "sha256": digest.hexdigest()})
        if relative in {"package.json", "PKG-INFO", "go.mod"}:
            special[relative] = bytes(captured)

    try:
        if zipfile.is_zipfile(io.BytesIO(archive)):
            with zipfile.ZipFile(io.BytesIO(archive)) as source:
                for entry in source.infolist():
                    mode = entry.external_attr >> 16
                    regular = not entry.is_dir() and (mode & 0o170000) in {0, 0o100000}
                    record(entry.filename, entry.file_size, regular, entry.is_dir(), lambda e=entry: source.open(e))
        else:
            with tarfile.open(fileobj=io.BytesIO(archive), mode="r|*") as source:
                for entry in source:
                    record(entry.name, entry.size, entry.isfile(), entry.isdir(), lambda e=entry: source.extractfile(e))
    except (tarfile.TarError, zipfile.BadZipFile, OSError, EOFError) as error:
        raise OriginError("registry archive could not be safely read") from error
    files.sort(key=lambda row: row["path"])
    if not files or len({row["path"] for row in files}) != len(files):
        raise OriginError("registry archive has empty or duplicate file membership")
    return files, special


def verify_registry_metadata(package, metadata, metadata_url, archive, archive_url, special):
    ecosystem, name, version = (package[key] for key in ("ecosystem", "id", "version"))
    if ecosystem == "pypi":
        official_url(metadata_url, "pypi.org", f"/pypi/{name}/json")
        official_url(archive_url, "files.pythonhosted.org")
        if normalized_pypi(metadata.get("info", {}).get("name")) != name:
            raise OriginError("PyPI metadata belongs to another project")
        releases = metadata.get("releases", {}).get(version)
        matches = [row for row in releases or [] if isinstance(row, dict)
                   and row.get("packagetype") == "sdist" and row.get("url") == archive_url]
        if len(matches) != 1 or matches[0].get("size") != len(archive) or matches[0].get("digests", {}).get("sha256") != hashlib.sha256(archive).hexdigest():
            raise OriginError("PyPI release does not bind the exact retained archive")
        info = BytesParser().parsebytes(special.get("PKG-INFO", b""))
        if normalized_pypi(info.get("Name")) != name or info.get("Version") != version:
            raise OriginError("sdist package metadata belongs to another project or version")
    elif ecosystem == "npm":
        official_url(metadata_url, "registry.npmjs.org", "/" + quote(name, safe="@/"))
        official_url(archive_url, "registry.npmjs.org")
        release = metadata.get("versions", {}).get(version)
        if metadata.get("name") != name or not isinstance(release, dict) or release.get("name") != name or release.get("version") != version:
            raise OriginError("NPM metadata belongs to another package or version")
        dist = release.get("dist", {})
        if dist.get("tarball") != archive_url:
            raise OriginError("NPM release does not bind the retained archive URL")
        integrity = dist.get("integrity")
        if integrity:
            candidates = []
            for token in integrity.split():
                algorithm, separator, encoded = token.partition("-")
                if separator and algorithm in {"sha512", "sha384", "sha256"}:
                    try:
                        candidates.append(base64.b64decode(encoded, validate=True) == hashlib.new(algorithm, archive).digest())
                    except ValueError as error:
                        raise OriginError("NPM integrity value is malformed") from error
            if not candidates or not all(candidates):
                raise OriginError("NPM release integrity differs from the retained archive")
        elif dist.get("shasum") != hashlib.sha1(archive).hexdigest():
            raise OriginError("NPM release shasum differs from the retained archive")
        try:
            info = json.loads(special.get("package.json", b""))
        except (ValueError, UnicodeDecodeError) as error:
            raise OriginError("NPM archive has no valid package identity metadata") from error
        if info.get("name") != name or info.get("version") != version:
            raise OriginError("NPM archive belongs to another package or version")
    else:
        raise OriginError("Go origin requires its separate authenticated module acquisition binding")
