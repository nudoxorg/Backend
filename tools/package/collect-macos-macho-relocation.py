#!/usr/bin/env python3
"""Collect a pinned Mach-O dependency closure and attach it to origin receipts.

The collector reads admitted app/helper/runtime inputs and pinned package roots.
It never copies or rewrites Mach-O files. Relocation happens later, in a fresh
bundle staging directory, and is independently checked by the bundle auditor.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import posixpath
import platform
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from macho_relocation import (
    RelocationInputError,
    admitted_origin_wrapper,
    canonical_json_sha256,
    file_tree_manifest,
    file_tree_sha256,
    require_sha256,
    validate_dotnet_runtime_receipt,
    validate_relocation_plan,
    write_new_bytes,
)


PACKAGE_DIR = Path(__file__).resolve().parent
_bundle_spec = importlib.util.spec_from_file_location(
    "macos_investor_bundle", PACKAGE_DIR / "macos-investor-bundle.py"
)
if _bundle_spec is None or _bundle_spec.loader is None:
    raise RuntimeError("cannot load the shared bundle admission and Mach-O inspection code")
bundle = importlib.util.module_from_spec(_bundle_spec)
sys.modules[_bundle_spec.name] = bundle
_bundle_spec.loader.exec_module(bundle)


@dataclass(frozen=True)
class InputRoot:
    role: str
    path: Path
    kind: str
    identity: str
    bundle_prefix: str | None = None
    package_id: str | None = None


def fail(message: str) -> None:
    raise RelocationInputError(message)


def parse_pairs(values: list[str], label: str) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for value in values:
        if "=" not in value:
            fail(f"{label} must use ID=PATH syntax")
        key, raw_path = value.split("=", 1)
        if not key or not raw_path or key in result:
            fail(f"{label} has an empty or duplicate ID")
        result[key] = Path(raw_path).resolve(strict=True)
    return result


def read_json(path: Path, label: str) -> tuple[dict[str, Any], bytes]:
    try:
        raw = path.read_bytes()
        value = json.loads(raw)
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {label}: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must be a JSON object")
    return value, raw


def exact_file_hashes(root: Path) -> dict[str, str]:
    observed: dict[str, str] = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            fail(f"origin root contains a symlink: {path.relative_to(root)}")
        if path.is_file():
            observed[path.relative_to(root).as_posix()] = bundle.sha256(path)
    return observed


def source_origin_records(
    args: argparse.Namespace,
    spec: dict[str, Any],
    source: dict[str, str],
    receipts: dict[str, Path],
) -> tuple[dict[str, dict[str, Any]], dict[str, InputRoot], dict[str, Path]]:
    target = spec["target"]["triple"]
    arch = spec["target"]["architecture"]
    expected_rid = "osx-arm64" if target == "aarch64-apple-darwin" else "osx-x64"
    origins: dict[str, dict[str, Any]] = {}
    roots: dict[str, InputRoot] = {}
    owner_source_paths: dict[str, Path] = {}

    app_receipt_path = receipts["application"]
    app_receipt, app_outputs = bundle.validate_app_build(
        app_receipt_path,
        args.artifact_dir.resolve(strict=True),
        source,
        target,
        args.expected_runner_sha256,
    )
    app_root = args.artifact_dir.resolve(strict=True)
    app_processes = {
        "backend-desktop": "Contents/MacOS/backend-desktop",
        "backend-mcp": "Contents/MacOS/backend-mcp",
        "backend-locald": "Contents/MacOS/backend-locald",
    }
    app_tree = file_tree_manifest(app_root)
    origins["application"] = {
        "receipt_kind": "application",
        "receipt_sha256": hashlib.sha256(app_receipt_path.read_bytes()).hexdigest(),
        "root_tree_sha256": file_tree_sha256(app_tree),
        "bundle_prefix": "Contents/MacOS",
        "process_roots": app_processes,
    }
    roots["application"] = InputRoot("application", app_root, "origin", "application", "Contents/MacOS")
    for name in app_processes:
        owner_source_paths[app_processes[name]] = app_outputs[name]

    roslyn_receipt_path = receipts["roslyn"]
    roslyn_root = args.roslyn_dir.resolve(strict=True)
    roslyn_receipt, roslyn_files = bundle.validate_roslyn_build(
        roslyn_receipt_path, roslyn_root, args.source_root.resolve(strict=True), source, target
    )
    roslyn_tree = file_tree_manifest(roslyn_root)
    origins["roslyn"] = {
        "receipt_kind": "roslyn",
        "receipt_sha256": hashlib.sha256(roslyn_receipt_path.read_bytes()).hexdigest(),
        "root_tree_sha256": file_tree_sha256(roslyn_tree),
        "bundle_prefix": "Contents/Resources/Helpers/csharp",
        "process_roots": {},
    }
    roots["roslyn"] = InputRoot("roslyn", roslyn_root, "origin", "roslyn", "Contents/Resources/Helpers/csharp")

    helpers_receipt_path = receipts["helpers"]
    helpers_root = args.helpers_dir.resolve(strict=True)
    helpers_receipt, helper_files = bundle.validate_helper_payload(
        helpers_receipt_path, helpers_root, args.source_root.resolve(strict=True), source, target
    )
    helpers_tree = file_tree_manifest(helpers_root)
    origins["helpers"] = {
        "receipt_kind": "helpers",
        "receipt_sha256": hashlib.sha256(helpers_receipt_path.read_bytes()).hexdigest(),
        "root_tree_sha256": file_tree_sha256(helpers_tree),
        "bundle_prefix": "Contents/Resources/Helpers",
        "process_roots": {
            "go/oracle": "Contents/Resources/Helpers/go/oracle",
            "python/pyrefly": "Contents/Resources/Helpers/python/pyrefly",
            "typescript/node/bin/node": "Contents/Resources/Helpers/typescript/node/bin/node",
        },
    }
    roots["helpers"] = InputRoot("helpers", helpers_root, "origin", "helpers", "Contents/Resources/Helpers")
    for relative, stage_path in origins["helpers"]["process_roots"].items():
        owner_source_paths[stage_path] = helpers_root / relative

    dotnet_receipt_path = receipts["dotnet-runtime"]
    dotnet_root = args.dotnet_root.resolve(strict=True)
    dotnet_receipt, _ = read_json(dotnet_receipt_path, ".NET runtime receipt")
    dotnet_files = validate_dotnet_runtime_receipt(
        dotnet_receipt,
        dotnet_root,
        target,
        args.dotnet_pin.resolve(strict=True),
        args.dotnet_source_archive.resolve(strict=True) if args.dotnet_source_archive else None,
    )
    dotnet_process = "Contents/Resources/dotnet/dotnet"
    origins["dotnet-runtime"] = {
        "receipt_kind": "dotnet-runtime",
        "receipt_sha256": hashlib.sha256(dotnet_receipt_path.read_bytes()).hexdigest(),
        "root_tree_sha256": dotnet_receipt["root_tree_sha256"],
        "bundle_prefix": "Contents/Resources/dotnet",
        "process_roots": {"dotnet": dotnet_process},
    }
    roots["dotnet-runtime"] = InputRoot("dotnet-runtime", dotnet_root, "origin", "dotnet-runtime", "Contents/Resources/dotnet")
    owner_source_paths[dotnet_process] = dotnet_root / "dotnet"

    expected_receipts = {"application", "roslyn", "helpers", "dotnet-runtime"}
    if set(receipts) != expected_receipts:
        fail("exactly one receipt is required for application, Roslyn, helpers, and .NET runtime")
    if expected_rid != dotnet_receipt.get("runtime_identifier") or arch != bundle.MACOS_TARGETS[target]:
        fail("origin receipt target identities do not agree")
    return origins, roots, owner_source_paths


def package_records(
    spec: dict[str, Any],
    root_paths: dict[str, Path],
    archives: dict[str, Path],
) -> tuple[dict[str, Any], dict[str, InputRoot]]:
    raw_packages = spec.get("packages")
    if not isinstance(raw_packages, list):
        fail("relocation spec packages must be an array")
    packages: dict[str, Any] = {}
    roots: dict[str, InputRoot] = {}
    for item in raw_packages:
        if not isinstance(item, dict):
            fail("relocation package pin is malformed")
        package_id = item.get("id")
        if not isinstance(package_id, str) or re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", package_id) is None or package_id in packages:
            fail("relocation package ID is unsafe or duplicated")
        role = item.get("root_role")
        if not isinstance(role, str) or role not in root_paths:
            fail(f"relocation package {package_id} has no supplied root")
        root = root_paths[role]
        manifest = file_tree_manifest(root)
        tree_sha = file_tree_sha256(manifest)
        if require_sha256(item.get("root_tree_sha256"), f"package {package_id} tree") != tree_sha:
            fail(f"pinned package root differs from its complete content tree: {package_id}")
        provenance = item.get("provenance")
        if not isinstance(provenance, dict) or provenance.get("kind") not in {"verified-archive", "pinned-nix-output"}:
            fail(f"package {package_id} requires explicit archive or Nix provenance")
        identity = require_sha256(provenance.get("identity_sha256"), f"package {package_id} provenance")
        if provenance["kind"] == "pinned-nix-output":
            store_output = item.get("store_output")
            if not isinstance(store_output, str) or re.fullmatch(r"[0-9a-z]{32}-[^/]+", store_output) is None:
                fail(f"pinned Nix package {package_id} must name one exact store output")
            if root.resolve(strict=True) != Path("/nix/store") / store_output:
                fail(f"pinned Nix package path differs from its declared store output: {package_id}")
            if package_id in archives:
                fail(f"Nix package {package_id} must not use an archive override")
        else:
            archive = archives.get(package_id)
            if archive is None or bundle.sha256(archive) != identity:
                fail(f"verified archive is missing or differs from its pin: {package_id}")
        package = {
            "name": item.get("name"),
            "version": item.get("version"),
            "root_tree_sha256": tree_sha,
            "provenance": {
                "kind": provenance["kind"],
                "locator": provenance.get("locator"),
                "identity_sha256": identity,
            },
            "notices": [],
        }
        if not isinstance(package["name"], str) or not package["name"].strip() or not isinstance(package["version"], str) or not package["version"].strip():
            fail(f"package {package_id} needs a name and version")
        notices = item.get("notices")
        if not isinstance(notices, list) or not notices:
            fail(f"package {package_id} must provide explicit license/notice inputs")
        for notice in notices:
            if not isinstance(notice, dict) or notice.get("root_role") not in root_paths:
                fail(f"package {package_id} notice source is not supplied")
            relative = str(notice.get("relative_path", ""))
            path = root_paths[notice["root_role"]] / relative
            if Path(relative).is_absolute() or ".." in Path(relative).parts or not path.is_file() or path.is_symlink():
                fail(f"package {package_id} notice path is unsafe or missing")
            content = path.read_bytes()
            digest = require_sha256(notice.get("sha256"), f"package {package_id} notice")
            if hashlib.sha256(content).hexdigest() != digest:
                fail(f"package {package_id} notice bytes differ from their pin")
            package["notices"].append(
                {
                    "name": notice.get("name"),
                    "sha256": digest,
                    "content_base64": base64.b64encode(content).decode("ascii"),
                    "source_identity": {
                        "root_role": notice["root_role"],
                        "relative_path": Path(relative).as_posix(),
                    },
                }
            )
        packages[package_id] = package
        roots[package_id] = InputRoot(package_id, root, "package", package_id)
    return packages, roots


def macho_images(roots: dict[str, InputRoot], arch: str) -> tuple[dict[str, dict[str, Any]], dict[Path, str]]:
    images: dict[str, dict[str, Any]] = {}
    path_to_ref: dict[Path, str] = {}
    for root_id, root in roots.items():
        for path in sorted(root.path.rglob("*")):
            if not path.is_file():
                continue
            try:
                with path.open("rb") as stream:
                    magic = stream.read(4)
            except OSError as error:
                fail(f"cannot inspect input image {path}: {error}")
            if magic not in bundle.MACHO_MAGICS:
                continue
            description = bundle.run(["file", "-b", str(path)])
            if "Mach-O" not in description:
                fail(f"Mach-O header candidate is not a recognized image: {path}")
            relative = path.relative_to(root.path).as_posix()
            ref = f"{root.kind}:{root.identity}:{relative}"
            stage_path = f"{root.bundle_prefix}/{relative}" if root.kind == "origin" else f"Contents/Frameworks/{root.identity}/{relative}"
            architectures = bundle.run(["lipo", "-archs", str(path)]).split()
            if architectures != [arch]:
                fail(f"input Mach-O {ref} has slices {architectures}, expected only {arch}")
            rpaths, dylib_id = bundle.inspect_load_metadata(path)
            record = {
                "ref": ref,
                "kind": root.kind,
                "source_root": root_id,
                "source_relative_path": relative,
                "bundle_path": stage_path,
                "sha256": bundle.sha256(path),
                "architectures": architectures,
                "minimum_macos": bundle.minimum_macos(path),
                "signature": bundle.inspect_signature(path),
                "dylib_id": dylib_id,
                "rpaths": rpaths,
                "remove_rpaths": [],
            }
            if root.kind == "package":
                record["package_id"] = root.identity
                record["package_relative_path"] = relative
            else:
                record["origin_id"] = root.identity
            if ref in images:
                fail(f"duplicate Mach-O image identity: {ref}")
            images[ref] = record
            path_to_ref[path] = ref
    return images, path_to_ref


def _relative_source_path(path: Path, root: InputRoot) -> str | None:
    try:
        path.absolute().relative_to(root.path.absolute())
    except ValueError:
        return None
    try:
        if not bundle.path_within(path.resolve(strict=True), root.path.resolve(strict=True)):
            fail(f"input symlink escapes its declared root: {path}")
    except OSError:
        return None
    return path.relative_to(root.path).as_posix()


def collect_plan(
    spec: dict[str, Any],
    origins: dict[str, dict[str, Any]],
    input_roots: dict[str, InputRoot],
    owner_paths: dict[str, Path],
    packages: dict[str, Any],
    arch: str,
) -> dict[str, Any]:
    images, path_to_ref = macho_images(input_roots, arch)
    root_paths = {key: root.path.resolve(strict=True) for key, root in input_roots.items()}
    process_root_rels = set(owner_paths)
    process_root_sources = {key: value.resolve(strict=True) for key, value in owner_paths.items()}
    origin_image_refs = [ref for ref, record in images.items() if record["kind"] == "origin"]
    loads_by_edge: dict[tuple[str, str], dict[str, Any]] = {}
    loaded: dict[str, set[str]] = {root: set() for root in process_root_rels}
    active: dict[str, set[str]] = {root: set() for root in process_root_rels}
    load_cache: dict[Path, tuple[list[str], list[str], str | None]] = {}

    def image_path(ref: str) -> Path:
        record = images[ref]
        root = input_roots[record["source_root"]]
        return root.path / record["source_relative_path"]

    def load_commands(path: Path) -> tuple[list[str], list[str], str | None]:
        canonical = path.resolve(strict=True)
        if canonical not in load_cache:
            output = bundle.run(["otool", "-L", str(canonical)])
            names = []
            for line in output.splitlines()[1:]:
                line = line.strip()
                if line:
                    names.append(line.split(" (compatibility version ", 1)[0])
            rpaths, dylib_id = bundle.inspect_load_metadata(canonical)
            load_cache[canonical] = ([name for name in names if name != dylib_id], rpaths, dylib_id)
        return load_cache[canonical]

    def expand_rpath(value: str, image: Path, owner: Path) -> str:
        if value.startswith("/"):
            expanded = Path(value)
        else:
            expanded = bundle.expand_image_path(value, image, owner)
            if expanded is None:
                fail(f"unsupported LC_RPATH origin in {image}: {value}")
        return os.path.normpath(str(expanded))

    def classify(candidate: Path) -> dict[str, Any] | None:
        rendered = os.path.normpath(str(candidate))
        if bundle.is_system_path(rendered):
            if bundle.system_cache_has_arch(Path(rendered), arch):
                return {"kind": "system", "path": rendered}
            return None
        if not candidate.exists():
            return None
        try:
            canonical = candidate.resolve(strict=True)
        except OSError:
            return None
        if not canonical.is_file():
            return None
        matches: list[tuple[int, InputRoot, str]] = []
        for root_id, root in input_roots.items():
            relative = _relative_source_path(candidate, root)
            if relative is None:
                relative = _relative_source_path(canonical, root)
            if relative is not None:
                matches.append((len(str(root.path.resolve(strict=True))), root, relative))
        if not matches:
            fail(f"Mach-O load resolves outside all pinned roots: {rendered}")
        matches.sort(key=lambda value: value[0], reverse=True)
        best_length = matches[0][0]
        best = [row for row in matches if row[0] == best_length]
        identities = {(row[1].kind, row[1].identity) for row in best}
        if len(identities) != 1:
            fail(f"Mach-O load has ambiguous source-root ownership: {rendered}")
        _, root, relative = best[0]
        ref = f"{root.kind}:{root.identity}:{relative}"
        if ref not in images:
            # A valid load target must be a Mach-O image captured in this plan.
            with candidate.open("rb") as stream:
                magic = stream.read(4)
            if magic not in bundle.MACHO_MAGICS:
                fail(f"non-Mach-O file is referenced as a dynamic library: {rendered}")
            fail(f"Mach-O dependency was not inventoried: {rendered}")
        return {"kind": root.kind, "ref": ref, "path": rendered}

    def resolve_load(
        install_name: str,
        image: Path,
        owner: Path,
        effective_rpaths: tuple[str, ...],
    ) -> dict[str, Any]:
        if install_name.startswith("@rpath/"):
            suffix = install_name[len("@rpath/") :]
            for rpath in effective_rpaths:
                result = classify(Path(rpath) / suffix)
                if result is not None:
                    return result
            fail(f"unresolved Mach-O @rpath load in {image}: {install_name}")
        expanded = bundle.expand_image_path(install_name, image, owner)
        candidate = expanded if expanded is not None else (Path(install_name) if install_name.startswith("/") else None)
        if candidate is None:
            fail(f"unsupported Mach-O install name in {image}: {install_name}")
        result = classify(candidate)
        if result is None:
            if bundle.is_system_path(os.path.normpath(str(candidate))):
                fail(f"Apple system dependency lacks requested architecture: {candidate}")
            fail(f"unresolved Mach-O dependency in {image}: {install_name}")
        return result

    def visit(ref: str, owner_rel: str) -> None:
        if ref in loaded[owner_rel] or ref in active[owner_rel]:
            return
        path = image_path(ref)
        owner = process_root_sources[owner_rel]
        dependencies, declared_rpaths, _ = load_commands(path)
        own_rpaths = tuple(expand_rpath(item, path, owner) for item in declared_rpaths)
        effective_rpaths = own_rpaths + tuple(
            expand_rpath(item, owner, owner) for item in load_commands(owner)[1]
        )
        loaded[owner_rel].add(ref)
        active[owner_rel].add(ref)
        try:
            for install_name in dependencies:
                target = resolve_load(install_name, path, owner, effective_rpaths)
                if target["kind"] == "system":
                    key = (ref, install_name)
                    row = {
                        "image_ref": ref,
                        "install_name": install_name,
                        "target_kind": "system",
                        "system_path": target["path"],
                    }
                    previous = loads_by_edge.get(key)
                    if previous is not None and previous != row:
                        fail(f"same image/load resolves differently in process contexts: {ref}: {install_name}")
                    loads_by_edge[key] = row
                    continue
                key = (ref, install_name)
                row = {
                    "image_ref": ref,
                    "install_name": install_name,
                    "target_kind": target["kind"],
                    "target_ref": target["ref"],
                }
                previous = loads_by_edge.get(key)
                if previous is not None and previous != row:
                    fail(f"same image/load resolves to different targets in process contexts: {ref}: {install_name}")
                loads_by_edge[key] = row
                visit(target["ref"], owner_rel)
        finally:
            active[owner_rel].remove(ref)

    for ref in origin_image_refs:
        record = images[ref]
        contexts = bundle.process_contexts_for_relative(record["bundle_path"], process_root_rels)
        for owner_rel in contexts:
            visit(ref, owner_rel)

    # The bundle auditor gives staged Frameworks images every process context.
    # Apply the same rule to discovered package images, repeating until closure
    # stops growing so dynamically loaded package images are included too.
    visited_package_contexts: set[tuple[str, str]] = set()
    while True:
        reached_packages = {
            ref for owner_refs in loaded.values() for ref in owner_refs
            if images[ref]["kind"] == "package"
        }
        pending = [
            (ref, owner_rel)
            for ref in reached_packages
            for owner_rel in sorted(process_root_rels)
            if (ref, owner_rel) not in visited_package_contexts
        ]
        if not pending:
            break
        for ref, owner_rel in pending:
            visited_package_contexts.add((ref, owner_rel))
            visit(ref, owner_rel)

    # Only retain package images reached through an admitted process context.
    used_package_refs = {
        ref for owner_refs in loaded.values() for ref in owner_refs
        if images[ref]["kind"] == "package"
    }
    used_origin_refs = {
        ref for owner_refs in loaded.values() for ref in owner_refs
        if images[ref]["kind"] == "origin"
    }
    keep_refs = used_package_refs | used_origin_refs
    images = {ref: record for ref, record in images.items() if ref in keep_refs}
    loads = [
        row for row in loads_by_edge.values()
        if row["image_ref"] in keep_refs
        and (row["target_kind"] == "system" or row["target_ref"] in keep_refs)
    ]
    used_package_ids = {images[ref]["package_id"] for ref in used_package_refs}
    packages = {key: value for key, value in packages.items() if key in used_package_ids}

    # A non-system RPATH is retained only if it is rooted in the same admitted
    # input tree as the image. Other inputs are reached through exact load rows
    # and will be addressed relative to their staged image after copying.
    virtual_bundle = Path("/_nudox_relocation_bundle")
    for ref, record in images.items():
        root = input_roots[record["source_root"]]
        path = image_path(ref)
        owner_rels = bundle.process_contexts_for_relative(record["bundle_path"], process_root_rels)
        owners = [process_root_sources[owner] for owner in owner_rels]
        virtual_image = virtual_bundle / record["bundle_path"]
        remove_values: set[str] = set()
        for raw in record["rpaths"]:
            if raw.startswith("/"):
                expanded_paths = [raw]
                remove_values.add(raw)
            else:
                outside_virtual_bundle = False
                for owner_rel in owner_rels:
                    virtual_owner = virtual_bundle / owner_rel
                    try:
                        bundle.expand_runpath(raw, virtual_image, virtual_owner, virtual_bundle)
                    except bundle.PackageError:
                        outside_virtual_bundle = True
                        break
                if not outside_virtual_bundle:
                    continue
                expanded_paths = [expand_rpath(raw, path, owner) for owner in owners]
            for expanded in expanded_paths:
                if bundle.is_system_path(expanded):
                    continue
                candidate = Path(expanded)
                matched_root = any(_relative_source_path(candidate, other.path) is not None for other in input_roots.values())
                if not matched_root:
                    fail(f"LC_RPATH resolves outside all pinned roots: {path}: {raw}")
                remove_values.add(raw)
        record["remove_rpaths"] = [raw for raw in record["rpaths"] if raw in remove_values]

    for origin_id, origin in origins.items():
        origin["image_hashes"] = {
            record["bundle_path"]: record["sha256"]
            for record in images.values()
            if record["kind"] == "origin" and record["origin_id"] == origin_id
        }
    plan = {
        "schema": 1,
        "kind": "macho-relocation-plan",
        "source": {"git_revision": spec["source"]["git_revision"], "git_tree": spec["source"]["git_tree"]},
        "target": {"triple": spec["target"]["triple"], "architecture": arch},
        "origins": origins,
        "packages": packages,
        "images": sorted(images.values(), key=lambda row: row["ref"]),
        "loads": sorted(loads, key=lambda row: (row["image_ref"], row["install_name"])),
    }
    return validate_relocation_plan(plan)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--expected-revision", required=True)
    parser.add_argument("--expected-tree", required=True)
    parser.add_argument("--expected-runner-sha256", required=True)
    parser.add_argument("--artifact-dir", required=True, type=Path)
    parser.add_argument("--build-receipt", required=True, type=Path)
    parser.add_argument("--roslyn-dir", required=True, type=Path)
    parser.add_argument("--roslyn-receipt", required=True, type=Path)
    parser.add_argument("--helpers-dir", required=True, type=Path)
    parser.add_argument("--helpers-receipt", required=True, type=Path)
    parser.add_argument("--dotnet-root", required=True, type=Path)
    parser.add_argument("--dotnet-receipt", required=True, type=Path)
    parser.add_argument("--dotnet-pin", required=True, type=Path)
    parser.add_argument("--dotnet-source-archive", type=Path)
    parser.add_argument("--package-spec", required=True, type=Path, help="pinned package identities, roots, and notice paths")
    parser.add_argument("--package-root", action="append", default=[], metavar="ROLE=PATH")
    parser.add_argument("--notice-root", action="append", default=[], metavar="ROLE=PATH")
    parser.add_argument("--package-archive", action="append", default=[], metavar="PACKAGE=PATH")
    parser.add_argument("--output-dir", required=True, type=Path, help="new directory for the shared plan and wrapped receipts")
    parser.add_argument("--target", choices=bundle.MACOS_TARGETS, default="aarch64-apple-darwin")
    args = parser.parse_args()
    bundle.macos_tools()
    source_root = args.source_root.resolve(strict=True)
    source = bundle.validate_source(source_root, args.expected_revision, args.expected_tree)
    target_arch = bundle.MACOS_TARGETS[args.target]
    if platform.system() != "Darwin" or platform.machine().lower() not in {"arm64", "aarch64"} and target_arch == "arm64":
        fail("Mach-O relocation inventory must be collected on a native host for the selected target")
    if args.output_dir.exists() or args.output_dir.is_symlink():
        fail(f"refusing to overwrite relocation output directory: {args.output_dir}")

    receipts = {
        "application": args.build_receipt.resolve(strict=True),
        "roslyn": args.roslyn_receipt.resolve(strict=True),
        "helpers": args.helpers_receipt.resolve(strict=True),
        "dotnet-runtime": args.dotnet_receipt.resolve(strict=True),
    }
    origins, origin_roots, owner_paths = source_origin_records(args, {"target": {"triple": args.target, "architecture": target_arch}}, source, receipts)
    package_roots = parse_pairs(args.package_root, "package root")
    notice_roots = parse_pairs(args.notice_root, "notice root")
    package_archives = parse_pairs(args.package_archive, "package archive")
    input_roots = dict(origin_roots)
    input_roots.update(
        {role: InputRoot(role, path, "package", role, package_id=role) for role, path in package_roots.items()}
    )
    # A notice source is authenticated through each notice's own digest and
    # pinned package provenance, but is not searched for Mach-O images.
    package_spec, _ = read_json(args.package_spec.resolve(strict=True), "package relocation spec")
    if package_spec.get("schema") != 1 or package_spec.get("source") != {
        "git_revision": source["git_revision"],
        "git_tree": source["git_tree"],
    } or package_spec.get("target") != {"triple": args.target, "architecture": target_arch}:
        fail("package relocation spec does not bind this source and target")
    package_rows = package_spec.get("packages")
    if not isinstance(package_rows, list):
        fail("package relocation spec packages must be an array")
    expected_package_roles = {row.get("root_role") for row in package_rows if isinstance(row, dict)}
    expected_notice_roles = {
        notice.get("root_role")
        for row in package_rows if isinstance(row, dict)
        for notice in row.get("notices", []) if isinstance(notice, dict)
    }
    expected_archive_ids = {
        row.get("id") for row in package_rows if isinstance(row, dict)
        and isinstance(row.get("provenance"), dict)
        and row["provenance"].get("kind") == "verified-archive"
    }
    if set(package_roots) != expected_package_roles or set(notice_roots) != expected_notice_roles:
        fail("supplied package and license roots must exactly match the reviewed relocation spec")
    if set(package_roots) & set(notice_roots):
        fail("package roots and license roots must use distinct role names")
    if set(package_archives) != expected_archive_ids:
        fail("archive inputs must exactly match packages pinned to verified archives")
    all_roots_for_pins = dict(package_roots)
    all_roots_for_pins.update(notice_roots)
    packages, package_image_roots = package_records(package_spec, all_roots_for_pins, package_archives)
    input_roots = dict(origin_roots)
    input_roots.update(package_image_roots)
    spec = {"source": {"git_revision": source["git_revision"], "git_tree": source["git_tree"]}, "target": {"triple": args.target, "architecture": target_arch}}
    plan = collect_plan(spec, origins, input_roots, owner_paths, packages, target_arch)
    plan_bytes = (json.dumps(plan, indent=2, sort_keys=True) + "\n").encode("utf-8")
    plan_sha = hashlib.sha256(plan_bytes).hexdigest()
    output = args.output_dir.absolute()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.mkdir()
    write_new_bytes(output / "macho-relocation-plan.json", plan_bytes, label="relocation plan")
    for origin_id, receipt_path in receipts.items():
        wrapper = admitted_origin_wrapper(receipt_path.read_bytes(), origin_id, plan_sha)
        wrapper_path = output / f"{origin_id}-admitted-receipt.json"
        wrapper_bytes = (json.dumps(wrapper, indent=2, sort_keys=True) + "\n").encode("utf-8")
        write_new_bytes(wrapper_path, wrapper_bytes, label=f"{origin_id} admitted receipt")
    print(f"Created relocation plan SHA-256 {plan_sha}")
    print(f"Created {len(origins)} receipt-bound origin wrappers and {len(plan['images'])} Mach-O image records")
    print("No Mach-O files were copied, rewritten, signed, or launched by this collector.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (RelocationInputError, bundle.PackageError, OSError, ValueError, StopIteration) as error:
        print(f"relocation collection refused: {error}", file=sys.stderr)
        sys.exit(2)
