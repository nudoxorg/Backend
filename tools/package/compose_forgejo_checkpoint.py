#!/usr/bin/env python3
"""Assemble a source- and digest-bound diagnostic Forgejo checkpoint bundle.

This prepares local candidate assets only. It never uploads or promotes them.
"""
from __future__ import annotations

import argparse
import ast
from collections.abc import Callable
from dataclasses import dataclass
import hashlib
import datetime
import tempfile
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tarfile
import types

import install_forgejo_preview as bootstrap
import release_contract as release


MAX_JSON = 65536
MAX_SCRIPT = 1024 * 1024
EXPECTED_PLATFORMS = ("linux-x64", "macos-arm64")
LINUX_LIBRARY_NAME = "install-linux-x64"
MAC_LIBRARY_NAME = "install-macos-arm64"
LINUX_BOOTSTRAP_ASSET = "install-linux-x64.py"
NATIVE_QA_NAME = "native-qa-linux-x64.json"
NATIVE_QA_FIELDS = {
    "schema", "platform", "archive_sha256", "source_sha", "tested_by",
    "tested_at", "ubuntu_release", "glibc_version", "cases",
}


class CandidateError(ValueError):
    """The selected source or package cannot form one coherent channel."""


@dataclass(frozen=True)
class SourceSnapshot:
    root: Path
    revision: str
    tree: str
    bootstrap_template: bytes
    mac_library: bytes
    launcher_template: bytes
    base_channel: dict
    base_channel_sha256: str


@dataclass(frozen=True)
class PlatformLibrary:
    platform: str
    asset_stem: str
    body: bytes

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.body).hexdigest()

    @property
    def byte_count(self) -> int:
        return len(self.body)

    @property
    def bootstrap_pin(self) -> tuple[str, int, str]:
        return self.sha256, self.byte_count, self.asset_stem


@dataclass(frozen=True)
class LinuxPackage:
    directory: Path
    manifest: dict
    manifest_bytes: bytes
    manifest_sha256: str
    tag: str
    archive: Path
    archive_sha256: str
    archive_bytes: int
    installer: bytes
    installer_source_template: bytes
    native_qa_template_sha256: str
    native_qa_limitation: str


@dataclass(frozen=True)
class MacSdkPackage:
    directory: Path
    manifest: dict
    manifest_bytes: bytes
    archive: Path
    tag: str


@dataclass(frozen=True)
class CandidateBundle:
    bootstrap_tag: str
    bootstrap: bytes
    launcher: bytes
    channel: dict
    libraries: dict[str, PlatformLibrary]
    linux_package: LinuxPackage
    source: SourceSnapshot
    mac_package: MacSdkPackage | None = None


def digest(body: bytes) -> str:
    return hashlib.sha256(body).hexdigest()


def git(root: Path, *args: str) -> bytes:
    try:
        return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE)
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode("utf-8", "replace").strip()
        raise CandidateError(f"git source inspection failed: {detail or 'unknown error'}") from error


def source_file(root: Path, revision: str, relative: str, limit: int) -> bytes:
    body = git(root, "show", f"{revision}:{relative}")
    if len(body) > limit:
        raise CandidateError(f"source file exceeds its {limit}-byte limit: {relative}")
    return body


def checked_json(body: bytes, label: str) -> dict:
    if len(body) > MAX_JSON:
        raise CandidateError(f"{label} exceeds the {MAX_JSON}-byte limit")
    try:
        result = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CandidateError(f"{label} is not valid JSON: {error}") from error
    if not isinstance(result, dict):
        raise CandidateError(f"{label} must be a JSON object")
    return result


def read_regular(path: Path, limit: int, label: str) -> bytes:
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        raise CandidateError(f"cannot open {label} as a regular file: {error}") from error
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise CandidateError(f"{label} is not a bounded regular file")
        body = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        try:
            current = path.lstat()
        except OSError as error:
            raise CandidateError(f"{label} changed while being read") from error
        identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if (len(body) > limit or len(body) != before.st_size or identity(before) != identity(after)
                or identity(after) != identity(current)):
            raise CandidateError(f"{label} changed or exceeded its read bound")
        return body


def top_level_assignments(source: bytes, names: set[str]) -> dict[str, ast.AST]:
    try:
        tree = ast.parse(source)
    except (SyntaxError, UnicodeDecodeError) as error:
        raise CandidateError(f"selected Python source does not parse: {error}") from error
    found: dict[str, ast.AST] = {}
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        if len(node.targets) != 1 or not isinstance(node.targets[0], ast.Name):
            continue
        name = node.targets[0].id
        if name in names:
            if name in found:
                raise CandidateError(f"source assigns {name} more than once")
            found[name] = node.value
    missing = names - set(found)
    if missing:
        raise CandidateError("selected Python source omits required assignments: " + ", ".join(sorted(missing)))
    return found


def literal_assignments(source: bytes, names: set[str]) -> dict[str, object]:
    nodes = top_level_assignments(source, names)
    try:
        values = {name: ast.literal_eval(node) for name, node in nodes.items()}
    except (ValueError, TypeError) as error:
        raise CandidateError("selected Python source has a nonliteral release identity") from error
    return values


def function_names(source: bytes) -> set[str]:
    try:
        tree = ast.parse(source)
    except (SyntaxError, UnicodeDecodeError) as error:
        raise CandidateError(f"platform library does not parse: {error}") from error
    return {node.name for node in tree.body if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))}


def verify_library_contract(body: bytes, platform: str) -> None:
    constants = literal_assignments(body, {"PLATFORM"})
    if constants["PLATFORM"] != platform:
        raise CandidateError(f"{platform} library declares a different platform")
    methods = function_names(body)
    if platform == "macos-arm64":
        required = {"install", "verify_existing_install", "reuse_existing_install"}
    else:
        required = {"install", "verify_existing_install"}
    if not required.issubset(methods):
        raise CandidateError(f"{platform} library omits required installer operations")


def selected_source_snapshot(source_root: Path) -> SourceSnapshot:
    root = source_root.resolve(strict=True)
    if not root.is_dir():
        raise CandidateError("source root must be a directory")
    revision = git(root, "rev-parse", "HEAD").decode().strip()
    tree = git(root, "rev-parse", "HEAD^{tree}").decode().strip()
    if not re.fullmatch(r"[a-f0-9]{40}", revision) or not re.fullmatch(r"[a-f0-9]{40}", tree):
        raise CandidateError("source checkout has an invalid Git identity")
    if git(root, "status", "--porcelain").strip():
        raise CandidateError("source checkout must be clean before candidate assembly")

    bootstrap_template = source_file(root, revision, "tools/package/install_forgejo_preview.py", MAX_SCRIPT)
    mac_library = source_file(root, revision, "tools/package/install_macos_preview.py", MAX_SCRIPT)
    launcher_template = source_file(root, revision, "tools/package/install_diagnostic_preview.sh", MAX_SCRIPT)
    channel_bytes = source_file(root, revision, "tools/package/forgejo-preview-channel.json", MAX_JSON)
    channel = checked_json(channel_bytes, "base diagnostic channel")
    historical_path = root / "tools/package/fixtures/install-forgejo-bootstrap-r2-pinned.py"
    historical_bytes = source_file(root, revision, "tools/package/fixtures/install-forgejo-bootstrap-r2-pinned.py", MAX_SCRIPT)
    if read_regular(historical_path, MAX_SCRIPT, "historical R2 bootstrap") != historical_bytes:
        raise CandidateError("historical R2 channel validator differs from the selected clean source")
    historical = types.ModuleType("nudox_r2_channel_validator")
    exec(compile(historical_bytes, str(historical_path), "exec"), historical.__dict__)
    try:
        historical.validate_channel(channel)
        historical.admit_libraries(channel)
    except (historical.InstallError, KeyError, TypeError, ValueError) as error:
        raise CandidateError(f"base channel is not the exact reviewed R2 channel: {error}") from error
    verify_library_contract(mac_library, "macos-arm64")
    return SourceSnapshot(root, revision, tree, bootstrap_template, mac_library, launcher_template, channel,
                          digest(channel_bytes))


def linux_checkpoint_tag(manifest: dict, installer: bytes, manifest_sha256: str) -> str:
    try:
        return release.linux_checkpoint_tag(manifest, installer, manifest_sha256)
    except ValueError as error:
        raise CandidateError(str(error)) from error


def uncompleted_linux_qa(package_dir: Path) -> tuple[str, str]:
    path = package_dir / NATIVE_QA_NAME
    body = read_regular(path, MAX_JSON, "Linux native QA template")
    qa = checked_json(body, "Linux native QA template")
    if set(qa) != NATIVE_QA_FIELDS or qa.get("schema") != 1 or qa.get("platform") != "linux-x64":
        raise CandidateError("Linux native QA template has an unsupported schema")
    placeholders = {
        "archive_sha256": "replace-with-the-release-manifest-sha256",
        "source_sha": "replace-with-the-release-source-commit",
    }
    if any(qa.get(field) != value for field, value in placeholders.items()):
        raise CandidateError("Linux native QA is populated; this composer only admits an explicit uncompleted template")
    if any(qa.get(field) != "" for field in ("tested_by", "tested_at", "ubuntu_release", "glibc_version")):
        raise CandidateError("Linux native QA includes a partial operator or host claim")
    cases = qa.get("cases")
    if not isinstance(cases, dict) or set(cases) != release.LINUX_QA_CASES or any(value is not False for value in cases.values()):
        raise CandidateError("Linux native QA is not the exact all-uncompleted template")
    names = ", ".join(sorted(cases))
    return digest(body), (
        "No native Linux runtime acceptance is claimed for this release candidate; "
        f"its native QA template marks these cases uncompleted: {names}."
    )


def candidate_launcher(template: bytes, bootstrap_tag: str, bootstrap_body: bytes) -> bytes:
    """Pin the selected immutable bootstrap into the reviewed POSIX launcher source."""
    if not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", bootstrap_tag):
        raise CandidateError("candidate bootstrap tag is invalid for the POSIX launcher")
    try:
        text = template.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CandidateError("POSIX launcher source is not UTF-8") from error
    bootstrap_sha256 = digest(bootstrap_body)
    bootstrap_asset = f"install-forgejo-bootstrap-{bootstrap_sha256[:16]}.py"
    bootstrap_url = bootstrap.release_url(bootstrap_tag, bootstrap_asset)
    old_description = (
        "# Download and execute the immutable R2 Python bootstrap as a saved file.\n"
        "# This delivery repair still installs the October 6/8 diagnostic binaries."
    )
    replacements = {
        old_description: "# Download and execute this immutable diagnostic bootstrap as a saved file.",
        "'Supports macOS arm64 and Linux x86_64; installs the historical R2 diagnostic preview.'":
            "'Supports macOS arm64 and Linux x86_64; installs this pinned diagnostic preview.'",
        "installer_url='https://dev.nudox.org/git/philocalyst/Backend/releases/download/"
        "nudox-diagnostic-installer-20261009-r2/install.py'": f"installer_url='{bootstrap_url}'",
        "installer_sha256='36f0ea62e3b49cadebcef0f89e735639530c0ba1686e62d4dcbfea96294174f5'":
            f"installer_sha256='{bootstrap_sha256}'",
        "installer_bytes=13426": f"installer_bytes={len(bootstrap_body)}",
        "printf '%s\\n' 'Downloading the pinned R2 diagnostic installer (historical October 6/8 binaries).' >&2":
            f"printf 'Downloading pinned diagnostic bootstrap for %s.\\n' '{bootstrap_tag}' >&2",
    }
    for old, new in replacements.items():
        if text.count(old) != 1:
            raise CandidateError("POSIX launcher source has changed its reviewed pin anchors")
        text = text.replace(old, new, 1)
    body = text.encode("utf-8")
    if (len(body) > MAX_SCRIPT or "historical October 6/8" in text
            or "nudox-diagnostic-installer-20261009-r2" in text):
        raise CandidateError("generated POSIX launcher is incomplete or exceeds its byte limit")
    return body


def linux_failures_for_candidate(base_row: dict, package: LinuxPackage) -> list[str]:
    """Retain detailed failures only when the source and exact archive are unchanged."""
    archive = base_row.get("archive") if isinstance(base_row, dict) else None
    if not isinstance(archive, dict):
        return []
    same_artifact = (
        base_row.get("source") == package.manifest.get("source_sha")
        and archive.get("sha256") == package.archive_sha256
        and archive.get("bytes") == package.archive_bytes
    )
    if not same_artifact:
        return []
    failures = base_row.get("known_failures")
    if not isinstance(failures, list) or any(not isinstance(item, str) or not item.strip() for item in failures):
        raise CandidateError("matching base Linux artifact has invalid detailed failure notes")
    return list(failures)


def pin_linux_installer(template: bytes, tag: str, manifest_sha256: str) -> bytes:
    try:
        text = template.decode("utf-8")
    except UnicodeDecodeError as error:
        raise CandidateError("Linux installer source template is not UTF-8") from error
    replacements = {
        'PINNED_RELEASE_TAG = ""': f'PINNED_RELEASE_TAG = "{tag}"',
        'PINNED_MANIFEST_SHA256 = ""': f'PINNED_MANIFEST_SHA256 = "{manifest_sha256}"',
    }
    for old, new in replacements.items():
        if text.count(old) != 1:
            raise CandidateError("Linux installer source has changed its immutable pin anchors")
        text = text.replace(old, new, 1)
    return text.encode("utf-8")


def validate_linux_package(directory: Path, source: SourceSnapshot) -> LinuxPackage:
    raw_directory = Path(directory)
    # Reject a symlink used as the package root itself; parent path aliases are canonicalized below.
    if raw_directory.is_symlink():
        raise CandidateError("Linux package input must not be a symlink")
    package_dir = raw_directory.resolve(strict=True)
    if not package_dir.is_dir():
        raise CandidateError("Linux package input must be a real directory")
    manifest_path = package_dir / "release-manifest.json"
    manifest_bytes = read_regular(manifest_path, MAX_JSON, "Linux release manifest")
    manifest = checked_json(manifest_bytes, "Linux release manifest")
    try:
        release.validate_linux(package_dir, manifest, source.root, require_qa=False,
                               manifest_sha256=hashlib.sha256(manifest_bytes).hexdigest())
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        raise CandidateError(f"Linux package or selected source failed validation: {error}") from error
    installer = read_regular(package_dir / LINUX_BOOTSTRAP_ASSET, MAX_SCRIPT, "pinned Linux installer")
    manifest_sha = digest(manifest_bytes)
    tag = linux_checkpoint_tag(manifest, installer, manifest_sha)
    installer_source = source_file(source.root, manifest["source_sha"],
                                   "tools/package/install_linux.py", MAX_SCRIPT)
    if pin_linux_installer(installer_source, tag, manifest_sha) != installer:
        raise CandidateError("pinned Linux installer is not generated from the manifest source commit")
    qa_digest, qa_limitation = uncompleted_linux_qa(package_dir)
    archive = package_dir / manifest["asset"]
    return LinuxPackage(package_dir, manifest, manifest_bytes, manifest_sha, tag, archive,
                        manifest["sha256"], manifest["size_bytes"], installer,
                        installer_source, qa_digest, qa_limitation)


def replace_bootstrap_identity(template: bytes, *, bootstrap_tag: str, channel_url: str,
                               libraries: dict[str, PlatformLibrary]) -> bytes:
    names = {"BOOTSTRAP_TAG", "CHANNEL", "LIBRARIES"}
    nodes = top_level_assignments(template, names)
    if set(libraries) != set(EXPECTED_PLATFORMS):
        raise CandidateError("candidate must provide exactly Linux x64 and macOS arm64 libraries")
    replacement_values = {
        "BOOTSTRAP_TAG": repr(bootstrap_tag),
        "CHANNEL": repr(channel_url),
        "LIBRARIES": "{\n" + "".join(
            f"    {platform!r}: {libraries[platform].bootstrap_pin!r},\n"
            for platform in EXPECTED_PLATFORMS
        ) + "}",
    }
    try:
        lines = template.decode("utf-8").splitlines(keepends=True)
    except UnicodeDecodeError as error:
        raise CandidateError("bootstrap template is not UTF-8") from error
    assignments: list[tuple[int, int, str]] = []
    parsed = ast.parse(template)
    for name in nodes:
        assignment = next(node for node in parsed.body
                          if isinstance(node, ast.Assign) and len(node.targets) == 1
                          and isinstance(node.targets[0], ast.Name) and node.targets[0].id == name)
        if assignment.lineno <= 0 or assignment.end_lineno is None:
            raise CandidateError(f"cannot locate bootstrap identity assignment: {name}")
        replacement = f"{name} = {replacement_values[name]}\n"
        assignments.append((assignment.lineno - 1, assignment.end_lineno, replacement))
    for start, end, replacement in sorted(assignments, reverse=True):
        lines[start:end] = [replacement]
    result = "".join(lines).encode("utf-8")
    if len(result) > MAX_SCRIPT:
        raise CandidateError("generated bootstrap exceeds its byte limit")
    constants = literal_assignments(result, names)
    expected_pins = {platform: libraries[platform].bootstrap_pin for platform in EXPECTED_PLATFORMS}
    if constants != {"BOOTSTRAP_TAG": bootstrap_tag, "CHANNEL": channel_url, "LIBRARIES": expected_pins}:
        raise CandidateError("generated bootstrap does not contain its selected immutable identities")
    return result


def validate_mac_sdk_package(directory: Path, source: SourceSnapshot, checkpoint_date: str) -> MacSdkPackage:
    """Explicit current Mac selection; never reinterpret the historical channel archive."""
    import install_macos_preview as mac
    try:
        directory = Path(directory)
        if directory.is_symlink():
            raise ValueError('Mac SDK package input must not be a symlink')
        directory = directory.resolve(strict=True)
        if not directory.is_dir():
            raise ValueError('Mac SDK package input must be a real directory')
        if not isinstance(checkpoint_date, str) or not re.fullmatch('[0-9]{8}', checkpoint_date):
            raise ValueError('invalid Mac checkpoint date')
        datetime.datetime.strptime(checkpoint_date, '%Y%m%d')
        raw = read_regular(directory/'release-manifest.json', MAX_JSON, 'Mac SDK release manifest')
        manifest = json.loads(raw)
        if (not isinstance(manifest, dict) or manifest.get('schema') != 1
                or manifest.get('platform') != 'macos-arm64' or manifest.get('package_schema') != mac.SDK_SCHEMA
                or manifest.get('source_sha') != source.revision or manifest.get('source_tree') != source.tree
                or manifest.get('production_ready') is not False
                or manifest.get('asset') != 'nudox-macos-arm64-'+source.revision[:10]+'.tar.gz'
                or type(manifest.get('size_bytes')) is not int or not 0 < manifest['size_bytes'] <= mac.MAX_ARCHIVE_BYTES
                or not isinstance(manifest.get('sha256'), str) or not re.fullmatch('[a-f0-9]{64}', manifest['sha256'])):
            raise ValueError('Mac SDK package differs from selected current source/artifact')
        archive = directory/manifest['asset']
        # Full bounded artifact read, not a HEAD/length-only admission.
        descriptor = os.open(archive, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        observed = hashlib.sha256()
        with os.fdopen(descriptor, 'rb') as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode) or before.st_size != manifest['size_bytes'] or before.st_nlink != 1:
                raise ValueError('Mac SDK archive is not the selected regular artifact')
            while chunk := stream.read(1024*1024): observed.update(chunk)
            if observed.hexdigest() != manifest['sha256']:
                raise ValueError('Mac SDK archive digest differs from selected manifest')
            if read_regular(directory/'install-macos-arm64.py', MAX_SCRIPT, 'Mac SDK installer') != source.mac_library:
                raise ValueError('Mac SDK installer differs from selected source')
            stream.seek(0)
            with tempfile.TemporaryDirectory(prefix='nudox-mac-sdk-admission-') as temporary:
                stage = Path(temporary)
                mac.safe_extract(archive, stage, archive_file=stream)
                package = json.loads(read_regular(stage/'manifest.json', mac.MAX_PACKAGE_METADATA_BYTES, 'Mac SDK package metadata'))
                mac.verify_sdk_package(stage, package, source.revision)
                origin = json.loads(read_regular(stage/mac.SDK_MANIFEST, mac.MAX_PACKAGE_METADATA_BYTES, 'selected Mac application manifest'))['source']
                selected_lock = digest(source_file(source.root, source.revision, 'Cargo.lock', MAX_SCRIPT))
                if origin['git_tree'] != source.tree or origin['cargo_lock_sha256'] != selected_lock:
                    raise ValueError('Mac application tree/lock differs from selected current source')
                if package.get('build_manifest_sha256') != manifest.get('build_manifest_sha256'):
                    raise ValueError('Mac SDK release/application manifest binding differs')
            if mac._file_identity(before) != mac._file_identity(os.fstat(stream.fileno())) or mac._file_identity(before) != mac._file_identity(archive.lstat()):
                raise ValueError('Mac SDK archive identity changed during admission')
        sidecar = read_regular(directory/(manifest['asset']+'.sha256'), 4096, 'Mac SDK archive sidecar')
        if sidecar != (manifest['sha256']+'  '+manifest['asset']+'\n').encode():
            raise ValueError('Mac SDK checksum sidecar differs from artifact')
        return MacSdkPackage(directory, manifest, raw, archive,
                             'checkpoint-'+checkpoint_date+'-'+source.revision[:10]+'-macos-arm64')
    except (mac.InstallError, KeyError, TypeError, ValueError, OSError, tarfile.TarError) as error:
        raise CandidateError('Mac SDK candidate refused: '+str(error)) from error


def compose_candidate(source: SourceSnapshot, package: LinuxPackage, mac_package: MacSdkPackage | None = None) -> CandidateBundle:
    verify_library_contract(source.mac_library, "macos-arm64")
    if linux_checkpoint_tag(package.manifest, package.installer, package.manifest_sha256) != package.tag:
        raise CandidateError("Linux package checkpoint identity changed after admission")
    if pin_linux_installer(package.installer_source_template, package.tag,
                           package.manifest_sha256) != package.installer:
        raise CandidateError("Linux platform library differs from its selected source and manifest")
    original_mac_row = source.base_channel["platforms"]["macos-arm64"]
    linux_tag = package.tag
    date = linux_tag.split("-")[1]
    bootstrap_tag = (f"nudox-diagnostic-installer-{date}-{package.manifest['source_sha'][:10]}-"
                     f"{package.manifest_sha256[:10]}-{source.revision[:10]}")
    if mac_package is not None:
        if mac_package.manifest.get('source_sha') != source.revision or mac_package.manifest.get('source_tree') != source.tree:
            raise CandidateError('Mac SDK package differs from selected source')
        bootstrap_tag += '-'+mac_package.tag.split('-')[1]+'-'+mac_package.manifest['sha256'][:10]
    if not re.fullmatch(r"[A-Za-z0-9.-]{1,100}", bootstrap_tag):
        raise CandidateError("derived bootstrap tag is invalid")
    if bootstrap_tag in {linux_tag, original_mac_row["tag"]}:
        raise CandidateError("candidate bootstrap tag collides with a platform release")

    libraries = {
        "linux-x64": PlatformLibrary("linux-x64", LINUX_LIBRARY_NAME, package.installer),
        "macos-arm64": PlatformLibrary("macos-arm64", MAC_LIBRARY_NAME, source.mac_library),
    }
    channel_url = bootstrap.release_url(bootstrap_tag, "preview-channel.json")
    bootstrap_bytes = replace_bootstrap_identity(source.bootstrap_template, bootstrap_tag=bootstrap_tag,
                                                 channel_url=channel_url, libraries=libraries)
    bootstrap_sha = digest(bootstrap_bytes)
    linux_manifest = package.manifest
    linux_asset = linux_manifest["asset"]
    linux_installer = libraries["linux-x64"]
    mac_installer = libraries["macos-arm64"]
    channel = json.loads(json.dumps(source.base_channel))
    channel["bootstrap"] = {
        "url": bootstrap.release_url(bootstrap_tag, f"install-forgejo-bootstrap-{bootstrap_sha[:16]}.py"),
        "sha256": bootstrap_sha,
        "bytes": len(bootstrap_bytes),
    }
    mac_row = channel["platforms"]["macos-arm64"]
    mac_row["installer"] = {
        "url": bootstrap.release_url(bootstrap_tag, f"{mac_installer.asset_stem}-{mac_installer.sha256[:16]}.py"),
        "sha256": mac_installer.sha256,
        "bytes": mac_installer.byte_count,
    }
    if mac_package is not None:
        mac_row.update(source=mac_package.manifest['source_sha'], tag=mac_package.tag,
                       archive={'url': bootstrap.release_url(mac_package.tag, mac_package.archive.name),
                                'sha256': mac_package.manifest['sha256'], 'bytes': mac_package.manifest['size_bytes']},
                       known_failures=['SDK-only app-preserving CLI candidate; whole-application fresh install/update acceptance is not established.'])
    base_linux_row = source.base_channel["platforms"]["linux-x64"]
    known_failures = linux_failures_for_candidate(base_linux_row, package)
    if package.native_qa_limitation not in known_failures:
        known_failures.append(package.native_qa_limitation)
    linux_row = {
        "status": "diagnostic-preview",
        "source": linux_manifest["source_sha"],
        "tag": linux_tag,
        "known_failures": known_failures,
        "installer": {
            "url": bootstrap.release_url(bootstrap_tag, f"{linux_installer.asset_stem}-{linux_installer.sha256[:16]}.py"),
            "sha256": linux_installer.sha256,
            "bytes": linux_installer.byte_count,
        },
        "archive": {
            "url": bootstrap.release_url(linux_tag, linux_asset),
            "sha256": linux_manifest["sha256"],
            "bytes": linux_manifest["size_bytes"],
        },
        "manifest": {
            "url": bootstrap.release_url(linux_tag, "release-manifest-linux-x64.json"),
            "sha256": package.manifest_sha256,
            "bytes": len(package.manifest_bytes),
        },
    }
    channel["platforms"]["linux-x64"] = linux_row

    try:
        bootstrap.validate_channel(channel, bootstrap_tag=bootstrap_tag,
                                   libraries={key: value.bootstrap_pin for key, value in libraries.items()})
        bootstrap.admit_libraries(channel, libraries={key: value.bootstrap_pin for key, value in libraries.items()})
    except (bootstrap.InstallError, KeyError, TypeError, ValueError) as error:
        raise CandidateError(f"assembled channel and bootstrap pins disagree: {error}") from error
    if mac_package is None and (mac_row["tag"] != original_mac_row["tag"]
            or mac_row["source"] != original_mac_row["source"]
            or mac_row["archive"] != original_mac_row["archive"]):
        raise CandidateError("Mac release identity changed while composing the Linux candidate")
    launcher = candidate_launcher(source.launcher_template, bootstrap_tag, bootstrap_bytes)
    return CandidateBundle(bootstrap_tag, bootstrap_bytes, launcher, channel, libraries, package, source, mac_package)


def write_exclusive(path: Path | str, body: bytes, *, dir_fd: int | None = None) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                         0o600, dir_fd=dir_fd)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(body)


def copy_verified(source: Path, destination: Path, expected_sha256: str, expected_bytes: int, *,
                  destination_dir_fd: int | None = None,
                  verify_destination: Callable[[], None] | None = None) -> None:
    if source.is_symlink() or not source.is_file():
        raise CandidateError(f"candidate package asset is not a regular file: {source.name}")
    source_hash = hashlib.sha256()
    owns_destination_parent_fd = destination_dir_fd is None
    destination_parent_fd = destination_dir_fd
    destination_fd = None
    destination_identity = None
    copied = 0
    try:
        if destination_parent_fd is None:
            destination_parent_fd = os.open(
                destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        if verify_destination is not None:
            verify_destination()
        source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(source_fd, "rb") as incoming:
            before = os.fstat(incoming.fileno())
            if before.st_size != expected_bytes:
                raise CandidateError(f"candidate package asset size changed: {source.name}")
            destination_fd = os.open(
                destination.name,
                os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
                dir_fd=destination_parent_fd,
            )
            created = os.fstat(destination_fd)
            destination_identity = (created.st_dev, created.st_ino)
            if not stat.S_ISREG(created.st_mode) or created.st_nlink != 1:
                raise CandidateError(f"staged package asset is not a new regular file: {source.name}")
            os.fchmod(destination_fd, 0o600)
            with os.fdopen(os.dup(destination_fd), "wb") as outgoing:
                while chunk := incoming.read(1024 * 1024):
                    copied += len(chunk)
                    if copied > expected_bytes:
                        raise CandidateError(f"candidate package asset grew while copying: {source.name}")
                    source_hash.update(chunk)
                    outgoing.write(chunk)
            after = os.fstat(incoming.fileno())
            identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
            current = source.lstat()
            if (identity(before) != identity(after) or identity(after) != identity(current)
                    or copied != expected_bytes or source_hash.hexdigest() != expected_sha256):
                raise CandidateError(f"candidate package asset changed while copying: {source.name}")

        os.fsync(destination_fd)
        target_before = os.fstat(destination_fd)
        if (not stat.S_ISREG(target_before.st_mode) or target_before.st_nlink != 1
                or target_before.st_size != expected_bytes
                or (target_before.st_dev, target_before.st_ino) != destination_identity):
            raise CandidateError(f"staged package asset identity changed: {source.name}")
        os.lseek(destination_fd, 0, os.SEEK_SET)
        destination_hash = hashlib.sha256()
        verified_bytes = 0
        while chunk := os.read(destination_fd, 1024 * 1024):
            verified_bytes += len(chunk)
            if verified_bytes > expected_bytes:
                raise CandidateError(f"staged package asset grew while verifying: {source.name}")
            destination_hash.update(chunk)
        target_after = os.fstat(destination_fd)
        named_target = os.stat(destination.name, dir_fd=destination_parent_fd, follow_symlinks=False)
        if (verified_bytes != expected_bytes or destination_hash.hexdigest() != expected_sha256
                or (target_after.st_dev, target_after.st_ino) != destination_identity
                or target_after.st_size != expected_bytes or target_after.st_nlink != 1
                or not stat.S_ISREG(target_after.st_mode)
                or (named_target.st_dev, named_target.st_ino) != destination_identity
                or named_target.st_size != expected_bytes or named_target.st_nlink != 1
                or not stat.S_ISREG(named_target.st_mode)):
            raise CandidateError(f"staged candidate package asset differs from its admitted identity: {source.name}")
        if verify_destination is not None:
            verify_destination()
    finally:
        if destination_fd is not None:
            os.close(destination_fd)
        if owns_destination_parent_fd and destination_parent_fd is not None:
            os.close(destination_parent_fd)


def _same_directory(left, right) -> bool:
    return (stat.S_ISDIR(left.st_mode) and stat.S_ISDIR(right.st_mode)
            and (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino))


def _parent_protects_output(parent_info) -> bool:
    """Accept caller-owned parents or root-owned parents; shared writes require sticky protection."""
    if parent_info.st_uid not in {0, os.geteuid()}:
        return False
    shared_write = parent_info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
    return not shared_write or bool(parent_info.st_mode & stat.S_ISVTX)


def _output_binding(parent: Path, parent_fd: int, output_name: str, output_fd: int,
                    output_identity: tuple[int, int]) -> None:
    """Require both directory descriptors still name their originally created paths."""
    parent_open = os.fstat(parent_fd)
    try:
        parent_named = os.stat(parent, follow_symlinks=False)
    except OSError as error:
        raise CandidateError("candidate output parent path changed during assembly") from error
    if not _same_directory(parent_open, parent_named):
        raise CandidateError("candidate output parent path changed during assembly")
    if not _parent_protects_output(parent_open):
        raise CandidateError("candidate output parent is writable by an untrusted user")

    output_open = os.fstat(output_fd)
    try:
        output_named = os.stat(output_name, dir_fd=parent_fd, follow_symlinks=False)
        output_path_named = os.stat(parent / output_name, follow_symlinks=False)
    except OSError as error:
        raise CandidateError("candidate output directory identity changed during assembly") from error
    if (not _same_directory(output_open, output_named)
            or (output_open.st_dev, output_open.st_ino) != output_identity
            or (output_path_named.st_dev, output_path_named.st_ino) != output_identity
            or output_open.st_uid != os.geteuid() or output_open.st_mode & 0o077):
        raise CandidateError("candidate output directory identity changed during assembly")


def _output_asset_hash(name: str, output_fd: int,
                       verify_binding: Callable[[], None]) -> dict[str, object]:
    verify_binding()
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
                         dir_fd=output_fd)
    with os.fdopen(descriptor, "rb") as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
            raise CandidateError(f"candidate output asset is not a single-link regular file: {name}")
        hasher = hashlib.sha256()
        count = 0
        while chunk := stream.read(1024 * 1024):
            count += len(chunk)
            if count > release.LINUX_MAX_ARCHIVE_BYTES:
                raise CandidateError(f"candidate output asset exceeds its verification bound: {name}")
            hasher.update(chunk)
        after = os.fstat(stream.fileno())
    named = os.stat(name, dir_fd=output_fd, follow_symlinks=False)
    identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
    if (identity(before) != identity(after) or identity(after) != identity(named)
            or count != after.st_size or named.st_nlink != 1 or not stat.S_ISREG(named.st_mode)):
        raise CandidateError(f"candidate output asset changed while hashing: {name}")
    verify_binding()
    return {"sha256": hasher.hexdigest(), "bytes": count}


def prepare_output(bundle: CandidateBundle, output_dir: Path) -> dict:
    """Assemble under a POSIX-trusted parent and retain failures for inspection.

    Parent safety uses owner/mode/sticky metadata; callers must avoid ACL grants that let
    other users replace entries in the selected parent.
    """
    requested = Path(os.path.abspath(Path(output_dir).expanduser()))
    if requested.is_symlink() or requested.exists():
        raise CandidateError("candidate output directory must be new")
    parent = requested.parent.resolve(strict=False)
    output = parent / requested.name
    if output.exists() or output.is_symlink():
        raise CandidateError("candidate output directory must be new")
    if output == bundle.source.root or bundle.source.root in output.parents:
        raise CandidateError("candidate output directory must be outside the source checkout")
    if output == bundle.linux_package.directory or bundle.linux_package.directory in output.parents:
        raise CandidateError("candidate output directory must be outside the selected package input")
    if bundle.mac_package is not None:
        mac_input = bundle.mac_package.directory.resolve(strict=True)
        if output == mac_input or mac_input in output.parents:
            raise CandidateError('candidate output directory must be outside the selected Mac package input')
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        parent_fd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    except OSError as error:
        raise CandidateError(f"cannot bind candidate output parent directory: {error}") from error
    output_fd = None
    output_identity = None
    output_name = output.name
    output_created = False
    try:
        parent_open = os.fstat(parent_fd)
        parent_named = os.stat(parent, follow_symlinks=False)
        if not _same_directory(parent_open, parent_named):
            raise CandidateError("candidate output parent path changed before assembly")
        if not _parent_protects_output(parent_open):
            raise CandidateError("candidate output parent is writable by an untrusted user")
        os.mkdir(output_name, mode=0o700, dir_fd=parent_fd)
        output_created = True
        output_fd = os.open(output_name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                            dir_fd=parent_fd)
        created_output = os.fstat(output_fd)
        output_identity = (created_output.st_dev, created_output.st_ino)
        if not stat.S_ISDIR(created_output.st_mode) or created_output.st_uid != os.geteuid():
            raise CandidateError("candidate output directory identity is not owned by this process")

        def verify_binding() -> None:
            _output_binding(parent, parent_fd, output_name, output_fd, output_identity)

        linux_file = f"{bundle.libraries['linux-x64'].asset_stem}-{bundle.libraries['linux-x64'].sha256[:16]}.py"
        mac_file = f"{bundle.libraries['macos-arm64'].asset_stem}-{bundle.libraries['macos-arm64'].sha256[:16]}.py"
        bootstrap_file = f"install-forgejo-bootstrap-{digest(bundle.bootstrap)[:16]}.py"
        launcher_file = f"install-forgejo-launcher-{digest(bundle.launcher)[:16]}.sh"
        channel_bytes = (json.dumps(bundle.channel, indent=2) + "\n").encode()
        def write_asset(name: str, body: bytes) -> None:
            verify_binding()
            write_exclusive(name, body, dir_fd=output_fd)
            verify_binding()

        write_asset(bootstrap_file, bundle.bootstrap)
        write_asset(launcher_file, bundle.launcher)
        write_asset("install.sh", bundle.launcher)
        write_asset(linux_file, bundle.libraries["linux-x64"].body)
        write_asset(mac_file, bundle.libraries["macos-arm64"].body)
        write_asset("preview-channel.json", channel_bytes)
        manifest_name = "release-manifest-linux-x64.json"
        write_asset(manifest_name, bundle.linux_package.manifest_bytes)
        archive_name = bundle.linux_package.archive.name
        verify_binding()
        copy_verified(bundle.linux_package.archive, output / archive_name,
                      bundle.linux_package.archive_sha256, bundle.linux_package.archive_bytes,
                      destination_dir_fd=output_fd, verify_destination=verify_binding)
        checksum_name = archive_name + ".sha256"
        checksum_source = bundle.linux_package.directory / checksum_name
        checksum = read_regular(checksum_source, 4096, "Linux archive checksum sidecar")
        expected_checksum = f"{bundle.linux_package.archive_sha256}  {archive_name}\n".encode()
        if checksum != expected_checksum:
            raise CandidateError("Linux archive checksum sidecar differs from the selected manifest")
        write_asset(checksum_name, checksum)

        expected_assets = {
            bootstrap_file: {"sha256": digest(bundle.bootstrap), "bytes": len(bundle.bootstrap)},
            launcher_file: {"sha256": digest(bundle.launcher), "bytes": len(bundle.launcher)},
            "install.sh": {"sha256": digest(bundle.launcher), "bytes": len(bundle.launcher)},
            linux_file: {"sha256": bundle.libraries["linux-x64"].sha256,
                         "bytes": bundle.libraries["linux-x64"].byte_count},
            mac_file: {"sha256": bundle.libraries["macos-arm64"].sha256,
                       "bytes": bundle.libraries["macos-arm64"].byte_count},
            "preview-channel.json": {"sha256": digest(channel_bytes), "bytes": len(channel_bytes)},
            manifest_name: {"sha256": digest(bundle.linux_package.manifest_bytes),
                            "bytes": len(bundle.linux_package.manifest_bytes)},
            archive_name: {"sha256": bundle.linux_package.archive_sha256,
                           "bytes": bundle.linux_package.archive_bytes},
            checksum_name: {"sha256": digest(checksum), "bytes": len(checksum)},
        }
        asset_names = {
            bootstrap_file, launcher_file, "install.sh", linux_file, mac_file,
            "preview-channel.json", manifest_name, archive_name, checksum_name,
        }
        if bundle.mac_package is not None:
            mac = bundle.mac_package
            mac_manifest_name = 'release-manifest-macos-arm64.json'
            write_asset(mac_manifest_name, mac.manifest_bytes)
            copy_verified(mac.archive, output/mac.archive.name, mac.manifest['sha256'], mac.manifest['size_bytes'],
                          destination_dir_fd=output_fd, verify_destination=verify_binding)
            mac_checksum = (mac.manifest['sha256']+'  '+mac.archive.name+'\n').encode()
            write_asset(mac.archive.name+'.sha256', mac_checksum)
            expected_assets.update({mac_manifest_name: {'sha256': digest(mac.manifest_bytes), 'bytes': len(mac.manifest_bytes)},
                                   mac.archive.name: {'sha256': mac.manifest['sha256'], 'bytes': mac.manifest['size_bytes']},
                                   mac.archive.name+'.sha256': {'sha256': digest(mac_checksum), 'bytes': len(mac_checksum)}})
            asset_names |= {mac_manifest_name, mac.archive.name, mac.archive.name+'.sha256'}
        verify_binding()
        actual_names = set(os.listdir(output_fd))
        if actual_names != asset_names:
            raise CandidateError("candidate output contains unexpected or missing assets")
        assets = {}
        for name in sorted(asset_names):
            measured = _output_asset_hash(name, output_fd, verify_binding)
            if measured != expected_assets[name]:
                raise CandidateError(f"candidate output asset differs from its admitted bytes: {name}")
            assets[name] = measured
        receipt = {
            "schema": "nudox.diagnostic-forgejo-candidate-assembly.v1",
            "production_ready": False,
            "published": False,
            "native_acceptance_claimed": False,
            "bootstrap_tag": bundle.bootstrap_tag,
            "launcher": {
                "asset": launcher_file,
                "sha256": digest(bundle.launcher),
                "bytes": len(bundle.launcher),
                "bootstrap_tag": bundle.bootstrap_tag,
                "bootstrap_sha256": digest(bundle.bootstrap),
                "bootstrap_bytes": len(bundle.bootstrap),
            },
            "source_snapshot": {"revision": bundle.source.revision, "tree": bundle.source.tree},
            "base_channel_sha256": bundle.source.base_channel_sha256,
            "linux_release": {
                "tag": bundle.linux_package.tag,
                "source": bundle.linux_package.manifest["source_sha"],
                "source_tree": bundle.linux_package.manifest["source_tree"],
                "manifest_sha256": bundle.linux_package.manifest_sha256,
                "archive_sha256": bundle.linux_package.archive_sha256,
                "archive_bytes": bundle.linux_package.archive_bytes,
                "uncompleted_native_qa_template_sha256": bundle.linux_package.native_qa_template_sha256,
            },
            "macos_identity": {
                "source": bundle.channel["platforms"]["macos-arm64"]["source"],
                "tag": bundle.channel["platforms"]["macos-arm64"]["tag"],
                "archive": bundle.channel["platforms"]["macos-arm64"]["archive"],
            },
            "assets": assets,
        }
        receipt_bytes = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode()
        write_asset("candidate-assembly.json", receipt_bytes)
        if _output_asset_hash("candidate-assembly.json", output_fd, verify_binding) != {
                "sha256": digest(receipt_bytes), "bytes": len(receipt_bytes)}:
            raise CandidateError("candidate assembly receipt differs from its admitted bytes")
        verify_binding()
        final_names = set(os.listdir(output_fd))
        verify_binding()
        if final_names != asset_names | {"candidate-assembly.json"}:
            raise CandidateError("candidate output changed after its receipt was written")
        return receipt
    except Exception as error:
        if not output_created:
            if isinstance(error, FileExistsError):
                raise CandidateError("candidate output directory must be new") from error
            raise CandidateError(f"cannot create candidate output directory: {error}") from error
        raise CandidateError(
            f"candidate assembly failed; the created private output was retained for inspection "
            f"(the requested path may have changed): {output}: {error}") from error
    finally:
        if output_fd is not None:
            os.close(output_fd)
        os.close(parent_fd)


def assemble(source_root: Path, linux_package_dir: Path, output_dir: Path, *, mac_package_dir: Path | None = None, mac_checkpoint_date: str | None = None) -> dict:
    source = selected_source_snapshot(source_root)
    package = validate_linux_package(linux_package_dir, source)
    if (mac_package_dir is None) != (mac_checkpoint_date is None):
        raise CandidateError('Mac SDK directory and explicit checkpoint date must be selected together')
    mac = validate_mac_sdk_package(mac_package_dir, source, mac_checkpoint_date) if mac_package_dir is not None else None
    bundle = compose_candidate(source, package, mac)
    return prepare_output(bundle, output_dir)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path,
                        help="clean Git checkout containing the reviewed bootstrap and current Mac library")
    parser.add_argument("--linux-package-directory", required=True, type=Path,
                        help="validated Linux release package with its manifest, archive, installer and QA template")
    parser.add_argument("--mac-sdk-package-directory", type=Path, help="optional current finalized SDK-only Mac CLI package; historical Mac remains unchanged when omitted")
    parser.add_argument("--mac-checkpoint-date", help="explicit YYYYMMDD date for the selected current Mac artifact")
    parser.add_argument("--output-directory", required=True, type=Path,
                        help="new private directory for immutable candidate assets; no upload is performed")
    args = parser.parse_args(argv)
    try:
        receipt = assemble(args.source_root, args.linux_package_directory, args.output_directory, mac_package_dir=args.mac_sdk_package_directory, mac_checkpoint_date=args.mac_checkpoint_date)
    except (CandidateError, OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"candidate assembly refused: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"output_directory": str(args.output_directory.resolve()),
                      "bootstrap_tag": receipt["bootstrap_tag"],
                      "assets": receipt["assets"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
