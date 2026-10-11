#!/usr/bin/env python3
"""Assemble a source- and digest-bound diagnostic Forgejo checkpoint bundle.

This prepares local candidate assets only. It never uploads or promotes them.
"""
from __future__ import annotations

import argparse
import ast
from dataclasses import dataclass
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
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
class CandidateBundle:
    bootstrap_tag: str
    bootstrap: bytes
    channel: dict
    libraries: dict[str, PlatformLibrary]
    linux_package: LinuxPackage
    source: SourceSnapshot


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
    return SourceSnapshot(root, revision, tree, bootstrap_template, mac_library, channel,
                          digest(channel_bytes))


def linux_checkpoint_tag(manifest: dict, installer: bytes, manifest_sha256: str) -> str:
    constants = literal_assignments(installer, {"PLATFORM", "PINNED_RELEASE_TAG", "PINNED_MANIFEST_SHA256"})
    tag = constants["PINNED_RELEASE_TAG"]
    if constants["PLATFORM"] != "linux-x64" or constants["PINNED_MANIFEST_SHA256"] != manifest_sha256:
        raise CandidateError("Linux installer does not pin this platform and exact manifest bytes")
    source = manifest.get("source_sha")
    if not isinstance(source, str) or not re.fullmatch(r"[a-f0-9]{40}", source):
        raise CandidateError("Linux manifest has no valid source revision")
    match = re.fullmatch(r"checkpoint-([0-9]{8})-([a-f0-9]{10})-linux-x64", tag) if isinstance(tag, str) else None
    if match is None or match[2] != source[:10]:
        raise CandidateError("Linux installer checkpoint tag differs from the manifest source")
    try:
        datetime.datetime.strptime(match[1], "%Y%m%d")
    except ValueError as error:
        raise CandidateError("Linux installer checkpoint date is invalid") from error
    expected_asset = f"nudox-linux-x86_64-{source[:10]}.tar.gz"
    if manifest.get("asset") != expected_asset:
        raise CandidateError("Linux archive name differs from its source revision")
    return tag


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
    return digest(body), f"Linux runtime acceptance remains open; the package carries an uncompleted native QA template ({names})."


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
    package_dir = directory.resolve(strict=True)
    if not package_dir.is_dir() or package_dir.is_symlink():
        raise CandidateError("Linux package input must be a real directory")
    manifest_path = package_dir / "release-manifest.json"
    manifest_bytes = read_regular(manifest_path, MAX_JSON, "Linux release manifest")
    manifest = checked_json(manifest_bytes, "Linux release manifest")
    try:
        release.validate_linux(package_dir, manifest, source.root, require_qa=False)
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


def compose_candidate(source: SourceSnapshot, package: LinuxPackage) -> CandidateBundle:
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
    linux_row = {
        "status": "diagnostic-preview",
        "source": linux_manifest["source_sha"],
        "tag": linux_tag,
        "known_failures": [package.native_qa_limitation],
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
    if (mac_row["tag"] != original_mac_row["tag"]
            or mac_row["source"] != original_mac_row["source"]
            or mac_row["archive"] != original_mac_row["archive"]):
        raise CandidateError("Mac release identity changed while composing the Linux candidate")
    return CandidateBundle(bootstrap_tag, bootstrap_bytes, channel, libraries, package, source)


def write_exclusive(path: Path, body: bytes) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(body)


def copy_verified(source: Path, destination: Path, expected_sha256: str, expected_bytes: int) -> None:
    if source.is_symlink() or not source.is_file():
        raise CandidateError(f"candidate package asset is not a regular file: {source.name}")
    source_hash = hashlib.sha256()
    copied = 0
    try:
        source_fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(source_fd, "rb") as incoming:
            before = os.fstat(incoming.fileno())
            if before.st_size != expected_bytes:
                raise CandidateError(f"candidate package asset size changed: {source.name}")
            target_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            with os.fdopen(target_fd, "wb") as outgoing:
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
    except Exception:
        destination.unlink(missing_ok=True)
        raise
    if release.sha256(destination) != expected_sha256:
        destination.unlink(missing_ok=True)
        raise CandidateError(f"staged candidate package asset differs from its admitted digest: {source.name}")


def prepare_output(bundle: CandidateBundle, output_dir: Path) -> dict:
    output = output_dir.resolve(strict=False)
    if output.exists() or output.is_symlink():
        raise CandidateError("candidate output directory must be new")
    if output == bundle.source.root or bundle.source.root in output.parents:
        raise CandidateError("candidate output directory must be outside the source checkout")
    if output == bundle.linux_package.directory or bundle.linux_package.directory in output.parents:
        raise CandidateError("candidate output directory must be outside the selected package input")
    output.mkdir(mode=0o700, parents=True)
    try:
        linux_file = f"{bundle.libraries['linux-x64'].asset_stem}-{bundle.libraries['linux-x64'].sha256[:16]}.py"
        mac_file = f"{bundle.libraries['macos-arm64'].asset_stem}-{bundle.libraries['macos-arm64'].sha256[:16]}.py"
        bootstrap_file = f"install-forgejo-bootstrap-{digest(bundle.bootstrap)[:16]}.py"
        channel_bytes = (json.dumps(bundle.channel, indent=2) + "\n").encode()
        write_exclusive(output / bootstrap_file, bundle.bootstrap)
        write_exclusive(output / linux_file, bundle.libraries["linux-x64"].body)
        write_exclusive(output / mac_file, bundle.libraries["macos-arm64"].body)
        write_exclusive(output / "preview-channel.json", channel_bytes)
        manifest_name = "release-manifest-linux-x64.json"
        write_exclusive(output / manifest_name, bundle.linux_package.manifest_bytes)
        archive_name = bundle.linux_package.archive.name
        copy_verified(bundle.linux_package.archive, output / archive_name,
                      bundle.linux_package.archive_sha256, bundle.linux_package.archive_bytes)
        checksum_name = archive_name + ".sha256"
        checksum_source = bundle.linux_package.directory / checksum_name
        checksum = read_regular(checksum_source, 4096, "Linux archive checksum sidecar")
        expected_checksum = f"{bundle.linux_package.archive_sha256}  {archive_name}\n".encode()
        if checksum != expected_checksum:
            raise CandidateError("Linux archive checksum sidecar differs from the selected manifest")
        write_exclusive(output / checksum_name, checksum)

        assets = {}
        for path in sorted(output.iterdir()):
            if path.name == "candidate-assembly.json":
                continue
            assets[path.name] = {"sha256": release.sha256(path), "bytes": path.stat().st_size}
        receipt = {
            "schema": "nudox.diagnostic-forgejo-candidate-assembly.v1",
            "production_ready": False,
            "published": False,
            "native_acceptance_claimed": False,
            "bootstrap_tag": bundle.bootstrap_tag,
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
                "source": bundle.source.base_channel["platforms"]["macos-arm64"]["source"],
                "tag": bundle.source.base_channel["platforms"]["macos-arm64"]["tag"],
                "archive": bundle.source.base_channel["platforms"]["macos-arm64"]["archive"],
            },
            "assets": assets,
        }
        receipt_bytes = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode()
        write_exclusive(output / "candidate-assembly.json", receipt_bytes)
        return receipt
    except Exception:
        shutil.rmtree(output)
        raise


def assemble(source_root: Path, linux_package_dir: Path, output_dir: Path) -> dict:
    source = selected_source_snapshot(source_root)
    package = validate_linux_package(linux_package_dir, source)
    bundle = compose_candidate(source, package)
    return prepare_output(bundle, output_dir)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path,
                        help="clean Git checkout containing the reviewed bootstrap and current Mac library")
    parser.add_argument("--linux-package-directory", required=True, type=Path,
                        help="validated Linux release package with its manifest, archive, installer and QA template")
    parser.add_argument("--output-directory", required=True, type=Path,
                        help="new private directory for immutable candidate assets; no upload is performed")
    args = parser.parse_args(argv)
    try:
        receipt = assemble(args.source_root, args.linux_package_directory, args.output_directory)
    except (CandidateError, OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"candidate assembly refused: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"output_directory": str(args.output_directory.resolve()),
                      "bootstrap_tag": receipt["bootstrap_tag"],
                      "assets": receipt["assets"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
