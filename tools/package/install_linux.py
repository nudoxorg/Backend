#!/usr/bin/env python3
"""Install an immutable NuDox Linux x86_64 CLI/MCP/locald release."""
from __future__ import annotations

import argparse
import datetime
import fcntl
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

CATALOG_URL = "https://api.nudox.org/v1/releases"
GITHUB_REPO = "nudoxorg/Backend"
PLATFORM = "linux-x64"
TARGET = "x86_64-unknown-linux-gnu"
# Filled by linux_release_package.py for a meeting/checkpoint release. A
# populated pair bypasses the public channel catalog and pins the manifest
# bytes directly; the stable installer leaves both values empty.
PINNED_RELEASE_TAG = ""
PINNED_MANIFEST_SHA256 = ""
MAX_METADATA = 65536
MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024
MAX_MEMBER_BYTES = 1024 * 1024 * 1024
REQUIRED_BINARIES = ("backend-cli", "backend-mcp", "backend-locald")
ASSET_RE = re.compile(r"nudox-linux-x86_64-[a-f0-9]{10,40}\.tar\.gz\Z")
SEMVER_RE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+\Z")
SHA_RE = re.compile(r"[a-f0-9]{64}\Z")
SOURCE_RE = re.compile(r"[a-f0-9]{40}\Z")
CHECKPOINT_TAG_RE = re.compile(r"checkpoint-[0-9]{8}-[a-f0-9]{10}-linux-x64\Z")


class InstallError(Exception):
    pass


class HTTPSOnlyRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise InstallError("release download redirected away from HTTPS")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


HTTPS = urllib.request.build_opener(HTTPSOnlyRedirect())


def fetch_bytes(url: str, limit: int, label: str) -> bytes:
    if urllib.parse.urlsplit(url).scheme != "https":
        raise InstallError(f"refusing non-HTTPS {label} URL")
    request = urllib.request.Request(url, headers={"User-Agent": "nudox-installer/1", "Accept": "application/json, application/octet-stream"})
    try:
        with HTTPS.open(request, timeout=60) as response:
            body = response.read(limit + 1)
    except urllib.error.HTTPError as error:
        raise InstallError(f"{label} request failed with HTTP {error.code}") from error
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise InstallError(f"could not fetch {label}: {error}") from error
    if len(body) > limit:
        raise InstallError(f"{label} exceeds the {limit}-byte limit")
    return body


def validate_release_manifest(release_manifest: object) -> dict:
    manifest_fields = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_glibc", "build_manifest_sha256", "packaging_manifest_sha256"}
    if not isinstance(release_manifest, dict) or set(release_manifest) != manifest_fields or release_manifest.get("schema") != 1:
        raise InstallError("immutable Linux release manifest has an unsupported schema")
    if not isinstance(release_manifest.get("version"), str) or not SEMVER_RE.fullmatch(release_manifest["version"]):
        raise InstallError("immutable Linux release manifest has an invalid version")
    for field in ("source_sha", "source_tree"):
        if not isinstance(release_manifest.get(field), str) or not SOURCE_RE.fullmatch(release_manifest[field]):
            raise InstallError(f"immutable Linux release manifest has an invalid {field}")
    if not isinstance(release_manifest.get("cargo_lock_sha256"), str) or not SHA_RE.fullmatch(release_manifest["cargo_lock_sha256"]):
        raise InstallError("immutable Linux release manifest has an invalid Cargo.lock digest")
    if release_manifest.get("platform") != PLATFORM or release_manifest.get("target") != TARGET:
        raise InstallError("immutable Linux release manifest targets an unsupported platform")
    if not isinstance(release_manifest.get("asset"), str) or not ASSET_RE.fullmatch(release_manifest["asset"]):
        raise InstallError("immutable Linux release manifest contains an invalid archive name")
    if not isinstance(release_manifest.get("sha256"), str) or not SHA_RE.fullmatch(release_manifest["sha256"]):
        raise InstallError("immutable Linux release manifest is missing a valid archive SHA-256")
    size = release_manifest.get("size_bytes")
    if not isinstance(size, int) or isinstance(size, bool) or not 1 <= size <= MAX_ARCHIVE_BYTES:
        raise InstallError("immutable Linux release manifest has an invalid archive size")
    minimum_glibc = release_manifest.get("minimum_glibc")
    if not isinstance(minimum_glibc, str) or not re.fullmatch(r"[0-9]+\.[0-9]+", minimum_glibc):
        raise InstallError("immutable Linux release manifest has an invalid glibc minimum")
    required_hashes = ("build_manifest_sha256", "packaging_manifest_sha256")
    if any(not isinstance(release_manifest.get(key), str) or not SHA_RE.fullmatch(release_manifest[key]) for key in required_hashes):
        raise InstallError("immutable Linux release manifest omits build/package evidence hashes")
    return release_manifest


def load_release() -> tuple[dict, dict, str]:
    """Load either the digest-pinned checkpoint or the promoted channel."""
    if bool(PINNED_RELEASE_TAG) != bool(PINNED_MANIFEST_SHA256):
        raise InstallError("installer checkpoint pin is incomplete")
    if PINNED_RELEASE_TAG:
        if not CHECKPOINT_TAG_RE.fullmatch(PINNED_RELEASE_TAG) or not SHA_RE.fullmatch(PINNED_MANIFEST_SHA256):
            raise InstallError("installer checkpoint pin is invalid")
        tag = PINNED_RELEASE_TAG
        manifest_url = "https://github.com/" + GITHUB_REPO + "/releases/download/" + urllib.parse.quote(tag, safe=".-") + "/release-manifest-linux-x64.json"
        raw_manifest = fetch_bytes(manifest_url, MAX_METADATA, "immutable Linux release manifest")
        entry, release_manifest = parse_pinned_manifest(tag, raw_manifest)
    else:
        try:
            catalog = json.loads(fetch_bytes(CATALOG_URL, MAX_METADATA, "release catalog"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise InstallError(f"release catalog is not valid JSON: {error}") from error
        if not isinstance(catalog, dict) or catalog.get("schema") != 1 or not isinstance(catalog.get("platforms"), dict):
            raise InstallError("release catalog has an unsupported schema")
        entry = catalog["platforms"].get(PLATFORM)
        if not isinstance(entry, dict):
            raise InstallError("release catalog has no Linux x86_64 entry")
        if entry.get("status") != "available":
            raise InstallError(f"Linux x86_64 release is not available (catalog status: {entry.get('status', 'missing')})")
        version, tag, asset, digest, source_sha = (entry.get(key) for key in ("version", "tag", "asset", "sha256", "source_sha"))
        if not isinstance(version, str) or not SEMVER_RE.fullmatch(version) or tag != "v" + version:
            raise InstallError("Linux release catalog contains an invalid version or tag")
        if not isinstance(asset, str) or not ASSET_RE.fullmatch(asset):
            raise InstallError("Linux release catalog contains an invalid archive name")
        if not isinstance(digest, str) or not SHA_RE.fullmatch(digest):
            raise InstallError("Linux release catalog is missing a valid SHA-256 digest")
        if not isinstance(source_sha, str) or not SOURCE_RE.fullmatch(source_sha):
            raise InstallError("Linux release catalog is missing a valid source revision")
        manifest_url = "https://github.com/" + GITHUB_REPO + "/releases/download/" + urllib.parse.quote(tag, safe=".-") + "/release-manifest-linux-x64.json"
        raw_manifest = fetch_bytes(manifest_url, MAX_METADATA, "immutable Linux release manifest")
        try:
            release_manifest = validate_release_manifest(json.loads(raw_manifest))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise InstallError(f"immutable Linux release manifest is not valid JSON: {error}") from error
        expected = {"version": version, "source_sha": source_sha, "asset": asset, "sha256": digest}
        if any(release_manifest.get(key) != value for key, value in expected.items()):
            raise InstallError("immutable Linux release manifest disagrees with catalog metadata")
        if entry.get("minimum_glibc") != release_manifest["minimum_glibc"]:
            raise InstallError("Linux release catalog glibc floor differs from immutable release metadata")
    archive_url = "https://github.com/" + GITHUB_REPO + "/releases/download/" + urllib.parse.quote(tag, safe=".-") + "/" + urllib.parse.quote(entry["asset"], safe=".-")
    return entry, release_manifest, archive_url


def parse_pinned_manifest(tag: str, raw_manifest: bytes) -> tuple[dict, dict]:
    if not CHECKPOINT_TAG_RE.fullmatch(tag) or not SHA_RE.fullmatch(PINNED_MANIFEST_SHA256):
        raise InstallError("installer checkpoint pin is invalid")
    if len(raw_manifest) > MAX_METADATA or hashlib.sha256(raw_manifest).hexdigest() != PINNED_MANIFEST_SHA256:
        raise InstallError("immutable Linux release manifest SHA-256 mismatch")
    try:
        release_manifest = validate_release_manifest(json.loads(raw_manifest))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError(f"immutable Linux release manifest is not valid JSON: {error}") from error
    if not tag.endswith("-" + release_manifest["source_sha"][:10] + "-linux-x64"):
        raise InstallError("checkpoint release tag does not match the manifest source revision")
    entry = {key: release_manifest[key] for key in ("version", "source_sha", "asset", "sha256", "minimum_glibc")}
    entry["tag"] = tag
    return entry, release_manifest


def load_local_candidate(directory: Path, temp_directory: Path) -> tuple[dict, dict, Path]:
    """Stage a local archive only when it matches this pinned installer exactly."""
    if not PINNED_RELEASE_TAG or not PINNED_MANIFEST_SHA256:
        raise InstallError("--candidate-directory is available only in a digest-pinned checkpoint installer")
    directory = directory.expanduser().resolve()
    manifest_path = directory / "release-manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file() or manifest_path.stat().st_size > MAX_METADATA:
        raise InstallError("candidate directory has no regular release-manifest.json under the metadata size limit")
    raw_manifest = manifest_path.read_bytes()
    entry, manifest = parse_pinned_manifest(PINNED_RELEASE_TAG, raw_manifest)
    archive_source = directory / entry["asset"]
    sidecar = directory / (entry["asset"] + ".sha256")
    if archive_source.is_symlink() or not archive_source.is_file():
        raise InstallError("candidate directory is missing its regular Linux archive")
    if sidecar.is_symlink() or not sidecar.is_file() or sidecar.read_text() != f'{manifest["sha256"]}  {entry["asset"]}\n':
        raise InstallError("candidate archive SHA-256 sidecar does not match the pinned release manifest")
    if archive_source.stat().st_size != manifest["size_bytes"]:
        raise InstallError("candidate archive size differs from the pinned release manifest")
    staged = temp_directory / entry["asset"]
    digest = hashlib.sha256()
    count = 0
    try:
        with archive_source.open("rb") as source, staged.open("xb") as target:
            while True:
                chunk = source.read(1024 * 1024)
                if not chunk:
                    break
                count += len(chunk)
                if count > MAX_ARCHIVE_BYTES:
                    raise InstallError("candidate archive exceeds the 2 GiB safety limit")
                target.write(chunk)
                digest.update(chunk)
    except OSError as error:
        raise InstallError(f"could not stage the local candidate archive: {error}") from error
    if count != manifest["size_bytes"] or digest.hexdigest() != manifest["sha256"]:
        raise InstallError("candidate archive size or SHA-256 differs from the pinned release manifest")
    return entry, manifest, staged


def download_archive(url: str, expected_sha256: str, expected_size: int, destination: Path) -> None:
    if urllib.parse.urlsplit(url).hostname != "github.com" or urllib.parse.urlsplit(url).scheme != "https":
        raise InstallError("release archive URL is outside the expected HTTPS GitHub release host")
    request = urllib.request.Request(url, headers={"User-Agent": "nudox-installer/1", "Accept": "application/octet-stream"})
    digest = hashlib.sha256()
    count = 0
    try:
        with HTTPS.open(request, timeout=120) as response, destination.open("xb") as output:
            final_host = urllib.parse.urlsplit(response.geturl()).hostname or ""
            if final_host != "github.com" and not final_host.endswith(".githubusercontent.com"):
                raise InstallError("GitHub release download redirected to an unexpected host")
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                count += len(chunk)
                if count > MAX_ARCHIVE_BYTES:
                    raise InstallError("release archive exceeds the 2 GiB safety limit")
                output.write(chunk)
                digest.update(chunk)
    except urllib.error.HTTPError as error:
        raise InstallError(f"release archive request failed with HTTP {error.code}") from error
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise InstallError(f"could not download the release archive: {error}") from error
    if count != expected_size:
        raise InstallError(f"release archive size mismatch (expected {expected_size}, received {count})")
    if digest.hexdigest() != expected_sha256:
        raise InstallError("release archive SHA-256 mismatch; nothing was installed")


def _safe_member(member: tarfile.TarInfo, topdir: str) -> str | None:
    name = member.name
    if not name or "\\" in name or name.startswith("/"):
        raise InstallError(f"archive has an unsafe path: {name!r}")
    stripped = name[:-1] if name.endswith("/") and member.isdir() else name
    raw_parts = stripped.split("/")
    if any(part in {"", ".", ".."} for part in raw_parts):
        raise InstallError(f"archive has an unsafe path: {name!r}")
    path = PurePosixPath(stripped)
    if path.parts[0] != topdir:
        raise InstallError(f"archive member is outside expected directory {topdir!r}: {name!r}")
    if member.issym() or member.islnk() or not (member.isdir() or member.isreg()):
        raise InstallError(f"archive contains a link or special file: {name!r}")
    if member.size < 0 or member.size > MAX_MEMBER_BYTES:
        raise InstallError(f"archive member exceeds the per-file safety limit: {name!r}")
    relative = PurePosixPath(*path.parts[1:])
    if not relative.parts:
        if member.isdir():
            return None
        raise InstallError("archive root must be a directory")
    return relative.as_posix()


def safe_extract(archive: Path, destination: Path) -> None:
    topdir = "nudox-linux-x86_64"
    seen: set[str] = set()
    total = 0
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            members = bundle.getmembers()
            if not members or len(members) > 10000:
                raise InstallError("archive has an empty or excessive member list")
            parsed: list[tuple[tarfile.TarInfo, str | None]] = []
            for member in members:
                relative = _safe_member(member, topdir)
                key = relative if relative is not None else "."
                if key in seen:
                    raise InstallError(f"archive contains a duplicate member: {member.name!r}")
                seen.add(key)
                total += member.size
                if total > MAX_ARCHIVE_BYTES:
                    raise InstallError("archive uncompressed size exceeds the 2 GiB safety limit")
                parsed.append((member, relative))
            files = {relative for member, relative in parsed if relative is not None and member.isreg()}
            required = {"bin/" + name for name in REQUIRED_BINARIES} | {"build-manifest.json", "packaging-manifest.json", "README.txt"}
            if not required.issubset(files):
                raise InstallError("archive is missing one or more required product files or manifests")
            allowed_bin = {"bin/" + name for name in REQUIRED_BINARIES}
            if not allowed_bin.issubset(files):
                raise InstallError("archive omits a required Linux CLI/MCP/locald executable")
            for member, relative in parsed:
                if relative is None:
                    continue
                target = destination.joinpath(*PurePosixPath(relative).parts)
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                if member.isdir():
                    target.mkdir(mode=0o700, exist_ok=True)
                    continue
                source = bundle.extractfile(member)
                if source is None:
                    raise InstallError(f"archive member could not be read: {member.name!r}")
                mode = 0o755 if relative in allowed_bin or relative == "share/nudox/typescript/node/bin/node" else 0o644
                flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
                descriptor = os.open(target, flags, mode)
                with os.fdopen(descriptor, "wb") as output, source:
                    shutil.copyfileobj(source, output, 1024 * 1024)
                os.chmod(target, mode)
    except (tarfile.TarError, OSError) as error:
        if isinstance(error, InstallError):
            raise
        raise InstallError(f"could not safely extract release archive: {error}") from error


def _file_sha256(path: Path) -> str:
    """Hash one bounded regular object without link following or FIFO waits on reuse."""
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_MEMBER_BYTES:
                raise InstallError("installed file is not a bounded regular file")
            digest = hashlib.sha256()
            count = 0
            while count <= before.st_size:
                chunk = stream.read(min(1024 * 1024, before.st_size - count + 1))
                if not chunk:
                    break
                count += len(chunk)
                digest.update(chunk)
            after = os.fstat(stream.fileno())
            current = path.lstat()
            identity = lambda metadata: (metadata.st_dev, metadata.st_ino, metadata.st_size,
                                         metadata.st_mtime_ns, metadata.st_ctime_ns)
            if count != before.st_size or identity(before) != identity(after) or identity(current) != identity(after):
                raise InstallError("installed file changed during bounded hashing")
            return digest.hexdigest()
    except OSError as error:
        raise InstallError(f"installed file could not be safely hashed: {error}") from error


def _glibc_version() -> tuple[int, int] | None:
    try:
        value = os.confstr("CS_GNU_LIBC_VERSION")
    except (OSError, ValueError):
        return None
    match = re.search(r"([0-9]+)\.([0-9]+)", value or "")
    return (int(match.group(1)), int(match.group(2))) if match else None



def _verified_package_path(root: Path, relative: str, *, directory: bool = False) -> Path:
    """Admit real package descendants, including every parent, before reuse or execution."""
    try:
        if root.is_symlink() or not root.is_dir() or root.resolve(strict=True) != root:
            raise InstallError("package root is not a canonical real directory")
        parts = PurePosixPath(relative).parts
        if "\\" in relative or any(part in {".", "..", "/"} for part in parts):
            raise InstallError("package path contains an unsafe component")
        path = root
        for index, part in enumerate(parts):
            path = path / part
            last = index == len(parts) - 1
            if path.is_symlink() or (not last or directory) and not path.is_dir() or last and not directory and not path.is_file():
                raise InstallError("package path contains a link or invalid file/directory boundary: " + relative)
        if path.resolve(strict=True) != path or not path.is_relative_to(root):
            raise InstallError("package path leaves its canonical root: " + relative)
        return path
    except OSError as error:
        raise InstallError("package path is unreadable: " + relative) from error


def verify_typescript_sdk(root: Path, package: dict) -> None:
    """Recheck the installed SDK before promotion or reuse; never run project setup."""
    sdk = package.get("typescript_sdk")
    if sdk is None or (isinstance(sdk, dict) and sdk.get("bundled") is False):
        return  # Legacy packages make no bundled SDK claim.
    if not isinstance(sdk, dict) or sdk.get("schema") != "nudox.typescript-sdk.v1" or sdk.get("bundled") is not True or sdk.get("root") != "share/nudox/typescript":
        raise InstallError("package has an invalid bundled TypeScript SDK contract")
    files = sdk.get("files")
    if not isinstance(files, dict) or not files or len(files) > 512:
        raise InstallError("SDK has no bounded complete file inventory")
    expected_directories = set()
    for relative, record in files.items():
        if not isinstance(relative, str):
            raise InstallError("SDK inventory contains a non-string path")
        path = PurePosixPath(relative)
        if not relative.startswith("share/nudox/typescript/") or path.as_posix() != relative or "\\" in relative or any(part in {".", ".."} for part in path.parts):
            raise InstallError("SDK inventory contains an unsafe path")
        expected_directories.update(parent.as_posix() for parent in path.parents if parent.parts)
        if not isinstance(record, dict) or record.get("kind") != "file" or not isinstance(record.get("sha256"), str) or not SHA_RE.fullmatch(record["sha256"]):
            raise InstallError("SDK inventory contains an invalid file record")
    observed = set()
    total = 0
    package_bytes = 0
    pending = [_verified_package_path(root, "share/nudox/typescript", directory=True)]
    while pending:
        for path in pending.pop().iterdir():
            relative = path.relative_to(root).as_posix()
            _verified_package_path(root, relative, directory=path.is_dir())
            if path.is_symlink() or not (path.is_file() or path.is_dir()):
                raise InstallError("installed SDK contains a link or special file")
            if path.is_dir():
                if relative not in expected_directories:
                    raise InstallError("installed SDK contains an unreceipted directory")
                pending.append(path)
                continue
            record = files.get(relative)
            member_bytes = path.stat().st_size
            total += member_bytes
            if relative.startswith("share/nudox/typescript/node_modules/typescript/"):
                package_bytes += member_bytes
                if package_bytes > 96 * 1024 * 1024:
                    raise InstallError("installed TypeScript package exceeds its 96 MiB byte bound")
            if record is None or total > 512 * 1024 * 1024 or len(observed) >= 512:
                raise InstallError("installed SDK differs from its bounded inventory")
            if record["sha256"] != _file_sha256(path) or record.get("size_bytes") != path.stat().st_size:
                raise InstallError("installed SDK file differs from its receipt: " + relative)
            observed.add(relative)
    if observed != set(files):
        raise InstallError("installed SDK has missing or unreceipted files")
    node_relative = "share/nudox/typescript/node/bin/node"
    required = {node_relative, "share/nudox/typescript/node/LICENSE",
                "share/nudox/typescript/node_modules/typescript/package.json",
                "share/nudox/typescript/node_modules/typescript/bin/tsc",
                "share/nudox/typescript/node_modules/typescript/lib/typescript.js",
                "share/nudox/typescript/node_modules/typescript/LICENSE.txt",
                "share/nudox/typescript/node_modules/typescript/ThirdPartyNoticeText.txt"}
    if not required.issubset(observed):
        raise InstallError("SDK omits its runtime, compiler API, or license notices")
    node = root / node_relative
    record = sdk.get("node")
    if not isinstance(record, dict) or record.get("packaged_path") != node_relative or record.get("packaged_sha256") != files[node_relative]["sha256"] or not os.access(node, os.X_OK):
        raise InstallError("SDK Node executable differs from its relocated identity")
    clean_environment = {key: value for key, value in os.environ.items()
                         if key not in {"LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "NIX_LD", "NIX_LD_LIBRARY_PATH", "NODE_PATH", "NODE_OPTIONS"}}
    for arguments, expected in [(["--version"], sdk.get("node_version")),
        (["-e", "process.stdout.write(require(process.argv[1]).version)", str(root / "share/nudox/typescript/node_modules/typescript/lib/typescript.js")], sdk.get("typescript_version"))]:
        try:
            result = subprocess.run([str(node), *arguments], env=clean_environment, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True, timeout=15)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise InstallError(f"installed SDK could not start without loader overrides: {error}") from error
        if not isinstance(expected, str) or result.returncode != 0 or result.stdout.strip() != expected:
            raise InstallError("installed SDK does not reproduce its recorded Node/Compiler API identity")

def verify_package(root: Path, entry: dict, manifest: dict) -> None:
    _verified_package_path(root, "", directory=True)
    for relative in ("build-manifest.json", "packaging-manifest.json", *("bin/" + name for name in REQUIRED_BINARIES)):
        _verified_package_path(root, relative)
    if not (root / "bin/backend-cli").is_file() or not os.access(root / "bin/backend-cli", os.X_OK):
        raise InstallError("installed stage does not contain an executable backend-cli")
    if any((root / name).stat().st_size > 16 * 1024 * 1024 for name in ("build-manifest.json", "packaging-manifest.json")):
        raise InstallError("package evidence exceeds the 16 MiB safety limit")
    try:
        build = json.loads((root / "build-manifest.json").read_text())
        package = json.loads((root / "packaging-manifest.json").read_text())
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError(f"package evidence is unreadable: {error}") from error
    build_source = build.get("source") if isinstance(build.get("source"), dict) else {}
    build_revision = build.get("source_sha") or build_source.get("commit") or build_source.get("git_revision")
    if build_revision != entry["source_sha"]:
        raise InstallError("build manifest source revision differs from the promoted release")
    if package.get("schema") != "nudox.linux-portable-package.v1":
        raise InstallError("package manifest has an unsupported schema")
    if _file_sha256(root / "build-manifest.json") != manifest["build_manifest_sha256"]:
        raise InstallError("build manifest hash differs from immutable release metadata")
    if _file_sha256(root / "packaging-manifest.json") != manifest["packaging_manifest_sha256"]:
        raise InstallError("package manifest hash differs from immutable release metadata")
    binaries = package.get("executables")
    if not isinstance(binaries, list):
        raise InstallError("package manifest has no executable inventory")
    binary_records = {item.get("name"): item for item in binaries if isinstance(item, dict)}
    if set(binary_records) != set(REQUIRED_BINARIES):
        raise InstallError("package manifest executable inventory is incomplete or ambiguous")
    for name, record in binary_records.items():
        if record.get("packaged_path") != f"bin/{name}" or record.get("packaged_sha256") != _file_sha256(root / "bin" / name):
            raise InstallError(f"package manifest does not attest the installed executable {name}")
    libraries = package.get("libraries")
    if not isinstance(libraries, dict):
        raise InstallError("package manifest has no shared-library inventory")
    if len(libraries) > 64:
        raise InstallError("package shared-library closure exceeds its 64 member bound")
    library_bytes = 0
    for soname, record in libraries.items():
        if not isinstance(record, dict) or record.get("packaged_path") != f"lib/{soname}":
            raise InstallError(f"package manifest has an invalid shared-library record for {soname}")
        if not isinstance(soname, str) or PurePosixPath(soname).name != soname or "\\" in soname or soname in {".", ".."}:
            raise InstallError("package has an unsafe shared-library name")
        library = _verified_package_path(root, "lib/" + soname)
        member_bytes = library.stat().st_size
        library_bytes += member_bytes
        if member_bytes > 96 * 1024 * 1024 or library_bytes > 512 * 1024 * 1024:
            raise InstallError("package shared-library closure exceeds its byte bound")
        if not library.is_file() or record.get("packaged_sha256") != _file_sha256(library):
            raise InstallError(f"package manifest does not attest shared library {soname}")
    minimum = tuple(int(part) for part in manifest["minimum_glibc"].split("."))
    host_glibc = _glibc_version()
    if host_glibc is None or host_glibc < minimum:
        detected = ".".join(map(str, host_glibc)) if host_glibc else "unknown"
        raise InstallError(f"this release requires glibc {manifest['minimum_glibc']} or newer; this host reports {detected}")
    for name in REQUIRED_BINARIES:
        binary = root / "bin" / name
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise InstallError(f"package omits executable {name}")
    verify_typescript_sdk(root, package)
    # Run harmless entrypoint checks with build-machine loader overrides removed.
    clean_env = {key: value for key, value in os.environ.items() if key not in {"LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT", "NIX_LD", "NIX_LD_LIBRARY_PATH"}}
    for name, args in (("backend-cli", ["--version"]), ("backend-mcp", ["--help"])):
        try:
            result = subprocess.run([str(root / "bin" / name), *args], env=clean_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise InstallError(f"packaged {name} could not start in a clean environment: {error}") from error
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().splitlines()
            raise InstallError(f"packaged {name} startup check failed: {detail[0] if detail else 'nonzero exit'}")


def _version_root(prefix: Path) -> Path:
    return prefix / "lib" / "nudox" / "versions"


def _managed_link(path: Path, managed_root: Path) -> bool:
    if not path.is_symlink():
        return False
    try:
        resolved = path.resolve(strict=False)
        resolved.relative_to(managed_root.resolve(strict=False))
        return True
    except (OSError, ValueError):
        return False


def _replace_symlink(link: Path, target: str) -> None:
    temp = link.with_name(link.name + ".nudox-new-" + str(os.getpid()) + "-" + str(time.time_ns()))
    try:
        os.symlink(target, temp)
        os.replace(temp, link)
    finally:
        try:
            temp.unlink()
        except FileNotFoundError:
            pass


def verify_existing_install(final: Path, expected_marker: dict, entry: dict, manifest: dict) -> None:
    """Reuse a prior release only after rechecking the files on disk."""
    installed_marker = _verified_package_path(final, ".installed-release.json")
    try:
        descriptor = os.open(installed_marker, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size > 16 * 1024:
                raise InstallError("installed release marker is not a bounded regular file")
            raw_marker = stream.read(16 * 1024 + 1)
            after = os.fstat(stream.fileno())
            if len(raw_marker) > 16 * 1024 or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns):
                raise InstallError("installed release marker changed during bounded admission")
            existing_marker = json.loads(raw_marker)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError(f"refusing to reuse existing unverified install at {final}") from error
    if existing_marker != expected_marker:
        raise InstallError(f"release {entry['tag']} is already installed with different bytes")
    try:
        verify_package(final, entry, manifest)
    except (InstallError, OSError, KeyError, TypeError, ValueError) as error:
        raise InstallError(f"existing release at {final} failed its integrity/startup recheck; refusing to reuse it: {error}") from error


def command_links(managed_root: Path) -> dict[str, str]:
    binaries = {
        "nudox": "backend-cli",
        "nudox-mcp": "backend-mcp",
        "nudox-locald": "backend-locald",
        # Keep the executable names used by current MCP setup guidance.
        "backend-cli": "backend-cli",
        "backend-mcp": "backend-mcp",
        "backend-locald": "backend-locald",
    }
    return {name: str(managed_root / "current" / "bin" / binary) for name, binary in binaries.items()}


def _validate_install_directory(path):
    metadata = path.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) & 0o022):
        raise InstallError("managed install directory is not a real private owned directory: " + str(path))


def _file_identity(value):
    return (value.st_dev, value.st_ino, value.st_size, value.st_uid, value.st_nlink,
            value.st_mode, value.st_mtime_ns, value.st_ctime_ns)


def _validate_marker(marker):
    fields = {"version", "tag", "source_sha", "asset", "sha256"}
    if not isinstance(marker, dict) or set(marker) != fields:
        raise InstallError("release marker fields are invalid")
    if not isinstance(marker["version"], str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", marker["version"]):
        raise InstallError("release marker version is invalid")
    if not isinstance(marker["source_sha"], str) or not re.fullmatch(r"[a-f0-9]{40}", marker["source_sha"]):
        raise InstallError("release marker source is invalid")
    if not isinstance(marker["sha256"], str) or not re.fullmatch(r"[a-f0-9]{64}", marker["sha256"]):
        raise InstallError("release marker archive hash is invalid")
    asset_prefix = "nudox-macos-arm64-" if PLATFORM == "macos-arm64" else "nudox-linux-x86_64-"
    asset = re.fullmatch(re.escape(asset_prefix) + r"([a-f0-9]{10,40})\.tar\.gz", marker["asset"] if isinstance(marker["asset"], str) else "")
    if asset is None or not marker["source_sha"].startswith(asset[1]):
        raise InstallError("release marker archive does not match its platform/source")
    tag = marker["tag"]
    if not isinstance(tag, str):
        raise InstallError("release marker tag is invalid")
    checkpoint = re.fullmatch(r"checkpoint-([0-9]{8})-([a-f0-9]{10})-" + re.escape(PLATFORM), tag)
    if checkpoint:
        if checkpoint[2] != marker["source_sha"][:10]:
            raise InstallError("release marker checkpoint does not match its source")
        try:
            return datetime.date(int(checkpoint[1][:4]), int(checkpoint[1][4:6]), int(checkpoint[1][6:8]))
        except ValueError as error:
            raise InstallError("release marker checkpoint date is invalid") from error
    if PLATFORM == "linux-x64" and tag == "v" + marker["version"]:
        return None
    raise InstallError("release marker has an unknown checkpoint identity")


def _read_active_marker(current):
    marker = current.resolve(strict=True) / ".installed-release.json"
    try:
        descriptor = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            before = os.fstat(stream.fileno())
            if (not stat.S_ISREG(before.st_mode) or before.st_size > 16 * 1024
                    or before.st_uid != os.geteuid() or before.st_nlink != 1
                    or stat.S_IMODE(before.st_mode) & 0o022):
                raise InstallError("active release marker is not a bounded private owned regular file")
            raw = stream.read(16 * 1024 + 1)
            after = os.fstat(stream.fileno())
            if len(raw) != before.st_size or _file_identity(before) != _file_identity(after) or _file_identity(after) != _file_identity(marker.lstat()):
                raise InstallError("active release marker changed during admission")
        return json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InstallError("cannot verify active checkpoint before update") from error


def check_checkpoint_update(current, incoming, allow_downgrade=False):
    """Admit exact marker identities; the explicit flag changes ordering only."""
    incoming_date = _validate_marker(incoming)
    if not current.exists():
        if current.is_symlink():
            raise InstallError("active install pointer is dangling")
        return
    installed = _read_active_marker(current)
    active_date = _validate_marker(installed)
    if incoming["tag"] == installed["tag"]:
        if incoming != installed:
            raise InstallError("active release tag is already installed with different bytes")
        return
    if allow_downgrade:
        return
    if incoming_date is None or active_date is None:
        if incoming_date is None and active_date is None and tuple(map(int, incoming["version"].split("."))) > tuple(map(int, installed["version"].split("."))):
            return
        raise InstallError("refusing unordered checkpoint/stable replacement; use --allow-downgrade explicitly")
    if incoming_date <= active_date:
        reason = "older" if incoming_date < active_date else "different same-date"
        raise InstallError(f"refusing {reason} checkpoint {incoming['tag']}; active is {installed['tag']}. Use --allow-downgrade to replace it explicitly")


@contextmanager
def _install_lease(managed_root):
    """Hold one owned inode from checkpoint admission through publication."""
    _validate_install_directory(managed_root)
    path = managed_root / ".install.lock"
    descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        before = os.fstat(descriptor)
        if (not stat.S_ISREG(before.st_mode) or before.st_size != 0
                or before.st_uid != os.geteuid() or before.st_nlink != 1
                or stat.S_IMODE(before.st_mode) != 0o600):
            raise InstallError("install lease is not a private owned single-link regular file")
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        if _file_identity(before) != _file_identity(os.fstat(descriptor)) or _file_identity(before) != _file_identity(path.lstat()):
            raise InstallError("install lease identity changed while waiting")
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def install(prefix: Path, entry: dict, manifest: dict, archive: Path, *, allow_downgrade: bool = False) -> None:
    os.umask(0o077)
    prefix.mkdir(mode=0o700, parents=True, exist_ok=True)
    _validate_install_directory(prefix)
    lib_dir = prefix / "lib"
    if lib_dir.is_symlink():
        raise InstallError(f"managed install parent must not be a symlink: {lib_dir}")
    lib_dir.mkdir(mode=0o700, exist_ok=True)
    _validate_install_directory(lib_dir)
    if not lib_dir.is_dir():
        raise InstallError(f"managed install parent is not a directory: {lib_dir}")
    managed_root = prefix / "lib" / "nudox"
    if managed_root.is_symlink():
        raise InstallError(f"managed install root must not be a symlink: {managed_root}")
    managed_root.mkdir(mode=0o700, exist_ok=True)
    _validate_install_directory(managed_root)
    if not managed_root.is_dir():
        raise InstallError("managed install root is not a directory")
    with _install_lease(managed_root):
        _install_locked(prefix, entry, manifest, archive, allow_downgrade=allow_downgrade)


def _install_locked(prefix, entry, manifest, archive, *, allow_downgrade=False):
    managed_root = prefix / "lib" / "nudox"
    version_root = _version_root(prefix)
    version_root.mkdir(mode=0o700, exist_ok=True)
    if version_root.is_symlink() or not version_root.is_dir():
        raise InstallError("managed install versions path must be a real directory")
    install_id = entry.get("tag")
    if not isinstance(install_id, str) or not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", install_id):
        raise InstallError("release has an unsafe install identity")
    final = version_root / install_id
    bin_dir = prefix / "bin"
    if bin_dir.is_symlink():
        raise InstallError(f"command install directory must not be a symlink: {bin_dir}")
    bin_dir.mkdir(mode=0o700, exist_ok=True)
    if not bin_dir.is_dir():
        raise InstallError(f"command install path is not a directory: {bin_dir}")
    links = command_links(managed_root)
    for name, target in links.items():
        link = bin_dir / name
        if link.exists() and not link.is_symlink():
            raise InstallError(f"refusing to replace existing non-symlink command: {link}")
        if link.is_symlink() and os.readlink(link) != target and not _managed_link(link, managed_root):
            raise InstallError(f"refusing to replace command symlink outside the managed NuDox install: {link}")
    current = managed_root / "current"
    if current.exists() and not current.is_symlink():
        raise InstallError(f"refusing to replace non-symlink active install pointer: {current}")
    if current.is_symlink() and not _managed_link(current, managed_root):
        raise InstallError(f"refusing to replace active install pointer outside the managed NuDox install: {current}")
    marker = {"version": entry["version"], "tag": entry["tag"], "source_sha": entry["source_sha"], "asset": entry["asset"], "sha256": manifest["sha256"]}
    check_checkpoint_update(current, marker, allow_downgrade)
    stage = Path(tempfile.mkdtemp(prefix=".staging-", dir=version_root))
    try:
        safe_extract(archive, stage)
        verify_package(stage, entry, manifest)
        (stage / ".installed-release.json").write_text(json.dumps(marker, sort_keys=True) + "\n")
        os.chmod(stage, 0o755)
        if final.exists() or final.is_symlink():
            if final.is_symlink() or not final.is_dir():
                raise InstallError(f"version path already exists and is not an install directory: {final}")
            verify_existing_install(final, marker, entry, manifest)
            shutil.rmtree(stage)
        else:
            os.replace(stage, final)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise

    for name, target in links.items():
        link = bin_dir / name
        if not link.is_symlink() or os.readlink(link) != target:
            _replace_symlink(link, target)

    _replace_symlink(current, os.path.relpath(final, managed_root))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, help="user install prefix (default: ~/.local)")
    parser.add_argument("--allow-downgrade", action="store_true", help="allow an older or different same-date checkpoint")
    parser.add_argument("--candidate-directory", type=Path, help="install the exact digest-pinned package output for native QA")
    args = parser.parse_args(argv)
    if sys.platform != "linux" or not (os.uname().machine.lower() in {"x86_64", "amd64"}):
        raise InstallError("this installer supports Linux x86_64 only")
    raw_home = os.environ.get("HOME")
    if not raw_home:
        raise InstallError("HOME is unset; pass --prefix with a user-writable directory")
    home = Path(raw_home).expanduser()
    prefix = (args.prefix or Path(os.environ.get("NUDOX_INSTALL_PREFIX", home / ".local"))).expanduser().resolve()
    with tempfile.TemporaryDirectory(prefix="nudox-download-") as temp_dir:
        if args.candidate_directory:
            entry, manifest, archive = load_local_candidate(args.candidate_directory, Path(temp_dir))
        else:
            entry, manifest, url = load_release()
            archive = Path(temp_dir) / entry["asset"]
            download_archive(url, manifest["sha256"], manifest["size_bytes"], archive)
        install(prefix, entry, manifest, archive, allow_downgrade=args.allow_downgrade)
    print(f"Installed NuDox {entry['version']} CLI, MCP server and local daemon in {prefix}.")
    print("Commands: nudox, nudox-mcp, nudox-locald, backend-cli, backend-mcp, backend-locald")
    print(f"MCP executable for client configuration: {prefix / 'bin' / 'backend-mcp'}")
    path_parts = os.environ.get("PATH", "").split(os.pathsep)
    if str(prefix / "bin") not in path_parts:
        print(f"Add this line to your shell profile to use the commands by name:\n  export PATH={shlex_quote(str(prefix / 'bin'))}:\"$PATH\"")
    print(f"Check the install with: {prefix / 'bin' / 'nudox'} --version")
    return 0


def shlex_quote(value: str) -> str:
    import shlex
    return shlex.quote(value)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except InstallError as error:
        print(f"nudox installer: {error}", file=sys.stderr)
        raise SystemExit(1)
    except (OSError, urllib.error.URLError, tarfile.TarError) as error:
        print(f"nudox installer: installation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
