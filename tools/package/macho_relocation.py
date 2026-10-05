#!/usr/bin/env python3
"""Shared, fail-closed receipt primitives for relocatable Mach-O inputs.

This module records input identity. It does not mutate or sign Mach-O files.
"""

from __future__ import annotations

import hashlib
import json
import os
import posixpath
import re
import stat
import tarfile
import base64
from pathlib import Path
from typing import Any


RUNTIME_RECEIPT_SCHEMA = 1
TREE_DOMAIN = b"nudox-macos-runtime-tree-v1\0"


class RelocationInputError(ValueError):
    pass


def write_new_bytes(path: Path, content: bytes, *, label: str) -> None:
    """Create immutable output bytes without following or replacing a name.

    ``xb`` maps to exclusive creation (O_CREAT | O_EXCL): a regular file,
    directory, or symlink appearing at ``path`` after a caller's preflight is
    refused by the filesystem rather than followed or truncated.
    """
    try:
        with path.open("xb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
    except FileExistsError as error:
        raise RelocationInputError(
            f"refusing to replace existing {label} (including a symlink): {path}"
        ) from error


def _fail(message: str) -> None:
    raise RelocationInputError(message)


def require_sha256(value: Any, label: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        _fail(f"{label} must be a lowercase SHA-256 digest")
    return value


def canonical_json_sha256(value: Any) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def file_tree_manifest(root: Path) -> dict[str, dict[str, Any]]:
    """Hash every regular file and record safe symlinks without following them."""
    try:
        resolved_root = root.resolve(strict=True)
    except OSError as error:
        _fail(f"runtime root is unavailable: {error}")
    if not resolved_root.is_dir():
        _fail("runtime root must be a directory")
    manifest: dict[str, dict[str, Any]] = {}
    for path in sorted(resolved_root.rglob("*")):
        relative = path.relative_to(resolved_root).as_posix()
        if path.is_symlink():
            target = os.readlink(path)
            try:
                resolved_target = path.resolve(strict=True)
            except OSError as error:
                _fail(f"runtime symlink is dangling: {relative}: {error}")
            if not _is_within(resolved_target, resolved_root):
                _fail(f"runtime symlink escapes its admitted root: {relative}")
            if not resolved_target.is_file():
                _fail(f"runtime symlink does not identify a regular file: {relative}")
            manifest[relative] = {"kind": "symlink", "target": target}
        elif path.is_file():
            metadata = path.stat()
            manifest[relative] = {
                "kind": "file",
                "sha256": _sha256(path),
                "size_bytes": metadata.st_size,
                "mode": stat.S_IMODE(metadata.st_mode),
            }
        elif not path.is_dir():
            _fail(f"runtime root contains an unsupported filesystem entry: {relative}")
    return manifest


def file_tree_sha256(manifest: dict[str, dict[str, Any]]) -> str:
    encoded = json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(TREE_DOMAIN + encoded).hexdigest()


def archive_tree_manifest(archive_path: Path) -> dict[str, dict[str, Any]]:
    """Read a pinned tar archive without extracting it and derive its file tree."""
    entries: dict[str, dict[str, Any]] = {}
    try:
        archive = tarfile.open(archive_path, mode="r:*")
    except (OSError, tarfile.TarError) as error:
        _fail(f"cannot read the pinned .NET runtime archive: {error}")
    with archive:
        for member in archive:
            raw = member.name
            parts = [part for part in raw.split("/") if part not in ("", ".")]
            if not parts:
                continue
            if raw.startswith("/") or any(part == ".." for part in parts):
                _fail(f".NET archive contains an unsafe member path: {raw}")
            relative = "/".join(parts)
            if member.isdir():
                continue
            if relative in entries:
                _fail(f".NET archive repeats a file path: {relative}")
            if member.issym():
                target = member.linkname
                if not target or target.startswith("/"):
                    _fail(f".NET archive has an unsafe symlink: {relative}")
                resolved = posixpath.normpath(posixpath.join(posixpath.dirname(relative), target))
                if resolved == ".." or resolved.startswith("../"):
                    _fail(f".NET archive symlink escapes its root: {relative}")
                entries[relative] = {"kind": "symlink", "target": target}
                continue
            if member.isfile() or member.islnk():
                stream = archive.extractfile(member)
                if stream is None:
                    _fail(f".NET archive member cannot be read: {relative}")
                digest = hashlib.sha256()
                size = 0
                with stream:
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        size += len(block)
                        digest.update(block)
                entries[relative] = {
                    "kind": "file",
                    "sha256": digest.hexdigest(),
                    "size_bytes": size,
                    "mode": stat.S_IMODE(member.mode),
                }
                continue
            _fail(f".NET archive contains an unsupported member type: {relative}")
    return dict(sorted(entries.items()))


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _safe_relative(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value or "\\" in value:
        _fail(f"{label} must be a nonempty POSIX relative path")
    path = Path(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        _fail(f"{label} must not escape its package root")
    normalized = path.as_posix()
    if normalized != value:
        _fail(f"{label} must use normalized POSIX path syntax")
    return normalized


def bundle_relative(value: Any, label: str) -> str:
    relative = _safe_relative(value, label)
    if not relative.startswith("Contents/"):
        _fail(f"{label} must be below Contents/")
    return relative


def validate_relocation_plan(plan: Any) -> dict[str, Any]:
    if not isinstance(plan, dict) or plan.get("schema") != 1 or plan.get("kind") != "macho-relocation-plan":
        _fail("Mach-O relocation plan has an unsupported schema or kind")
    source = plan.get("source")
    if not isinstance(source, dict) or re.fullmatch(r"[0-9a-f]{40}", str(source.get("git_revision", ""))) is None or re.fullmatch(r"[0-9a-f]{40}", str(source.get("git_tree", ""))) is None:
        _fail("Mach-O relocation plan is missing its exact application source identity")
    target = plan.get("target")
    if not isinstance(target, dict) or target.get("triple") not in {
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    } or target.get("architecture") != ("arm64" if target["triple"].startswith("aarch64-") else "x86_64"):
        _fail("Mach-O relocation plan has an invalid target architecture")
    origins = plan.get("origins")
    if not isinstance(origins, dict) or not origins:
        _fail("Mach-O relocation plan has no admitted origins")
    for origin_id, origin in origins.items():
        _safe_id(origin_id, "origin ID")
        if not isinstance(origin, dict):
            _fail("Mach-O relocation origin record is malformed")
        if origin.get("receipt_kind") not in {"application", "roslyn", "helpers", "dotnet-runtime"}:
            _fail(f"Mach-O origin {origin_id} has an unsupported receipt kind")
        require_sha256(origin.get("receipt_sha256"), f"Mach-O origin {origin_id} receipt")
        require_sha256(origin.get("root_tree_sha256"), f"Mach-O origin {origin_id} tree")
        prefix = bundle_relative(origin.get("bundle_prefix"), f"Mach-O origin {origin_id} prefix")
        process_roots = origin.get("process_roots")
        if not isinstance(process_roots, dict):
            _fail(f"Mach-O origin {origin_id} has a malformed process-root map")
        for source_path, bundle_path in process_roots.items():
            _safe_relative(source_path, f"Mach-O origin {origin_id} process root")
            bundle_relative(bundle_path, f"Mach-O origin {origin_id} process root")
        image_hashes = origin.get("image_hashes")
        if not isinstance(image_hashes, dict):
            _fail(f"Mach-O origin {origin_id} has no image digest map")
        for path, digest in image_hashes.items():
            bundle_relative(path, f"Mach-O origin {origin_id} image path")
            require_sha256(digest, f"Mach-O origin {origin_id} image")
        if not prefix:
            _fail("Mach-O origin prefix cannot be empty")
    packages = plan.get("packages")
    if not isinstance(packages, dict):
        _fail("Mach-O relocation plan has no pinned package map")
    for package_id, package in packages.items():
        _safe_id(package_id, "package ID")
        if not isinstance(package, dict) or not isinstance(package.get("name"), str) or not package["name"].strip():
            _fail(f"Mach-O package {package_id} is malformed")
        if not isinstance(package.get("version"), str) or not package["version"].strip():
            _fail(f"Mach-O package {package_id} has no version")
        require_sha256(package.get("root_tree_sha256"), f"Mach-O package {package_id} tree")
        provenance = package.get("provenance")
        if not isinstance(provenance, dict) or provenance.get("kind") not in {"verified-archive", "pinned-nix-output"}:
            _fail(f"Mach-O package {package_id} has no supported provenance")
        require_sha256(provenance.get("identity_sha256"), f"Mach-O package {package_id} provenance")
        if not isinstance(provenance.get("locator"), str) or not provenance["locator"].strip():
            _fail(f"Mach-O package {package_id} has no provenance locator")
        notices = package.get("notices")
        if not isinstance(notices, list) or not notices:
            _fail(f"Mach-O package {package_id} has no license notices")
        notice_names: set[str] = set()
        for notice in notices:
            if not isinstance(notice, dict) or not isinstance(notice.get("name"), str):
                _fail(f"Mach-O package {package_id} has a malformed notice")
            if notice["name"] in notice_names:
                _fail(f"Mach-O package {package_id} repeats a notice name")
            notice_names.add(notice["name"])
            require_sha256(notice.get("sha256"), f"Mach-O package {package_id} notice")
            encoded = notice.get("content_base64")
            if not isinstance(encoded, str):
                _fail(f"Mach-O package {package_id} notice bytes are absent")
            try:
                decoded = base64.b64decode(encoded, validate=True)
            except ValueError:
                _fail(f"Mach-O package {package_id} notice encoding is invalid")
            if hashlib.sha256(decoded).hexdigest() != notice["sha256"]:
                _fail(f"Mach-O package {package_id} notice digest mismatch")
    images = plan.get("images")
    if not isinstance(images, list) or not images:
        _fail("Mach-O relocation plan has no image records")
    refs: set[str] = set()
    for image in images:
        if not isinstance(image, dict) or image.get("kind") not in {"origin", "package"}:
            _fail("Mach-O relocation image record is malformed")
        ref = image.get("ref")
        if not isinstance(ref, str) or not ref or ref in refs:
            _fail("Mach-O relocation image reference is missing or duplicated")
        refs.add(ref)
        require_sha256(image.get("sha256"), f"Mach-O image {ref}")
        if image["kind"] == "origin":
            origin_id = image.get("origin_id")
            if origin_id not in origins:
                _fail(f"Mach-O image {ref} names an unknown origin")
            bundle_path = bundle_relative(image.get("bundle_path"), f"Mach-O image {ref} bundle path")
            source_path = _safe_relative(image.get("source_relative_path"), f"Mach-O image {ref} source path")
            origin = origins[origin_id]
            expected_path = f"{origin['bundle_prefix']}/{source_path}"
            if bundle_path != expected_path or origin["image_hashes"].get(bundle_path) != image["sha256"]:
                _fail(f"Mach-O origin image {ref} differs from its admitted root mapping")
        else:
            package_id = image.get("package_id")
            if package_id not in packages:
                _fail(f"Mach-O image {ref} names an unknown package")
            package_path = _safe_relative(image.get("package_relative_path"), f"Mach-O image {ref} package path")
            if image.get("bundle_path") != f"Contents/Frameworks/{package_id}/{package_path}":
                _fail(f"Mach-O package image {ref} has a noncanonical staging path")
        if not isinstance(image.get("rpaths"), list) or any(not isinstance(item, str) for item in image["rpaths"]):
            _fail(f"Mach-O image {ref} has malformed RPATH evidence")
        if not isinstance(image.get("remove_rpaths"), list) or any(not isinstance(item, str) for item in image["remove_rpaths"]):
            _fail(f"Mach-O image {ref} has malformed external RPATH evidence")
        if not isinstance(image.get("architectures"), list) or image["architectures"] != [target["architecture"]]:
            _fail(f"Mach-O image {ref} has the wrong architecture evidence")
        minimum = image.get("minimum_macos")
        if not isinstance(minimum, str) or re.fullmatch(r"\d+(?:\.\d+){1,2}", minimum) is None:
            _fail(f"Mach-O image {ref} has invalid minimum macOS evidence")
        if image.get("signature") not in {"unsigned", "ad-hoc", "signed", "invalidated", "invalid"}:
            _fail(f"Mach-O image {ref} has invalid signature evidence")
        if image.get("dylib_id") is not None and not isinstance(image.get("dylib_id"), str):
            _fail(f"Mach-O image {ref} has malformed LC_ID_DYLIB evidence")
    loads = plan.get("loads")
    if not isinstance(loads, list):
        _fail("Mach-O relocation load graph is missing")
    seen_loads: set[tuple[str, str]] = set()
    for load in loads:
        if not isinstance(load, dict) or load.get("image_ref") not in refs:
            _fail("Mach-O relocation load edge names an unknown image")
        target_kind = load.get("target_kind")
        if target_kind == "system":
            system_path = load.get("system_path")
            if not isinstance(system_path, str) or not system_path.startswith(("/System/Library/", "/usr/lib/", "/System/Volumes/Preboot/Cryptexes/OS/System/Library/")):
                _fail("Mach-O relocation plan has an invalid Apple system edge")
        elif target_kind in {"origin", "package"}:
            if load.get("target_ref") not in refs or not load["target_ref"].startswith(target_kind + ":"):
                _fail("Mach-O relocation load edge names an unknown target image")
        else:
            _fail("Mach-O relocation load edge has an invalid target kind")
        install_name = load.get("install_name")
        if not isinstance(install_name, str) or not install_name:
            _fail("Mach-O relocation load edge has no original install name")
        key = (load["image_ref"], install_name)
        if key in seen_loads:
            _fail("Mach-O relocation load graph repeats an edge")
        seen_loads.add(key)
    return plan


def relative_loader_name(image_bundle_path: str, target_bundle_path: str) -> str:
    image = bundle_relative(image_bundle_path, "Mach-O image path")
    target = bundle_relative(target_bundle_path, "Mach-O dependency path")
    relative = posixpath.relpath(target, posixpath.dirname(image))
    return "@loader_path" if relative == "." else f"@loader_path/{relative}"


def _safe_id(value: Any, label: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", value) is None:
        _fail(f"{label} is not a safe stable identifier")
    return value


def admitted_origin_wrapper(
    receipt_bytes: bytes, origin_id: str, plan_sha256: str
) -> dict[str, Any]:
    try:
        receipt_text = receipt_bytes.decode("utf-8")
        receipt = json.loads(receipt_text)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        _fail(f"origin receipt is not valid UTF-8 JSON: {error}")
    if not isinstance(receipt, dict):
        _fail("origin receipt must be a JSON object")
    _safe_id(origin_id, "origin ID")
    return {
        "schema": 1,
        "kind": "admitted-origin-receipt",
        "origin_id": origin_id,
        "source_receipt_sha256": hashlib.sha256(receipt_bytes).hexdigest(),
        "source_receipt_json": receipt_text,
        "macho_relocation_inputs": {
            "schema": 1,
            "manifest_sha256": require_sha256(plan_sha256, "Mach-O relocation plan"),
            "origin_id": origin_id,
        },
    }


def unwrap_admitted_origin(receipt: dict[str, Any], expected_origin: str) -> tuple[dict[str, Any], dict[str, Any] | None]:
    if receipt.get("kind") != "admitted-origin-receipt":
        return receipt, None
    if receipt.get("schema") != 1 or receipt.get("origin_id") != expected_origin:
        _fail(f"admitted receipt is not for origin {expected_origin}")
    text = receipt.get("source_receipt_json")
    if not isinstance(text, str):
        _fail("admitted origin receipt does not retain the original receipt bytes")
    digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
    if require_sha256(receipt.get("source_receipt_sha256"), "source receipt") != digest:
        _fail("admitted origin receipt source bytes differ from their digest")
    try:
        raw = json.loads(text)
    except json.JSONDecodeError as error:
        _fail(f"admitted origin contains invalid source receipt JSON: {error}")
    if not isinstance(raw, dict):
        _fail("admitted origin source receipt must be an object")
    attachment = receipt.get("macho_relocation_inputs")
    if (
        not isinstance(attachment, dict)
        or attachment.get("schema") != 1
        or attachment.get("origin_id") != expected_origin
    ):
        _fail("admitted origin receipt is missing its typed relocation-plan attachment")
    require_sha256(attachment.get("manifest_sha256"), "attached Mach-O relocation plan")
    return raw, attachment


def validate_dotnet_runtime_receipt(
    receipt: dict[str, Any],
    runtime_root: Path,
    target: str,
    pin_path: Path,
    source_archive: Path | None = None,
) -> dict[str, Any]:
    expected_receipt = create_dotnet_runtime_receipt(
        runtime_root, pin_path, target, source_archive
    )
    if receipt != expected_receipt:
        _fail(".NET runtime receipt differs from the independently checked package pin")
    if receipt.get("schema") != RUNTIME_RECEIPT_SCHEMA or receipt.get("kind") != "dotnet-runtime":
        _fail(".NET runtime receipt has an unsupported schema or kind")
    expected_rid = "osx-arm64" if target == "aarch64-apple-darwin" else "osx-x64"
    if receipt.get("target") != target or receipt.get("runtime_identifier") != expected_rid:
        _fail(".NET runtime receipt target does not match the requested bundle")
    package = receipt.get("package")
    if not isinstance(package, dict) or package.get("name") != "Microsoft.NETCore.App":
        _fail(".NET runtime receipt must identify Microsoft.NETCore.App")
    version = package.get("version")
    if not isinstance(version, str) or re.fullmatch(r"\d+\.\d+\.\d+", version) is None:
        _fail(".NET runtime receipt has an invalid runtime version")
    provenance = receipt.get("distribution")
    if not isinstance(provenance, dict) or provenance.get("kind") not in {
        "verified-archive",
        "pinned-nix-output",
    }:
        _fail(".NET runtime receipt must name a verified archive or pinned Nix output")
    locator = provenance.get("locator")
    if not isinstance(locator, str) or not locator.strip() or "\n" in locator:
        _fail(".NET runtime receipt has no stable distribution locator")
    require_sha256(provenance.get("identity_sha256"), ".NET distribution identity")

    observed = file_tree_manifest(runtime_root)
    expected_files = receipt.get("files")
    if not isinstance(expected_files, dict) or not expected_files or observed != expected_files:
        _fail(".NET runtime files differ from the complete admitted file manifest")
    expected_tree = require_sha256(receipt.get("root_tree_sha256"), ".NET runtime root tree")
    if file_tree_sha256(observed) != expected_tree:
        _fail(".NET runtime root differs from its pinned tree identity")
    if {path.split("/", 1)[0] for path in observed} != {
        "dotnet", "host", "shared", "LICENSE.txt", "ThirdPartyNotices.txt"
    }:
        _fail(".NET runtime root contains unadmitted SDK or unrelated files")
    if "dotnet" not in observed or observed["dotnet"].get("kind") != "file":
        _fail(".NET runtime root is missing its regular dotnet host")
    if not any(path.startswith("host/fxr/") for path in observed):
        _fail(".NET runtime root is missing host/fxr")
    if not any(path.startswith("shared/Microsoft.NETCore.App/") for path in observed):
        _fail(".NET runtime root is missing Microsoft.NETCore.App")
    notices = receipt.get("notices")
    if not isinstance(notices, dict) or set(notices) != {"license", "third_party"}:
        _fail(".NET runtime receipt must pin the license and third-party notices")
    for label, item in notices.items():
        if not isinstance(item, dict):
            _fail(f".NET runtime {label} notice record is malformed")
        relative = _safe_relative(item.get("path"), f".NET runtime {label} notice path")
        entry = observed.get(relative)
        if entry is None or entry.get("kind") != "file":
            _fail(f".NET runtime {label} notice is not a regular file in the admitted root")
        if require_sha256(item.get("sha256"), f".NET runtime {label} notice") != entry["sha256"]:
            _fail(f".NET runtime {label} notice hash differs from the admitted bytes")
    return observed


def create_dotnet_runtime_receipt(
    runtime_root: Path,
    pin_path: Path,
    target: str,
    source_archive: Path | None = None,
) -> dict[str, Any]:
    try:
        pin = json.loads(pin_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        _fail(f"cannot read pinned .NET distribution descriptor: {error}")
    if not isinstance(pin, dict) or pin.get("schema") != 1:
        _fail(".NET distribution descriptor must use schema 1")
    expected_rid = "osx-arm64" if target == "aarch64-apple-darwin" else "osx-x64"
    if pin.get("target") != target or pin.get("runtime_identifier") != expected_rid:
        _fail(".NET distribution descriptor target does not match the requested bundle")
    package = pin.get("package")
    if not isinstance(package, dict) or package.get("name") != "Microsoft.NETCore.App":
        _fail(".NET distribution descriptor must identify Microsoft.NETCore.App")
    version = package.get("version")
    if not isinstance(version, str) or re.fullmatch(r"\d+\.\d+\.\d+", version) is None:
        _fail(".NET distribution descriptor has an invalid runtime version")
    distribution = pin.get("distribution")
    if not isinstance(distribution, dict):
        _fail(".NET distribution descriptor is missing its upstream identity")
    kind = distribution.get("kind")
    locator = distribution.get("locator")
    if not isinstance(locator, str) or not locator.strip() or "\n" in locator:
        _fail(".NET distribution descriptor has no stable source locator")
    if kind == "verified-archive":
        expected_archive_sha = require_sha256(distribution.get("archive_sha256"), ".NET source archive")
        if source_archive is None or _sha256(source_archive) != expected_archive_sha:
            _fail(".NET source archive is missing or differs from its pinned digest")
        identity_sha = expected_archive_sha
    elif kind == "pinned-nix-output":
        if source_archive is not None:
            _fail("a Nix output pin must not be paired with a source-archive override")
        store_output = distribution.get("store_output")
        derivation_sha = require_sha256(distribution.get("derivation_sha256"), ".NET derivation")
        root_resolved = runtime_root.resolve(strict=True)
        if not isinstance(store_output, str) or re.fullmatch(r"[0-9a-z]{32}-[^/]+", store_output) is None:
            _fail(".NET Nix pin must name one exact store output")
        if root_resolved != Path("/nix/store") / store_output:
            _fail(".NET runtime root is not the exact Nix output named by its pin")
        identity_sha = derivation_sha
    else:
        _fail(".NET distribution kind must be verified-archive or pinned-nix-output")

    observed = file_tree_manifest(runtime_root)
    if kind == "verified-archive" and archive_tree_manifest(source_archive) != observed:
        _fail(".NET runtime root is not the exact file tree in its pinned archive")
    expected_tree = require_sha256(pin.get("root_tree_sha256"), ".NET runtime root tree")
    if file_tree_sha256(observed) != expected_tree:
        _fail(".NET runtime root differs from the pinned distribution tree")
    runtime_versions = {
        path.split("/")[2]
        for path in observed
        if path.startswith("shared/Microsoft.NETCore.App/")
    }
    if runtime_versions != {version}:
        _fail(".NET runtime root must contain exactly the pinned shared runtime version")
    hostfxr_versions = {
        path.split("/")[2]
        for path in observed
        if path.startswith("host/fxr/")
    }
    if hostfxr_versions != {version}:
        _fail(".NET runtime root must contain exactly the pinned host/fxr version")
    notices_pin = pin.get("notices")
    if not isinstance(notices_pin, dict) or set(notices_pin) != {"license", "third_party"}:
        _fail(".NET distribution pin must name both required notices")
    notices: dict[str, dict[str, str]] = {}
    for label, value in notices_pin.items():
        if not isinstance(value, dict):
            _fail(f".NET {label} notice pin is malformed")
        relative = _safe_relative(value.get("path"), f".NET {label} notice path")
        entry = observed.get(relative)
        if entry is None or entry.get("kind") != "file":
            _fail(f".NET {label} notice must be present in the pinned runtime root")
        notice_sha = require_sha256(value.get("sha256"), f".NET {label} notice")
        if entry["sha256"] != notice_sha:
            _fail(f".NET {label} notice differs from its pinned source bytes")
        notices[label] = {"path": relative, "sha256": notice_sha}

    receipt = {
        "schema": RUNTIME_RECEIPT_SCHEMA,
        "kind": "dotnet-runtime",
        "target": target,
        "runtime_identifier": expected_rid,
        "package": {"name": "Microsoft.NETCore.App", "version": version},
        "distribution": {
            "kind": kind,
            "locator": locator,
            "identity_sha256": identity_sha,
        },
        "root_tree_sha256": expected_tree,
        "files": observed,
        "notices": notices,
        "pin_descriptor_sha256": _sha256(pin_path),
    }
    return receipt
