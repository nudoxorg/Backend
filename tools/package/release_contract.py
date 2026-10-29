#!/usr/bin/env python3
"""Validate immutable native candidates, publish them, and promote one platform."""
from __future__ import annotations

import argparse
import ast
import base64
import datetime
import stat
import tarfile
import tempfile
import hashlib
import http.client
import json
import os
import plistlib
import re
import shlex
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

from release_platforms import PLATFORMS

ASSET = PLATFORMS["macos"]["asset"]
QA_CASES = PLATFORMS["macos"]["qa"]

LINUX_ASSET_RE = re.compile(r"nudox-linux-x86_64-[a-f0-9]{10,40}\.tar\.gz\Z")
LINUX_QA_CASES = {"cli_version", "project_add_search", "mcp_help", "mcp_session", "locald_sibling_discovery", "clean_environment_without_nix_paths", "ubuntu_glibc_floor"}
LINUX_MAX_ARCHIVE_BYTES = 2 * 1024 * 1024 * 1024
LINUX_MAX_ARCHIVE_MEMBERS = 10000
LINUX_MAX_MEMBER_BYTES = 1024 * 1024 * 1024


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def linux_archive_relative(member):
    """Return the canonical path below the Linux tar root, or None for its directory."""
    name = member.name
    if not name or name.startswith("/") or "\\" in name:
        raise ValueError("Linux archive contains an unsafe path")
    stripped = name[:-1] if name.endswith("/") and member.isdir() else name
    parts = stripped.split("/")
    if any(part in {"", ".", ".."} for part in parts) or parts[0] != "nudox-linux-x86_64":
        raise ValueError("Linux archive contains a noncanonical or unsafe path")
    if member.issym() or member.islnk() or not (member.isdir() or member.isreg()):
        raise ValueError("Linux archive contains links or special files")
    if member.size < 0 or member.size > LINUX_MAX_MEMBER_BYTES:
        raise ValueError("Linux archive member exceeds the safety limit")
    if len(parts) == 1:
        if member.isdir():
            return None
        raise ValueError("Linux archive root must be a directory")
    return "/".join(parts[1:])


def validate_linux_archive_members(members):
    if not members or len(members) > LINUX_MAX_ARCHIVE_MEMBERS:
        raise ValueError("Linux archive has an empty or excessive member list")
    paths = {}
    total = 0
    for member in members:
        relative = linux_archive_relative(member)
        key = relative if relative is not None else "."
        if key in paths:
            raise ValueError("Linux archive has duplicate normalized paths")
        paths[key] = member
        total += member.size
        if total > LINUX_MAX_ARCHIVE_BYTES:
            raise ValueError("Linux archive uncompressed size exceeds the 2 GiB safety limit")

    for relative, member in paths.items():
        if relative == ".":
            continue
        parts = relative.split("/")
        for index in range(1, len(parts)):
            ancestor = "/".join(parts[:index])
            if ancestor in paths and not paths[ancestor].isdir():
                raise ValueError("Linux archive has a file used as a parent directory")

    required_files = {
        "bin/backend-cli", "bin/backend-mcp", "bin/backend-locald",
        "build-manifest.json", "packaging-manifest.json", "README.txt",
    }
    regular_files = {path for path, member in paths.items() if member.isfile()}
    if not required_files.issubset(regular_files):
        raise ValueError("Linux archive has duplicate paths or omits required product files")
    return paths


def read_bounded_linux_members(bundle):
    """Read archive headers lazily and stop before allocating beyond the member cap."""
    members = []
    total = 0
    for member in bundle:
        if len(members) >= LINUX_MAX_ARCHIVE_MEMBERS:
            raise ValueError("Linux archive has too many members")
        linux_archive_relative(member)
        total += member.size
        if total > LINUX_MAX_ARCHIVE_BYTES:
            raise ValueError("Linux archive uncompressed size exceeds the 2 GiB safety limit")
        members.append(member)
    return members


def linux_checkpoint_tag(manifest, installer_bytes, manifest_sha256):
    """Bind the pinned Linux installer, package tag, archive name and manifest bytes."""
    try:
        tree = ast.parse(installer_bytes)
        values = {}
        required = {"PLATFORM", "PINNED_RELEASE_TAG", "PINNED_MANIFEST_SHA256"}
        for node in tree.body:
            if (isinstance(node, ast.Assign) and len(node.targets) == 1
                    and isinstance(node.targets[0], ast.Name) and node.targets[0].id in required):
                name = node.targets[0].id
                if name in values:
                    raise ValueError(f"Linux installer assigns {name} more than once")
                values[name] = ast.literal_eval(node.value)
        if set(values) != required:
            raise ValueError("Linux installer omits its immutable checkpoint pins")
    except (SyntaxError, ValueError, TypeError) as error:
        raise ValueError(f"Linux installer checkpoint pins are invalid: {error}") from error

    source = manifest.get("source_sha")
    tag = values["PINNED_RELEASE_TAG"]
    if (values["PLATFORM"] != "linux-x64" or values["PINNED_MANIFEST_SHA256"] != manifest_sha256
            or not isinstance(source, str) or not re.fullmatch(r"[a-f0-9]{40}", source)):
        raise ValueError("Linux installer does not pin this platform, source and exact manifest bytes")
    match = re.fullmatch(r"checkpoint-([0-9]{8})-([a-f0-9]{10})-linux-x64", tag) if isinstance(tag, str) else None
    if match is None or match[2] != source[:10]:
        raise ValueError("Linux installer checkpoint tag differs from the manifest source")
    try:
        datetime.datetime.strptime(match[1], "%Y%m%d")
    except ValueError as error:
        raise ValueError("Linux installer checkpoint date is invalid") from error
    if manifest.get("asset") != f"nudox-linux-x86_64-{source[:10]}.tar.gz":
        raise ValueError("Linux archive name differs from its source revision")
    return tag


def read_json(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 65536:
        raise ValueError("release JSON must be a regular file under 64 KiB")
    return json.loads(path.read_text())


def metadata_path(directory, name, platform=None):
    directory = Path(directory)
    paths = [directory / (name + ".json")]
    if platform:
        paths.append(directory / f"{name}-{platform}.json")
    else:
        paths.extend(directory.glob(name + "-*.json"))
    existing = [path for path in paths if path.exists() or path.is_symlink()]
    if len(existing) != 1:
        raise ValueError(f"candidate requires exactly one {name} regular file")
    return existing[0]


def optional_metadata_path(directory, name, platform):
    paths = [Path(directory) / (name + ".json"), Path(directory) / f"{name}-{platform}.json"]
    existing = [path for path in paths if path.exists() or path.is_symlink()]
    if len(existing) > 1:
        raise ValueError(f"candidate has more than one {name} file")
    return existing[0] if existing else None


def read_ci_waiver(directory, manifest):
    """An explicit, recorded decision to release this exact source despite red CI.

    It names the CI contexts it waives, so any other context still has to pass,
    and it is published beside the archive so the release says it was waived.
    """
    path = optional_metadata_path(directory, "ci-waiver", manifest["platform"])
    if path is None:
        return None
    waiver = read_json(path)
    if set(waiver) != {"schema", "source_sha", "contexts", "reason", "approved_by", "approved_at"} or waiver["schema"] != 1:
        raise ValueError("invalid CI waiver schema")
    if waiver["source_sha"] != manifest["source_sha"]:
        raise ValueError("CI waiver does not name this candidate's source revision")
    allowed = PLATFORMS[manifest["platform"]]["ci"]
    if (not isinstance(waiver["contexts"], list) or not waiver["contexts"]
            or len(set(waiver["contexts"])) != len(waiver["contexts"]) or not set(waiver["contexts"]) <= set(allowed)):
        raise ValueError(f"CI waiver contexts must be a non-empty subset of {allowed}")
    if not all(isinstance(waiver[name], str) and waiver[name].strip() for name in ("reason", "approved_by", "approved_at")):
        raise ValueError("CI waiver requires a reason, approver and UTC timestamp")
    try:
        approved_at = datetime.datetime.fromisoformat(waiver["approved_at"].replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("CI waiver timestamp must be UTC ISO 8601") from error
    if approved_at.utcoffset() != datetime.timedelta(0):
        raise ValueError("CI waiver timestamp must be UTC")
    return waiver


def archive_evidence(archive, profile):
    def inspect(names, read, regular):
        if len(names) != len(set(names)) or any(
            name.startswith("/") or ".." in name.split("/") or "\\" in name
            or ":" in name or name.split("/")[0] != profile["root"] for name in names
        ):
            raise ValueError("archive contains ambiguous or unsafe paths")
        if profile["host"] == "Windows" and len({name.casefold() for name in names}) != len(names):
            raise ValueError("archive contains case-ambiguous Windows paths")
        for executable in profile["executables"]:
            if executable not in names or not regular(executable):
                raise ValueError("archive omits a required product executable")
        build_bytes = read(profile["build"], 16 * 1024 * 1024)
        info = None
        if profile["host"] == "Darwin":
            info = plistlib.loads(read("Nudox.app/Contents/Info.plist", 65536))
        return build_bytes, info
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as bundle:
            def regular(name):
                mode = bundle.getinfo(name).external_attr >> 16
                return not bundle.getinfo(name).is_dir() and not stat.S_ISLNK(mode)
            def read(name, limit):
                if not regular(name) or bundle.getinfo(name).file_size > limit:
                    raise ValueError("invalid or oversized archive evidence")
                return bundle.read(name)
            return inspect(bundle.namelist(), read, regular)
    with tarfile.open(archive, "r:gz") as bundle:
        members = bundle.getmembers()
        # Portable Linux payloads must be staged with actual files, not links
        # escaping to the build machine's Nix store or compiler installation.
        if any(not (member.isfile() or member.isdir()) for member in members):
            raise ValueError("Linux archive contains links or special files")
        def read(name, limit):
            member = bundle.getmember(name)
            if not member.isfile() or member.size > limit:
                raise ValueError("invalid or oversized archive evidence")
            with bundle.extractfile(member) as stream:
                return stream.read(limit + 1)
        return inspect([member.name for member in members], read,
                       lambda name: bundle.getmember(name).isfile())


def validate(directory, source=None, platform=None, require_qa=True):
    directory = Path(directory)
    manifest = read_json(metadata_path(directory, "release-manifest", platform))
    # Preserve the existing receipted Linux checkpoint format. It carries
    # its own packaging evidence and native acceptance contract.
    if manifest.get("platform") == "linux-x64" and "minimum_glibc" in manifest:
        if platform and platform != "linux-x64":
            raise ValueError("candidate belongs to the wrong platform lane")
        return validate_linux(directory, manifest, source, require_qa=require_qa)
    required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_os", "build_manifest_sha256", "signed", "notarized"}
    if set(manifest) != required or manifest["schema"] != 1:
        raise ValueError("invalid release manifest schema")
    if not isinstance(manifest["version"], str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("invalid release version")
    for name in ("source_sha", "source_tree"):
        if not isinstance(manifest[name], str) or not re.fullmatch(r"[a-f0-9]{40}", manifest[name]):
            raise ValueError(f"invalid {name}")
    for name in ("cargo_lock_sha256", "sha256", "build_manifest_sha256"):
        if not isinstance(manifest[name], str) or not re.fullmatch(r"[a-f0-9]{64}", manifest[name]):
            raise ValueError(f"invalid {name}")
    selected = manifest["platform"]
    if not isinstance(selected, str) or selected not in PLATFORMS or (platform and selected != platform):
        raise ValueError("candidate belongs to the wrong platform lane")
    profile = PLATFORMS[selected]
    if manifest["target"] not in profile["targets"] or manifest["asset"] != profile["asset"]:
        raise ValueError("candidate target/asset differs from platform contract")
    if manifest["signed"] is not profile["signed"] or manifest["notarized"] is not profile["notarized"]:
        raise ValueError("candidate is missing platform signing/notarization evidence")
    if not isinstance(manifest["minimum_os"], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9 ._+-]{0,63}", manifest["minimum_os"]):
        raise ValueError("candidate requires a supported minimum OS")
    if selected == "macos" and not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", manifest["minimum_os"]):
        raise ValueError("invalid macOS minimum version")
    archive = directory / manifest["asset"]
    if (archive.is_symlink() or not archive.is_file() or type(manifest["size_bytes"]) is not int
            or archive.stat().st_size != manifest["size_bytes"] or sha256(archive) != manifest["sha256"]):
        raise ValueError("candidate archive differs from its immutable manifest")
    build_bytes, info = archive_evidence(archive, profile)
    build = json.loads(build_bytes)
    if hashlib.sha256(build_bytes).hexdigest() != manifest["build_manifest_sha256"] or any(build["source"].get(key) != manifest[field] for key, field in (("git_revision", "source_sha"), ("git_tree", "source_tree"), ("cargo_lock_sha256", "cargo_lock_sha256"))):
        raise ValueError("archive build evidence differs from release source")
    if info and (info.get("CFBundleShortVersionString") != manifest["version"] or info.get("LSMinimumSystemVersion") != manifest["minimum_os"]):
        raise ValueError("archive version/minimum OS differs from release metadata")
    checksum = directory / (manifest["asset"] + ".sha256")
    if checksum.is_symlink() or checksum.read_text() != f'{manifest["sha256"]}  {manifest["asset"]}\n':
        raise ValueError("checksum sidecar differs from manifest")
    if require_qa:
        qa = read_json(metadata_path(directory, "native-qa", selected))
        fields = {"schema", "archive_sha256", "source_sha", "tested_by", "tested_at", profile["qa_host"], "cases"}
        if selected != "macos":
            fields |= {"host_os", "target"}
        if set(qa) != fields or qa["schema"] != 1:
            raise ValueError("invalid native acceptance record")
        if qa["archive_sha256"] != manifest["sha256"] or qa["source_sha"] != manifest["source_sha"]:
            raise ValueError("native QA does not attest this exact archive and source")
        if not all(isinstance(qa[name], str) and qa[name].strip() for name in ("tested_by", "tested_at", profile["qa_host"])):
            raise ValueError("native QA requires operator, timestamp and host OS")
        try:
            tested_at = datetime.datetime.fromisoformat(qa["tested_at"].replace("Z", "+00:00"))
            if tested_at.utcoffset() != datetime.timedelta(0):
                raise ValueError("native acceptance timestamp must be UTC")
        except (TypeError, ValueError) as error:
            raise ValueError("native acceptance timestamp must be UTC ISO 8601") from error
        if selected != "macos" and (qa["host_os"] != profile["host"] or qa["target"] != manifest["target"]):
            raise ValueError("native QA was performed on the wrong OS/architecture")
        if not isinstance(qa["cases"], dict) or set(qa["cases"]) != profile["qa"] or any(value is not True for value in qa["cases"].values()):
            raise ValueError("native acceptance has missing or failed cases")
    if source is not None:
        source = Path(source)
        revision = manifest["source_sha"]
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, "HEAD"], check=True)
        def blob(path):
            return subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:{path}"])
        if hashlib.sha256(blob("Cargo.lock")).hexdigest() != manifest["cargo_lock_sha256"]:
            raise ValueError("candidate lock differs from selected canonical source")
        tree = subprocess.check_output(["git", "-C", str(source), "rev-parse", revision + "^{tree}"], text=True).strip()
        if tree != manifest["source_tree"] or tomllib.loads(blob("Cargo.toml").decode())["workspace"]["package"]["version"] != manifest["version"]:
            raise ValueError("candidate version/tree differs from selected canonical source")
    return manifest



def linux_checkpoint_shell_assets(directory, manifest, *, required=False):
    """Admit the optional legacy/new-required checkpoint shell and its exact sidecar."""
    directory = Path(directory)
    shell = directory / "install-linux-x64.sh"
    sidecar = directory / "install-linux-x64.sh.sha256"
    present = (os.path.lexists(shell), os.path.lexists(sidecar))
    if not any(present) and not required:
        return None  # Older portable-package records did not emit a shell asset.
    if not all(present):
        raise ValueError("Linux checkpoint shell requires both launcher and SHA-256 sidecar")
    from linux_release_package import checkpoint_shell, read_regular_bytes
    installer = read_regular_bytes(directory / "install-linux-x64.py", 1024 * 1024,
                                   "Linux checkpoint installer")
    manifest_digest = sha256(metadata_path(directory, "release-manifest", "linux-x64"))
    tag = linux_checkpoint_tag(manifest, installer, manifest_digest)
    raw = read_regular_bytes(shell, 64 * 1024, "Linux checkpoint shell")
    if raw != checkpoint_shell(tag, installer, manifest["minimum_glibc"]):
        raise ValueError("Linux checkpoint shell differs from its actual installer/tag/glibc pins")
    digest = hashlib.sha256(raw).hexdigest()
    sidecar_raw = read_regular_bytes(sidecar, 256, "Linux checkpoint shell sidecar")
    if sidecar_raw != f"{digest}  {shell.name}\n".encode():
        raise ValueError("Linux checkpoint shell sidecar differs from its exact bytes")
    # Carry the admitted cohort, never reopen mutable candidate paths as new authority.
    return tag, installer, [(shell.name, raw), (sidecar.name, sidecar_raw)]


def validate_linux(directory, manifest, source=None, *, require_qa=True, manifest_sha256=None):
    """Validate a receipted Ubuntu x86-64 package and, by default, its native QA."""
    required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform", "target", "asset", "sha256", "size_bytes", "minimum_glibc", "build_manifest_sha256", "packaging_manifest_sha256"}
    if set(manifest) != required or manifest["schema"] != 1:
        raise ValueError("invalid Linux release manifest schema")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", manifest["version"]):
        raise ValueError("invalid Linux release version")
    for name in ("source_sha", "source_tree"):
        if not re.fullmatch(r"[a-f0-9]{40}", manifest[name]):
            raise ValueError(f"invalid Linux {name}")
    for name in ("cargo_lock_sha256", "sha256", "build_manifest_sha256", "packaging_manifest_sha256"):
        if not re.fullmatch(r"[a-f0-9]{64}", manifest[name]):
            raise ValueError(f"invalid Linux {name}")
    if manifest["platform"] != "linux-x64" or manifest["target"] != "x86_64-unknown-linux-gnu" or not LINUX_ASSET_RE.fullmatch(manifest["asset"]):
        raise ValueError("Linux candidate has an unsupported platform, target or archive name")
    if not re.fullmatch(r"[0-9]+\.[0-9]+", manifest["minimum_glibc"]):
        raise ValueError("Linux candidate has an invalid minimum glibc version")
    archive_size = manifest["size_bytes"]
    if (type(archive_size) is not int or not 1 <= archive_size <= LINUX_MAX_ARCHIVE_BYTES):
        raise ValueError("Linux candidate has an invalid or oversized archive size")
    manifest_path = directory / "release-manifest.json"
    if read_json(manifest_path) != manifest:
        raise ValueError("Linux manifest file differs from the admitted manifest object")
    actual_manifest_sha256 = sha256(manifest_path)
    if manifest_sha256 is None:
        manifest_sha256 = actual_manifest_sha256
    elif manifest_sha256 != actual_manifest_sha256:
        raise ValueError("Linux manifest bytes changed after admission")
    if not isinstance(manifest_sha256, str) or not re.fullmatch(r"[a-f0-9]{64}", manifest_sha256):
        raise ValueError("Linux manifest SHA-256 is invalid")
    archive = directory / manifest["asset"]
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size != archive_size or sha256(archive) != manifest["sha256"]:
        raise ValueError("Linux archive differs from its immutable manifest")
    checksum = directory / (manifest["asset"] + ".sha256")
    if checksum.is_symlink() or not checksum.is_file() or checksum.read_text() != f'{manifest["sha256"]}  {manifest["asset"]}\n':
        raise ValueError("Linux checksum sidecar differs from manifest")
    try:
        with tarfile.open(archive, "r:gz") as bundle:
            members = read_bounded_linux_members(bundle)
            paths = validate_linux_archive_members(members)
            for name in ("bin/backend-cli", "bin/backend-mcp", "bin/backend-locald"):
                member = paths[name]
                if not (member.mode & 0o111):
                    raise ValueError("Linux product executable lost its executable bit")
            build_bytes = bundle.extractfile(paths["build-manifest.json"]).read(16 * 1024 * 1024 + 1)
            packaging_bytes = bundle.extractfile(paths["packaging-manifest.json"]).read(16 * 1024 * 1024 + 1)
            if len(build_bytes) > 16 * 1024 * 1024 or len(packaging_bytes) > 16 * 1024 * 1024:
                raise ValueError("Linux package evidence exceeds the 16 MiB safety limit")
            if hashlib.sha256(build_bytes).hexdigest() != manifest["build_manifest_sha256"] or hashlib.sha256(packaging_bytes).hexdigest() != manifest["packaging_manifest_sha256"]:
                raise ValueError("Linux archive evidence hashes differ from release manifest")
            build = json.loads(build_bytes)
            package = json.loads(packaging_bytes)
            executable_digests = {}
            for executable in ("backend-cli", "backend-mcp", "backend-locald"):
                member = paths[f"bin/{executable}"]
                stream = bundle.extractfile(member)
                if stream is None:
                    raise ValueError("Linux archive executable could not be read")
                digest = hashlib.sha256()
                byte_count = 0
                with stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        byte_count += len(chunk)
                        digest.update(chunk)
                executable_digests[executable] = {"sha256": digest.hexdigest(), "bytes": byte_count}
    except (tarfile.TarError, OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not validate Linux archive: {error}") from error

    build_fields = {"schema", "source", "executables", "root_receipt"}
    build_source_fields = {"commit", "tree", "clean"}
    if (not isinstance(build, dict) or set(build) != build_fields
            or build.get("schema") != "nudox.runtime-build-manifest.v1"):
        raise ValueError("Linux build manifest has an unsupported runtime-artifact schema")
    build_source = build.get("source")
    if (not isinstance(build_source, dict) or set(build_source) != build_source_fields
            or build_source.get("clean") is not True
            or build_source.get("commit") != manifest["source_sha"]
            or build_source.get("tree") != manifest["source_tree"]):
        raise ValueError("Linux build manifest source commit/tree/clean identity differs from release metadata")
    receipt = build.get("root_receipt")
    if (not isinstance(receipt, dict) or set(receipt) != {"schema", "sha256"}
            or receipt.get("schema") != "nudox.runtime-artifact-build-receipt.v1"
            or not isinstance(receipt.get("sha256"), str)
            or not re.fullmatch(r"[a-f0-9]{64}", receipt["sha256"])):
        raise ValueError("Linux build manifest has no hash-bound artifact receipt")
    executables = build.get("executables")
    if not isinstance(executables, dict) or set(executables) != {"backend-cli", "backend-mcp", "backend-locald"}:
        raise ValueError("Linux build manifest omits or adds executable identities")
    for executable in executables.values():
        if (not isinstance(executable, dict) or set(executable) != {"sha256", "bytes"}
                or not isinstance(executable.get("sha256"), str)
                or not re.fullmatch(r"[a-f0-9]{64}", executable["sha256"])
                or type(executable.get("bytes")) is not int
                or not 0 < executable["bytes"] <= 512 * 1024 * 1024):
            raise ValueError("Linux build manifest has an invalid executable digest or size")

    if not isinstance(package, dict) or package.get("schema") != "nudox.linux-portable-package.v1":
        raise ValueError("Linux package manifest has an unsupported schema")
    package_source = package.get("source")
    if (not isinstance(package_source, dict) or set(package_source) != build_source_fields
            or package_source != build_source):
        raise ValueError("Linux package manifest source differs from the build manifest identity")
    if package.get("build_manifest_sha256") != manifest["build_manifest_sha256"]:
        raise ValueError("Linux package manifest does not bind the exact public build manifest")
    if package.get("required_glibc") != manifest["minimum_glibc"]:
        raise ValueError("Linux package glibc requirement differs from the release manifest")
    package_executables = package.get("executables")
    if not isinstance(package_executables, list) or len(package_executables) != 3:
        raise ValueError("Linux package manifest omits or adds packaged executable identities")
    packaged_by_name = {}
    for record in package_executables:
        if not isinstance(record, dict) or record.get("name") not in executables:
            raise ValueError("Linux package manifest has an unknown executable identity")
        name = record["name"]
        if name in packaged_by_name:
            raise ValueError("Linux package manifest has duplicate executable identities")
        if (record.get("original_sha256") != executables[name]["sha256"]
                or record.get("packaged_path") != f"bin/{name}"
                or not isinstance(record.get("packaged_sha256"), str)
                or not re.fullmatch(r"[a-f0-9]{64}", record["packaged_sha256"])
                or type(record.get("packaged_bytes")) is not int
                or not 0 < record["packaged_bytes"] <= 512 * 1024 * 1024):
            raise ValueError(f"Linux package manifest has an invalid {name} packaged identity")
        packaged_by_name[name] = record
    if set(packaged_by_name) != set(executables):
        raise ValueError("Linux package manifest omits packaged executable identities")
    for name, record in packaged_by_name.items():
        if (record["packaged_sha256"] != executable_digests[name]["sha256"]
                or record["packaged_bytes"] != executable_digests[name]["bytes"]):
            raise ValueError(f"Linux archived {name} bytes differ from the packaging manifest")

    installer = directory / "install-linux-x64.py"
    channel_installer = directory / "install-linux-x64-channel.py"
    if installer.is_symlink() or not installer.is_file() or channel_installer.is_symlink() or not channel_installer.is_file():
        raise ValueError("Linux candidate is missing its checkpoint or channel installer bootstrap")
    if installer.stat().st_size > 1024 * 1024:
        raise ValueError("Linux checkpoint installer exceeds its source bound")
    try:
        pinned_tag = linux_checkpoint_tag(manifest, installer.read_bytes(), manifest_sha256)
    except ValueError as error:
        raise ValueError(f"Linux checkpoint installer identity is invalid: {error}") from error
    release_tag = package.get("release_tag")
    if release_tag != pinned_tag:
        raise ValueError("Linux package release tag differs from its pinned installer")
    if "checkpoint_shell_asset" in package and package["checkpoint_shell_asset"] != "install-linux-x64.sh":
        raise ValueError("Linux package declares an unknown checkpoint shell asset")
    linux_checkpoint_shell_assets(directory, manifest, required="checkpoint_shell_asset" in package)

    if require_qa:
        qa = read_json(directory / "native-qa-linux-x64.json")
        qa_fields = {"schema", "platform", "archive_sha256", "source_sha", "tested_by", "tested_at", "ubuntu_release", "glibc_version", "cases"}
        if set(qa) != qa_fields or qa["schema"] != 1 or qa["platform"] != "linux-x64":
            raise ValueError("invalid Linux native acceptance record")
        if qa["archive_sha256"] != manifest["sha256"] or qa["source_sha"] != manifest["source_sha"]:
            raise ValueError("Linux native QA does not attest this exact archive and source")
        for name in ("tested_by", "tested_at", "ubuntu_release", "glibc_version"):
            if not isinstance(qa[name], str) or not qa[name]:
                raise ValueError("Linux native QA requires operator, timestamp, Ubuntu release and glibc version")
        if set(qa["cases"]) != LINUX_QA_CASES or any(value is not True for value in qa["cases"].values()):
            raise ValueError("Linux native acceptance has missing or failed cases")
    if source is not None:
        source = Path(source)
        revision = manifest["source_sha"]
        subprocess.run(["git", "-C", str(source), "merge-base", "--is-ancestor", revision, "HEAD"], check=True)
        def blob(path):
            return subprocess.check_output(["git", "-C", str(source), "show", f"{revision}:{path}"])
        if hashlib.sha256(blob("Cargo.lock")).hexdigest() != manifest["cargo_lock_sha256"]:
            raise ValueError("Linux candidate lock differs from selected canonical source")
        tree = subprocess.check_output(["git", "-C", str(source), "rev-parse", revision + "^{tree}"], text=True).strip()
        version = tomllib.loads(blob("Cargo.toml").decode())["workspace"]["package"]["version"]
        if tree != manifest["source_tree"] or version != manifest["version"]:
            raise ValueError("Linux candidate version/tree differs from selected canonical source")
        current = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
        if subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"], text=True).strip():
            raise ValueError("packaging source checkout must be clean")
        expected_installer = subprocess.check_output(["git", "-C", str(source), "show", f"{current}:tools/package/install_linux.py"])
        if channel_installer.read_bytes() != expected_installer:
            raise ValueError("channel installer bootstrap differs from clean packaging source")
        pinned_installer = expected_installer.decode()
        pinned_installer = pinned_installer.replace('PINNED_RELEASE_TAG = ""', f'PINNED_RELEASE_TAG = "{release_tag}"', 1)
        pinned_installer = pinned_installer.replace('PINNED_MANIFEST_SHA256 = ""', f'PINNED_MANIFEST_SHA256 = "{sha256(directory / "release-manifest.json")}"', 1)
        if installer.read_bytes() != pinned_installer.encode():
            raise ValueError("checkpoint installer does not pin this tag and exact manifest bytes")
    return manifest


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class GitHub:
    def __init__(self, repo, token):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) or not token:
            raise ValueError("release publishing needs a repository and dedicated token")
        self.repo, self.token = repo, token

    def api(self, path, method="GET", body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request("https://api.github.com/repos/" + self.repo + path, data=data, method=method,
            headers={"Authorization": "Bearer " + self.token, "Accept": "application/vnd.github+json", "Content-Type": "application/json", "X-GitHub-Api-Version": "2022-11-28"})
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.load(response)

    def upload(self, release_id, path, name=None):
        path = Path(path)
        connection = http.client.HTTPSConnection("uploads.github.com", timeout=120)
        endpoint = f"/repos/{self.repo}/releases/{release_id}/assets?name=" + urllib.parse.quote(name or path.name)
        try:
            connection.putrequest("POST", endpoint)
            connection.putheader("Authorization", "Bearer " + self.token)
            connection.putheader("Content-Type", "application/octet-stream")
            connection.putheader("Content-Length", str(path.stat().st_size))
            connection.endheaders()
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    connection.send(chunk)
            response = connection.getresponse()
            if response.status != 201:
                raise ValueError(f"GitHub asset upload failed: HTTP {response.status}")
            return json.load(response)
        finally:
            connection.close()

    def asset_response(self, asset):
        request = urllib.request.Request(f'https://api.github.com/repos/{self.repo}/releases/assets/{asset["id"]}',
            headers={"Authorization": "Bearer " + self.token, "Accept": "application/octet-stream"})
        try:
            return urllib.request.build_opener(NoRedirect).open(request, timeout=120)
        except urllib.error.HTTPError as error:
            if error.code != 302:
                raise
            location = error.headers.get("Location", "")
            error.close()
            if urllib.parse.urlparse(location).scheme != "https":
                raise ValueError("GitHub returned a non-HTTPS asset location")
            # Never forward a private-repository token to the download CDN.
            return urllib.request.urlopen(location, timeout=120)

    def asset_hash(self, asset):
        digest = hashlib.sha256()
        with self.asset_response(asset) as response:
            for chunk in iter(lambda: response.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()

    def asset_json(self, asset):
        with self.asset_response(asset) as response:
            data = response.read(65537)
        if len(data) > 65536:
            raise ValueError("published release evidence exceeds size limit")
        return json.loads(data)

    def find_release(self, tag):
        try:
            return self.api("/releases/tags/" + urllib.parse.quote(tag, safe=""))
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
            error.close()
        # GitHub's tag endpoint finds published releases. Include drafts when
        # retrying staging or a partially uploaded stable release.
        matches = []
        for page in range(1, 101):
            releases = self.api(f"/releases?per_page=100&page={page}")
            matches.extend(release for release in releases if release["tag_name"] == tag)
            if len(releases) < 100:
                break
        else:
            raise ValueError("release inventory exceeds lookup limit; refusing to create a duplicate")
        if len(matches) > 1:
            raise ValueError("multiple draft releases have this tag; inspect before retrying")
        return matches[0] if matches else None

    def publish(self, directory, tag, stable, platform=None, draft_only=False, *, source=None):
        if source is None:
            raise ValueError("release publication requires the canonical source checkout")
        directory = Path(directory)
        manifest = validate(directory, platform=platform, require_qa=not draft_only)
        selected = manifest["platform"]
        if stable and draft_only:
            raise ValueError("a stable release cannot bypass native QA")
        if stable and tag != "v" + manifest["version"]:
            raise ValueError("stable tag does not match candidate version")
        if not stable and not re.fullmatch(re.escape("v" + manifest["version"]) + r"-rc\.[0-9]+-" + re.escape(selected), tag):
            legacy = selected == "linux-x64" and "minimum_glibc" in manifest
            if not legacy:
                raise ValueError("candidate tag must be v<version>-rc.<number>-<platform>")
            with tarfile.open(directory / manifest["asset"], "r:gz") as bundle:
                package = json.load(bundle.extractfile("nudox-linux-x86_64/packaging-manifest.json"))
            if tag != package.get("release_tag") and not re.fullmatch(re.escape("v" + manifest["version"]) + r"-rc\.[0-9]+", tag):
                raise ValueError("candidate tag differs from its immutable Linux checkpoint")
        shell_assets = []
        pinned_installer = None
        if selected == "linux-x64" and not stable:
            checkpoint = linux_checkpoint_shell_assets(directory, manifest)
            if checkpoint is not None:
                pinned_tag, pinned_installer, shell_assets = checkpoint
                if tag != pinned_tag:
                    raise ValueError("Linux checkpoint shell can only publish under its exact pinned checkpoint tag")
        require_published_source_tag(source, tag, manifest["source_sha"], self)
        release = self.find_release(tag)
        if release is None:
            release = self.api("/releases", "POST", {"tag_name": tag, "name": "NuDox " + tag, "draft": True, "prerelease": not stable, "make_latest": "false", "body": f'Verified native artifacts. Canonical Forgejo Backend source: {manifest["source_sha"]}. The GitHub tag is a distribution identifier.'})
        source_marker = re.search(r"Canonical Forgejo Backend source: ([a-f0-9]{40})", release.get("body", ""))
        if source_marker and source_marker.group(1) != manifest["source_sha"]:
            raise ValueError("all platforms in one release version must use the same source revision")
        if release["assets"] and not source_marker and not any(asset["name"].startswith("release-manifest-") for asset in release["assets"]):
            raise ValueError("existing release has no source contract; use a new version")
        assets = {asset["name"]: asset for asset in release["assets"]}
        if not release["draft"] and bool(release.get("prerelease")) == stable:
            raise ValueError("published tag has the wrong stable/prerelease status; use a new version")
        for name, asset in assets.items():
            if name.startswith("release-manifest-") and name.endswith(".json"):
                existing = self.asset_json(asset)
                if existing.get("source_sha") != manifest["source_sha"] or existing.get("version") != manifest["version"]:
                    raise ValueError("all platforms in one release version must use the same source revision")
        self.release_url = release.get("html_url", f"https://github.com/{self.repo}/releases")
        kinds = ("release-manifest",) if draft_only else ("release-manifest", "native-qa")
        if read_ci_waiver(directory, manifest) is not None:
            kinds += ("ci-waiver",)
        names = [manifest["asset"], manifest["asset"] + ".sha256"] + [f"{kind}-{selected}.json" for kind in kinds]
        with tempfile.TemporaryDirectory(prefix="nudox-release-evidence-") as temporary:
            staging = Path(temporary)
            for kind in kinds:
                (staging / f"{kind}-{selected}.json").write_bytes(metadata_path(directory, kind, selected).read_bytes())
            paths = [directory / name for name in names[:2]] + [staging / name for name in names[2:]]
            if selected == "linux-x64":
                bootstrap_name = "install-linux-x64-channel.py" if stable else "install-linux-x64.py"
                bootstrap = directory / bootstrap_name
                if bootstrap.is_symlink() or not bootstrap.is_file() or bootstrap.stat().st_size > 1024 * 1024:
                    raise ValueError("Linux release is missing its bounded regular installer bootstrap")
                published_bootstrap = staging / "install-linux-x64.py"
                published_bootstrap.write_bytes(pinned_installer if pinned_installer is not None else bootstrap.read_bytes())
                paths.append(published_bootstrap)
                for name, admitted_bytes in shell_assets:
                    staged_shell = staging / name
                    staged_shell.write_bytes(admitted_bytes)
                    paths.append(staged_shell)
            self.publish_assets(release, paths, assets, stable)
        if release["draft"] and not draft_only:
            # Uploads can outlive the initial observation. Re-admit both refs
            # immediately before making the draft public; never repair refs here.
            require_published_source_tag(source, tag, manifest["source_sha"], self)
            self.api(f'/releases/{release["id"]}', "PATCH", {"draft": False, "prerelease": not stable, "make_latest": "false"})
        return manifest

    def publish_assets(self, release, paths, assets, stable):
        for path in paths:
            name = path.name
            expected = sha256(path)
            if name in assets:
                if self.asset_hash(assets[name]) != expected:
                    raise ValueError(f"refusing to overwrite immutable asset {name} in {release["tag_name"]}")
            elif release["draft"] or stable:
                uploaded = self.upload(release["id"], path)
                if self.asset_hash(uploaded) != expected:
                    raise ValueError("uploaded release bytes failed read-back verification")
            else:
                raise ValueError("published release is missing an expected asset; use a new version")


def require_published_source_tag(source, tag, source_sha, github):
    """Require the canonical Forgejo tag and GitHub distribution tag to pin source_sha."""
    if not re.fullmatch(r"[A-Za-z0-9_.-]+", tag) or not re.fullmatch(r"[a-f0-9]{40}", source_sha):
        raise ValueError("release tag or source revision is invalid")

    source = Path(source)
    try:
        origin = subprocess.check_output(
            ["git", "-C", str(source), "remote", "get-url", "origin"],
            text=True,
            stderr=subprocess.PIPE,
            timeout=30,
        ).strip()
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        raise ValueError("could not verify the canonical Forgejo source remote; git failed or exceeded 30 seconds") from error
    if not _is_canonical_forgejo_origin(origin):
        raise ValueError("release source origin is not the canonical Forgejo Backend repository")
    try:
        refs = subprocess.check_output(
            ["git", "-C", str(source), "ls-remote", "--exit-code", "origin",
             f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"],
            text=True,
            stderr=subprocess.PIPE,
            timeout=30,
        )
    except subprocess.TimeoutExpired as error:
        raise ValueError("canonical Forgejo tag lookup exceeded 30 seconds; release publication refused") from error
    except (OSError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError) and error.returncode == 2:
            command = (
                f"git -C {shlex.quote(str(source))} tag {tag} {source_sha} && "
                f"git -C {shlex.quote(str(source))} push origin refs/tags/{tag}"
            )
            raise ValueError(
                f"canonical Forgejo tag {tag} is absent. After verifying the exact source, "
                f"create and push it explicitly with: {command}"
            ) from error
        raise ValueError("could not read the canonical Forgejo tag; release publication refused") from error
    forgejo_objects = {}
    for line in refs.splitlines():
        fields = line.split("\t", 1)
        if len(fields) != 2 or fields[1] not in {f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"}:
            raise ValueError("canonical Forgejo tag lookup returned malformed or unexpected refs")
        if not re.fullmatch(r"[a-f0-9]{40}", fields[0]) or fields[1] in forgejo_objects:
            raise ValueError("canonical Forgejo tag lookup returned malformed or duplicate refs")
        forgejo_objects[fields[1]] = fields[0]
    if f"refs/tags/{tag}" not in forgejo_objects:
        raise ValueError("canonical Forgejo tag lookup omitted the requested base ref")
    forgejo_sha = forgejo_objects.get(f"refs/tags/{tag}^{{}}", forgejo_objects[f"refs/tags/{tag}"])
    if forgejo_sha != source_sha:
        raise ValueError("canonical Forgejo release tag does not resolve to the candidate source revision")

    reference = github.api("/git/ref/tags/" + urllib.parse.quote(tag, safe=""))
    if not isinstance(reference, dict) or reference.get("ref") != f"refs/tags/{tag}":
        raise ValueError("GitHub distribution tag reference is missing or does not match")
    target = reference.get("object", {})
    for _ in range(8):
        if not isinstance(target, dict):
            raise ValueError("GitHub distribution tag has a malformed target")
        object_type = target.get("type")
        object_sha = target.get("sha")
        if not isinstance(object_sha, str) or not re.fullmatch(r"[a-f0-9]{40}", object_sha):
            raise ValueError("GitHub distribution tag has an invalid target")
        if object_type == "commit":
            if object_sha != source_sha:
                raise ValueError("GitHub distribution tag does not resolve to the candidate source revision")
            return
        if object_type != "tag":
            raise ValueError("GitHub distribution tag does not resolve to a commit")
        annotated = github.api("/git/tags/" + object_sha)
        if not isinstance(annotated, dict):
            raise ValueError("GitHub annotated tag response is malformed")
        target = annotated.get("object", {})
    raise ValueError("GitHub distribution tag has too many annotated-tag layers")


def _is_canonical_forgejo_origin(origin):
    """Accept only the canonical web clone URL and configured Forgejo SSH URL."""
    if re.fullmatch(r"forgejo@dev\.nudox\.org:Nudox/Backend\.git", origin):
        return True
    try:
        parsed = urllib.parse.urlsplit(origin)
        port = parsed.port
    except ValueError:
        return False
    if "?" in origin or "#" in origin or parsed.query or parsed.fragment or parsed.password is not None:
        return False
    if parsed.scheme == "https":
        return (
            parsed.hostname == "dev.nudox.org"
            and parsed.username is None
            and port in {None, 443}
            and parsed.path == "/git/Nudox/Backend.git"
        )
    if parsed.scheme == "ssh":
        return (
            parsed.hostname == "dev.nudox.org"
            and parsed.username == "forgejo"
            and port == 2222
            and parsed.path == "/Nudox/Backend.git"
        )
    return False


def check_fast(revision, platform="macos", waiver=None):
    """Require each CI context of the platform to pass, unless the waiver names it.

    Returns the waived contexts that were not green.
    """
    waived = set(waiver["contexts"]) if waiver else set()
    token = os.environ.get("FORGEJO_TOKEN", "")
    if not token:
        raise ValueError("fast-gate verification requires Forgejo read access")
    endpoint = f"https://dev.nudox.org/api/v1/repos/Nudox/Backend/statuses/{revision}?limit=100"
    request = urllib.request.Request(endpoint, headers={"Authorization": "token " + token})
    with urllib.request.urlopen(request, timeout=30) as response:
        statuses = json.load(response)
    red = []
    for context in PLATFORMS[platform]["ci"]:
        matches = [entry for entry in statuses if entry["context"] == context]
        if not matches or max(matches, key=lambda entry: entry["id"])["status"] != "success":
            if context not in waived:
                raise ValueError(f"selected source revision has no passing {context} status")
            red.append(context)
    if red:
        print(f"release CI waiver: releasing {revision} with {', '.join(red)} not green: {waiver['reason']}", file=sys.stderr)
    return red


def promote(manifest, host, key, known_hosts):
    command = ["ssh", "-i", str(key), "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "StrictHostKeyChecking=yes", "-o", "UserKnownHostsFile=" + str(known_hosts), "-o", "ConnectTimeout=10", host]
    def request(data):
        result = subprocess.run(command, input=json.dumps(data), text=True, stdout=subprocess.PIPE, check=True)
        return json.loads(result.stdout)
    current = request({"action": "show"})
    if public_metadata() != current["catalog"]:
        raise ValueError("public auth release metadata does not match the active channel catalog; deploy routing first")
    channel = {name: manifest[name] for name in ("version", "source_sha", "asset", "sha256")}
    for optional in ("minimum_os", "minimum_glibc"):
        if optional in manifest:
            channel[optional] = manifest[optional]
    channel.update(tag="v" + manifest["version"], status="available")
    updated = request({"action": "promote", "expected_sha256": current["sha256"], "platform": manifest["platform"], "channel": channel})
    for platform, before in current["catalog"]["platforms"].items():
        if platform != manifest["platform"] and updated["catalog"]["platforms"][platform] != before:
            raise ValueError("promotion changed an unrelated platform")
    try:
        verify_public_download(manifest, updated["catalog"])
    except (ValueError, OSError) as error:
        try:
            restored = request({"action": "rollback", "expected_sha256": updated["sha256"], "platform": manifest["platform"], "history_sha256": current["sha256"]})
            if restored["catalog"] != current["catalog"] or public_metadata() != current["catalog"]:
                raise ValueError("rollback verification failed")
        except (ValueError, OSError, subprocess.CalledProcessError) as rollback_error:
            raise ValueError("public download verification failed; automatic rollback also failed, inspect the active catalog") from rollback_error
        raise ValueError("public download verification failed; previous platform channel restored") from error
    return updated["sha256"]


def public_metadata(require_versioned=False):
    with urllib.request.urlopen("https://api.nudox.org/v1/releases", timeout=30) as response:
        if require_versioned and response.headers.get("X-Nudox-Versioned-Downloads") != "1":
            raise ValueError("deploy the versioned Auth download route before publishing")
        data = response.read(65537)
    if len(data) > 65536:
        raise ValueError("public release metadata exceeds size limit")
    return json.loads(data)


def verify_public_download(manifest, expected_catalog):
    if public_metadata() != expected_catalog:
        raise ValueError("public metadata did not refresh to promoted release")
    urls = [f'https://api.nudox.org/v1/downloads/{manifest["platform"]}', versioned_download_url(manifest)]
    for url in urls:
        digest = hashlib.sha256()
        with urllib.request.urlopen(url, timeout=120) as response:
            for chunk in iter(lambda: response.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != manifest["sha256"]:
            raise ValueError("public download differs from accepted release archive")


def versioned_download_url(manifest):
    return f'https://api.nudox.org/v1/downloads/{manifest["platform"]}/{manifest["version"]}/{manifest["sha256"]}'


def homebrew_cask(manifest):
    if manifest["platform"] != "macos":
        raise ValueError("Homebrew cask requires a macOS candidate")
    major = int(manifest["minimum_os"].split(".")[0])
    releases = {11: "big_sur", 12: "monterey", 13: "ventura", 14: "sonoma", 15: "sequoia", 26: "tahoe"}
    if major not in releases:
        raise ValueError("add the supported macOS release to the Homebrew cask mapping")
    return f'''cask "nudox" do
  version "{manifest["version"]}"
  sha256 "{manifest["sha256"]}"

  url "{versioned_download_url(manifest)}"
  container type: :zip
  name "Nudox"
  desc "Local-first code intelligence"
  homepage "https://nudox.org"

  depends_on arch: :arm64
  depends_on macos: :{releases[major]}
  app "Nudox.app"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-cli", target: "nudox"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-mcp"
  binary "#{{appdir}}/Nudox.app/Contents/MacOS/nudox-locald"

  caveats "Language indexing requires the corresponding native compiler/toolchain."
end
'''


def update_homebrew(manifest):
    channel = public_metadata()["platforms"][manifest["platform"]]
    expected = {name: manifest[name] for name in ("version", "source_sha", "asset", "sha256", "minimum_os")}
    if channel != {**expected, "tag": "v" + manifest["version"], "status": "available"}:
        raise ValueError("Homebrew update requires this exact artifact to be the current promoted release")
    content = homebrew_cask(manifest).encode()
    github = GitHub("nudoxorg/homebrew-tap", os.environ.get("NUDOX_HOMEBREW_TOKEN", ""))
    path = "/contents/Casks/nudox.rb"
    body = {"message": f'Publish Nudox {manifest["version"]} ({manifest["source_sha"][:12]})',
            "content": base64.b64encode(content).decode(), "branch": "main"}
    try:
        current = github.api(path + "?ref=main")
        if base64.b64decode(current["content"]) == content:
            return
        body["sha"] = current["sha"]
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
    github.api(path, "PUT", body)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "cask", "stage", "submit", "candidate", "publish", "homebrew"))
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--check-fast", "--check-ci", dest="check_fast", action="store_true", help="check exact source CI: fast plus the selected platform lane")
    parser.add_argument("--repo", default="nudoxorg/releases")
    parser.add_argument("--candidate-tag")
    parser.add_argument("--rc", type=int)
    parser.add_argument("--platform", choices=PLATFORMS)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--local-preview", action="store_true", help="generate a QA-only cask using this local archive")
    parser.add_argument("--promotion-host")
    parser.add_argument("--promotion-key", type=Path)
    parser.add_argument("--known-hosts", type=Path)
    args = parser.parse_args()
    mutating = args.action not in {"validate", "cask"}
    if mutating and args.source is None:
        raise ValueError("submission/publication requires the canonical source checkout")
    manifest = validate(args.candidate, args.source, args.platform, require_qa=args.action not in {"stage", "cask"})
    waiver = read_ci_waiver(args.candidate, manifest)
    waived = []
    if args.check_fast or mutating:
        waived = check_fast(manifest["source_sha"], manifest["platform"], waiver)
    if args.action == "cask":
        if args.output is None:
            raise ValueError("cask generation requires --output")
        content = homebrew_cask(manifest)
        if args.local_preview:
            content = content.replace(versioned_download_url(manifest), (args.candidate / manifest["asset"]).resolve().as_uri())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        # Never accidentally overwrite an existing recipe during acceptance.
        with args.output.open("x") as stream:
            stream.write(content)
    elif args.action == "homebrew":
        update_homebrew(manifest)
    elif args.action != "validate":
        github = GitHub(args.repo, os.environ.get("NUDOX_RELEASE_TOKEN", ""))
        if args.action in {"candidate", "stage", "submit"}:
            if args.action in {"stage", "submit"}:
                if args.rc is None or args.rc < 1:
                    raise ValueError("stage/submit requires --rc with a positive candidate number")
                args.candidate_tag = f'v{manifest["version"]}-rc.{args.rc}-{manifest["platform"]}'
            if not args.candidate_tag:
                raise ValueError("candidate staging requires --candidate-tag")
            github.publish(args.candidate, args.candidate_tag, False, args.platform, draft_only=args.action == "stage", source=args.source)
        else:
            if not all((args.promotion_host, args.promotion_key, args.known_hosts)):
                raise ValueError("publishing requires channel promotion credentials and pinned SSH host key")
            metadata = public_metadata(require_versioned=True)
            if metadata.get("schema") != 1 or set(metadata.get("platforms", {})) != {"macos", "linux-x64", "linux-arm64", "windows"}:
                raise ValueError("platform-specific auth routing must be deployed before publishing")
            github.publish(args.candidate, "v" + manifest["version"], True, args.platform, source=args.source)
            promote(manifest, args.promotion_host, args.promotion_key, args.known_hosts)
    result = {"action": args.action, "version": manifest["version"], "source_sha": manifest["source_sha"], "sha256": manifest["sha256"]}
    if waived:
        result["ci_waived"] = waived
    if args.action == "stage":
        result["preview_url"] = github.release_url
    print(json.dumps(result))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, zipfile.BadZipFile, tarfile.TarError, subprocess.CalledProcessError) as error:
        if isinstance(error, urllib.error.HTTPError):
            message = f"release service returned HTTP {error.code}"
        else:
            message = str(error)
        print("release refused: " + message, file=sys.stderr)
        sys.exit(1)
