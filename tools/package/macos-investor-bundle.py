#!/usr/bin/env python3
"""Assemble an auditable macOS investor bundle from admitted build outputs.

This script does not build, sign, notarize, or run the application. It accepts
only outputs carrying build receipts for the pinned, clean application source
revision and refuses unresolved non-system Mach-O dependencies.
"""

from __future__ import annotations

import argparse
import datetime
import functools
import hashlib
import json
import os
import platform
import plistlib
import posixpath
import re
import selectors
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path
from typing import Any

from linux_release_package import admit_file_digest, parse_json_bytes, read_regular_bytes

from macho_relocation import (
    RelocationInputError,
    bundle_relative,
    file_tree_manifest,
    file_tree_sha256,
    relative_loader_name,
    unwrap_admitted_origin,
    validate_dotnet_runtime_receipt,
    validate_relocation_plan,
    write_new_bytes,
)


MACOS_TARGETS = {
    "aarch64-apple-darwin": "arm64",
    "x86_64-apple-darwin": "x86_64",
}
FONT_LICENSES = (
    "BricolageGrotesque-OFL.txt",
    "Geist-OFL.txt",
    "GeistMono-OFL.txt",
    "Newsreader-OFL.txt",
)
EXECUTABLES = ("backend-desktop", "backend-mcp", "backend-locald", "backend-cli")
HELPER_EXECUTABLES = {
    "go_oracle": "go/oracle",
    "pyrefly": "python/pyrefly",
    "node": "typescript/node/bin/node",
}
MACHO_MAGICS = {
    b"\xfe\xed\xfa\xce",
    b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"\xbe\xba\xfe\xca",
    b"\xca\xfe\xba\xbf",
    b"\xbf\xba\xfe\xca",
}


class PackageError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise PackageError(message)


def run(command: list[str], *, cwd: Path | None = None) -> str:
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError as error:
        fail(f"cannot run {command[0]}: {error}")
    if completed.returncode != 0:
        fail(f"command failed ({completed.returncode}): {command!r}\n{completed.stdout.strip()}")
    return completed.stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def file_inventory(root: Path, *, maximum_entries: int = 65536,
                   maximum_bytes: int = 8 * 1024**3) -> dict[str, dict[str, Any]]:
    inventory: dict[str, dict[str, Any]] = {}
    pending = [root]
    entries = total = 0
    while pending:
        with os.scandir(pending.pop()) as children:
            for child in children:
                entries += 1
                if entries > maximum_entries:
                    fail("bundle inventory exceeds its finite entry bound")
                path = Path(child.path)
                relative = path.relative_to(root).as_posix()
                mode = path.lstat().st_mode
                if stat.S_ISLNK(mode):
                    inventory[relative] = {"kind": "symlink", "target": os.readlink(path)}
                elif stat.S_ISDIR(mode):
                    pending.append(path)
                elif stat.S_ISREG(mode):
                    size, digest = admit_file_digest(path, min(512 * 1024**2, maximum_bytes - total))
                    if path.lstat().st_mode != mode:
                        fail("bundle file mode changed during inventory admission")
                    total += size
                    inventory[relative] = {"kind": "file", "sha256": digest,
                                           "size_bytes": size, "mode": stat.S_IMODE(mode)}
                else:
                    fail("bundle inventory contains a special file")
    return dict(sorted(inventory.items()))


def load_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {label} JSON at {path}: {error}")
    if not isinstance(value, dict):
        fail(f"{label} JSON must be an object")
    return value


def require_sha(value: Any, label: str) -> str:
    if not isinstance(value, str) or re.fullmatch(r"[0-9a-f]{64}", value) is None:
        fail(f"{label} must be a lowercase SHA-256 digest")
    return value


def origin_receipt(path: Path, origin_id: str) -> tuple[dict[str, Any], dict[str, Any] | None]:
    value = load_json(path, f"{origin_id} origin receipt")
    try:
        return unwrap_admitted_origin(value, origin_id)
    except RelocationInputError as error:
        fail(str(error))


def relocation_inputs(
    receipt_paths: dict[str, Path],
    plan_path: Path | None,
    source: dict[str, str],
    target: str,
    roots: dict[str, Path],
) -> tuple[dict[str, Any] | None, str | None]:
    admissions: dict[str, tuple[dict[str, Any], dict[str, Any] | None, str]] = {}
    for origin_id, path in receipt_paths.items():
        raw_receipt, attachment = origin_receipt(path, origin_id)
        outer = load_json(path, f"{origin_id} origin receipt")
        if attachment is None:
            receipt_sha = hashlib.sha256(path.read_bytes()).hexdigest()
        else:
            receipt_sha = require_sha(outer.get("source_receipt_sha256"), f"{origin_id} source receipt")
        admissions[origin_id] = (raw_receipt, attachment, receipt_sha)
    attached = {origin_id for origin_id, (_, attachment, _) in admissions.items() if attachment is not None}
    if not attached:
        if plan_path is not None:
            fail("a relocation plan was supplied but no origin receipt attaches to it")
        return None, None
    if attached != set(receipt_paths):
        fail("every selected origin receipt must attach to one shared relocation plan")
    if plan_path is None:
        fail("admitted origin receipts require --relocation-plan")
    try:
        plan_bytes = plan_path.read_bytes()
    except OSError as error:
        fail(f"cannot read the admitted Mach-O relocation plan: {error}")
    try:
        plan = json.loads(plan_bytes)
        plan = validate_relocation_plan(plan)
    except (json.JSONDecodeError, RelocationInputError) as error:
        fail(f"invalid Mach-O relocation plan: {error}")
    plan_digest = hashlib.sha256(plan_bytes).hexdigest()
    expected_origin_ids = set(receipt_paths)
    if set(plan["origins"]) != expected_origin_ids:
        fail("Mach-O relocation plan origins differ from the selected admitted inputs")
    if plan["source"] != {"git_revision": source["git_revision"], "git_tree": source["git_tree"]}:
        fail("Mach-O relocation plan does not bind the exact application source")
    if plan["target"]["triple"] != target or plan["target"]["architecture"] != MACOS_TARGETS[target]:
        fail("Mach-O relocation plan target differs from the requested bundle")
    expected_kinds = {
        "application": "application",
        "roslyn": "roslyn",
        "helpers": "helpers",
        "dotnet-runtime": "dotnet-runtime",
    }
    for origin_id, (receipt, attachment, receipt_sha) in admissions.items():
        if attachment is None or attachment["manifest_sha256"] != plan_digest:
            fail(f"origin receipt {origin_id} does not attach to the exact relocation plan bytes")
        origin = plan["origins"][origin_id]
        if origin["receipt_kind"] != expected_kinds[origin_id]:
            fail(f"relocation plan has the wrong origin receipt kind for {origin_id}")
        if origin["receipt_sha256"] != receipt_sha:
            fail(f"relocation plan does not bind the original {origin_id} receipt bytes")
        root = roots[origin_id]
        try:
            tree = file_tree_sha256(file_tree_manifest(root))
        except RelocationInputError as error:
            fail(str(error))
        if tree != origin["root_tree_sha256"]:
            fail(f"relocation plan input tree changed for origin {origin_id}")
    return plan, plan_digest


def parse_named_paths(values: list[str], label: str) -> dict[str, Path]:
    result: dict[str, Path] = {}
    for value in values:
        if "=" not in value:
            fail(f"{label} must use ID=PATH syntax")
        key, raw = value.split("=", 1)
        if not key or not raw or key in result:
            fail(f"{label} has an empty or duplicate ID")
        result[key] = Path(raw).resolve(strict=True)
    return result


def _stage_path(bundle: Path, relative: str) -> Path:
    safe = bundle_relative(relative, "Mach-O staged path")
    return bundle.joinpath(*safe.split("/"))


def apply_macho_relocation(
    staging: Path,
    plan: dict[str, Any],
    package_roots: dict[str, Path],
) -> dict[str, Any]:
    expected_packages = set(plan["packages"])
    if set(package_roots) != expected_packages:
        fail("relocation package roots must exactly match the pinned plan package set")
    for package_id, package in plan["packages"].items():
        root = package_roots[package_id]
        actual_tree = file_tree_sha256(file_tree_manifest(root))
        if actual_tree != package["root_tree_sha256"]:
            fail(f"relocation package root changed after collection: {package_id}")

    image_paths: dict[str, Path] = {}
    original_signatures: dict[str, str] = {}
    for image in plan["images"]:
        ref = image["ref"]
        if image["kind"] == "origin":
            path = _stage_path(staging, image["bundle_path"])
        else:
            package_id = image["package_id"]
            relative = image["package_relative_path"]
            root = package_roots[package_id].resolve(strict=True)
            source = (root / relative)
            try:
                resolved = source.resolve(strict=True)
            except OSError as error:
                fail(f"pinned Mach-O package input is missing: {package_id}/{relative}: {error}")
            if not path_within(resolved, root) or not resolved.is_file() or source.is_dir():
                fail(f"pinned Mach-O package input escapes its root or is not a file: {package_id}/{relative}")
            if sha256(resolved) != image["sha256"]:
                fail(f"pinned Mach-O package input hash changed: {package_id}/{relative}")
            path = _stage_path(staging, image["bundle_path"])
            path.parent.mkdir(parents=True, exist_ok=True)
            if path.exists() or path.is_symlink():
                fail(f"relocation destination already exists: {image['bundle_path']}")
            shutil.copy2(resolved, path)
        if not path.is_file() or path.is_symlink() or sha256(path) != image["sha256"]:
            fail(f"staged Mach-O image differs from the admitted input: {image['bundle_path']}")
        signature = inspect_signature(path)
        if signature != image["signature"]:
            fail(f"staged Mach-O signature observation differs from collection: {image['bundle_path']}")
        rpaths, dylib_id = inspect_load_metadata(path)
        if rpaths != image["rpaths"] or dylib_id != image["dylib_id"]:
            fail(f"staged Mach-O load commands differ from collection: {image['bundle_path']}")
        image_paths[ref] = path
        original_signatures[ref] = signature

    expected_loads: dict[str, list[dict[str, Any]]] = {ref: [] for ref in image_paths}
    for load in plan["loads"]:
        expected_loads[load["image_ref"]].append(load)
    for image in plan["images"]:
        ref = image["ref"]
        path = image_paths[ref]
        output = run(["otool", "-L", str(path)])
        actual_names = []
        for line in output.splitlines()[1:]:
            line = line.strip()
            if line:
                actual_names.append(line.split(" (compatibility version ", 1)[0])
        if image["dylib_id"] is not None:
            actual_names = [name for name in actual_names if name != image["dylib_id"]]
        admitted_names = [load["install_name"] for load in expected_loads[ref]]
        if sorted(actual_names) != sorted(admitted_names):
            fail(f"Mach-O load-command list differs from admitted relocation graph: {image['bundle_path']}")

    loads_by_image: dict[str, list[dict[str, Any]]] = {ref: [] for ref in image_paths}
    for load in plan["loads"]:
        loads_by_image[load["image_ref"]].append(load)
    relocation_records: list[dict[str, Any]] = []
    for image in plan["images"]:
        ref = image["ref"]
        path = image_paths[ref]
        bundle_path = image["bundle_path"]
        command = ["install_name_tool"]
        changes = 0
        for load in loads_by_image[ref]:
            if load["target_kind"] == "system":
                if load["install_name"] != load["system_path"]:
                    command.extend(["-change", load["install_name"], load["system_path"]])
                    changes += 1
                continue
            target_image = next(item for item in plan["images"] if item["ref"] == load["target_ref"])
            relocated_name = relative_loader_name(bundle_path, target_image["bundle_path"])
            command.extend(["-change", load["install_name"], relocated_name])
            changes += 1
        for raw_rpath in image["remove_rpaths"]:
            command.extend(["-delete_rpath", raw_rpath])
            changes += 1
        normalized_id = None
        if image["dylib_id"] is not None:
            relative_from_contents = bundle_path.removeprefix("Contents/")
            normalized_id = f"@rpath/{relative_from_contents}"
            command.extend(["-id", normalized_id])
            root_rpath_relative = posixpath.relpath("Contents", posixpath.dirname(bundle_path))
            root_rpath = "@loader_path" if root_rpath_relative == "." else f"@loader_path/{root_rpath_relative}"
            current_rpaths = [item for item in image["rpaths"] if item not in image["remove_rpaths"]]
            if root_rpath not in current_rpaths:
                command.extend(["-add_rpath", root_rpath])
            changes += 1
        if changes:
            command.append(str(path))
            run(command)
        rpaths_after, dylib_id_after = inspect_load_metadata(path)
        if any(item in rpaths_after for item in image["remove_rpaths"]):
            fail(f"external LC_RPATH remains after relocation: {bundle_path}")
        if normalized_id is not None and dylib_id_after != normalized_id:
            fail(f"Mach-O ID was not normalized after relocation: {bundle_path}")
        relocation_records.append(
            {
                "path": bundle_path,
                "input_sha256": image["sha256"],
                "staged_sha256": sha256(path),
                "input_signature": original_signatures[ref],
                "staged_signature": inspect_signature(path),
                "load_edges_rewritten": sum(
                    load["target_kind"] != "system" or load["install_name"] != load.get("system_path")
                    for load in loads_by_image[ref]
                ),
                "rpaths_removed": image["remove_rpaths"],
                "dylib_id_after": dylib_id_after,
            }
        )

    notice_records: list[dict[str, Any]] = []
    license_root = staging / "Contents/Resources/Licenses/Third Party"
    for package_id, package in sorted(plan["packages"].items()):
        for notice in package["notices"]:
            name = notice["name"]
            if not isinstance(name, str) or not name or Path(name).name != name or name in {".", ".."}:
                fail(f"unsafe package notice name for {package_id}")
            content = base64.b64decode(notice["content_base64"], validate=True)
            digest = hashlib.sha256(content).hexdigest()
            if digest != notice["sha256"]:
                fail(f"package notice bytes changed after collection: {package_id}/{name}")
            destination = license_root / package_id / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            if destination.exists() or destination.is_symlink():
                fail(f"package notice destination already exists: {package_id}/{name}")
            destination.write_bytes(content)
            notice_records.append(
                {
                    "package_id": package_id,
                    "name": name,
                    "sha256": digest,
                    "path": destination.relative_to(staging).as_posix(),
                }
            )
    return {
        "plan_sha256": None,
        "images": relocation_records,
        "notices": notice_records,
        "signature_policy": "input observations are retained; modified images require a later real sign-and-verify step",
    }


def validate_direct_cargo_provenance(
    provenance: dict[str, Any], source: dict[str, str], target: str, runner: dict[str, Any]
) -> dict[str, str]:
    expected_source = {
        "git_revision": source["git_revision"],
        "git_tree": source["git_tree"],
        "cargo_lock_sha256": source["cargo_lock_sha256"],
        "working_tree": "clean",
    }
    source_fingerprint = hashlib.sha256(
        json.dumps(expected_source, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    features = {
        "features": [],
        "all_features": False,
        "no_default_features": False,
        "targets": [target],
    }
    expected_command = [
        "$CARGO",
        "build",
        "--locked",
        "--manifest-path",
        "$SOURCE_ROOT/Cargo.toml",
        "--target-dir",
        "$SOURCE_ROOT/.local/target",
        "--target",
        target,
        "--profile",
        "release",
        "-p",
        "backend-desktop",
        "-p",
        "backend-mcp",
        "-p",
        "backend-locald",
        "-p",
        "backend-cli",
        "--message-format=json-render-diagnostics",
    ]
    if (
        provenance.get("schema") != 3
        or provenance.get("kind") != "direct-cargo"
        or provenance.get("git_head") != source["git_revision"]
        or provenance.get("git_tree") != source["git_tree"]
        or provenance.get("git_head_after") != source["git_revision"]
        or provenance.get("git_tree_after") != source["git_tree"]
        or provenance.get("source_fingerprint_sha256") != source_fingerprint
        or provenance.get("source_fingerprint_sha256_after") != source_fingerprint
        or provenance.get("source_unchanged") is not True
        or provenance.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
        or provenance.get("cargo_exit_status") != 0
        or provenance.get("features") != features
        or provenance.get("target") != target
        or provenance.get("profile") != "release"
        or provenance.get("locked") is not True
        or provenance.get("runner_sha256") != runner.get("sha256")
        or provenance.get("runner_sha256_after") != runner.get("sha256")
        or provenance.get("runner_asset_sha256") != runner.get("referenced_asset_sha256")
        or provenance.get("runner_asset_sha256_after") != runner.get("referenced_asset_sha256")
        or provenance.get("environment_snapshot_sha256") != runner.get("environment_sha256")
        or provenance.get("environment_variables") != runner.get("environment_variables")
        or provenance.get("effective_environment_sha256") != runner.get("effective_environment_sha256")
        or provenance.get("effective_environment_variables") != runner.get("effective_environment_variables")
        or provenance.get("command") != expected_command
    ):
        fail("schema-3 direct Cargo provenance does not bind the exact source, runner, environment, and command")
    run_id = provenance.get("run_id")
    if not isinstance(run_id, str) or re.fullmatch(r"[0-9a-f]{32}", run_id) is None:
        fail("schema-3 direct Cargo provenance is missing its invocation identity")
    child_pid = provenance.get("cargo_child_pid")
    if not isinstance(child_pid, int) or isinstance(child_pid, bool) or child_pid <= 0:
        fail("schema-3 direct Cargo provenance has no valid Cargo child PID")
    output_log = provenance.get("cargo_output_log")
    if (
        not isinstance(output_log, dict)
        or output_log.get("path") != f"direct-{run_id}.cargo.log"
        or not isinstance(output_log.get("size_bytes"), int)
        or isinstance(output_log.get("size_bytes"), bool)
        or output_log["size_bytes"] <= 0
    ):
        fail("schema-3 direct Cargo provenance has an invalid raw output log identity")
    require_sha(output_log.get("sha256"), "direct Cargo raw output log")
    require_sha(provenance.get("command_sha256"), "direct Cargo argv digest")
    started = provenance.get("started_at_utc")
    finished = provenance.get("finished_at_utc")
    try:
        started_at = datetime.datetime.fromisoformat(started)
        finished_at = datetime.datetime.fromisoformat(finished)
    except (TypeError, ValueError):
        fail("schema-3 direct Cargo provenance has invalid start/end timestamps")
    if (
        started_at.tzinfo is None
        or finished_at.tzinfo is None
        or finished_at < started_at
        or not isinstance(provenance.get("elapsed_ns"), int)
        or isinstance(provenance.get("elapsed_ns"), bool)
        or provenance["elapsed_ns"] <= 0
    ):
        fail("schema-3 direct Cargo provenance has invalid execution timing")
    toolchain = provenance.get("toolchain")
    if not isinstance(toolchain, dict) or set(toolchain) not in (
        {"cargo", "rustc", "rustdoc"},
        {"cargo", "rustc", "rustdoc", "rustc_wrapper"},
    ):
        fail("schema-3 provenance must identify direct Cargo, rustc, rustdoc, and selected wrapper tools")
    if provenance.get("toolchain_unchanged") is not True or provenance.get("toolchain_after") != toolchain:
        fail("schema-3 Cargo/Rust tool versions and executable hashes changed during the build")
    assets = runner.get("referenced_asset_sha256")
    if not isinstance(assets, dict):
        fail("direct Cargo runner is missing its pinned tool assets")
    if assets.get("runner.tool.CARGO") != assets.get("runner.exec"):
        fail("direct Cargo runner executable differs from its explicit CARGO tool pin")
    tool_assets = {
        "cargo": "runner.exec",
        "rustc": "runner.tool.RUSTC",
        "rustdoc": "runner.tool.RUSTDOC",
        "rustc_wrapper": "runner.tool.RUSTC_WRAPPER",
    }
    for name, tool in toolchain.items():
        if not isinstance(tool, dict) or not isinstance(tool.get("version"), str) or not tool["version"].strip():
            fail(f"schema-3 direct Cargo provenance is missing the {name} version")
        digest = require_sha(tool.get("sha256"), f"direct Cargo tool {name}")
        if assets.get(tool_assets[name]) != digest:
            fail(f"direct Cargo {name} digest differs from the pinned runner asset")
    wrapper_digest = provenance.get("rustc_wrapper_sha256")
    if "rustc_wrapper" in toolchain:
        require_sha(wrapper_digest, "selected rustc wrapper")
        if wrapper_digest != toolchain["rustc_wrapper"]["sha256"]:
            fail("direct Cargo selected wrapper digest differs from its toolchain snapshot")
    elif wrapper_digest is not None or "runner.tool.RUSTC_WRAPPER" in assets:
        fail("direct Cargo receipt has inconsistent selected-wrapper evidence")
    outputs = provenance.get("outputs")
    if not isinstance(outputs, list) or any(
        not isinstance(item, dict)
        or not isinstance(item.get("path"), str)
        or not isinstance(item.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
        or not isinstance(item.get("size_bytes"), int)
        or isinstance(item.get("size_bytes"), bool)
        or item["size_bytes"] <= 0
        for item in outputs
    ):
        fail("schema-3 direct Cargo output manifest is malformed")
    expected_paths = {f"{target}/release/{name}" for name in EXECUTABLES}
    if len(outputs) != len(expected_paths) or {item["path"] for item in outputs} != expected_paths:
        fail("direct Cargo provenance must list exactly the three admitted executable outputs")
    return {item["path"]: item["sha256"] for item in outputs}


def validate_source(source: Path, expected_revision: str, expected_tree: str) -> dict[str, str]:
    head = run(["git", "-C", str(source), "rev-parse", "HEAD"])
    tree = run(["git", "-C", str(source), "rev-parse", "HEAD^{tree}"])
    if head != expected_revision or tree != expected_tree:
        fail(
            "application source differs from the operator-pinned release manifest: expected "
            f"{expected_revision} ({expected_tree}), got {head} ({tree})"
        )
    status = run(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"])
    if status:
        fail(f"application source checkout is not clean:\n{status}")
    lock = source / "Cargo.lock"
    if not lock.is_file():
        fail("the pinned application source has no Cargo.lock")
    return {
        "git_revision": head,
        "git_tree": tree,
        "cargo_lock_sha256": sha256(lock),
        "working_tree": "clean",
    }


def validate_app_build(
    receipt_path: Path,
    artifact_dir: Path,
    source: dict[str, str],
    target: str,
    expected_runner_sha256: str,
) -> tuple[dict[str, Any], dict[str, Path]]:
    receipt, _ = origin_receipt(receipt_path, "application")
    expected_source = {
        "git_revision": source["git_revision"],
        "git_tree": source["git_tree"],
        "cargo_lock_sha256": source["cargo_lock_sha256"],
        "working_tree": "clean",
    }
    if receipt.get("schema") != 1 or receipt.get("source") != expected_source:
        fail("application build receipt does not bind the exact clean source and Cargo.lock")
    if receipt.get("target") != target or receipt.get("profile") != "release":
        fail("application build receipt must name the requested target and release profile")
    if receipt.get("locked_build") is not True:
        fail("application build must use --locked")
    runner_before = receipt.get("cargo_runner_before")
    runner_after = receipt.get("cargo_runner_after")
    if not isinstance(runner_before, dict) or not isinstance(runner_after, dict):
        fail("application receipt is missing the operator-pinned Cargo runner identity")
    if receipt.get("cargo_runner_unchanged") is not True or runner_before != runner_after:
        fail("application receipt must prove the pinned runner and its referenced files were unchanged")
    if require_sha(runner_before.get("sha256"), "Cargo runner") != require_sha(
        runner_before.get("expected_sha256"), "expected Cargo runner"
    ):
        fail("application receipt Cargo runner differs from its content pin")
    if runner_before["sha256"] != expected_runner_sha256:
        fail("application receipt Cargo runner differs from the package operator's explicit pin")
    execution_kind = runner_before.get("execution_kind", "wrapper")
    if execution_kind not in {"wrapper", "direct-cargo"}:
        fail("application receipt has an unsupported Cargo runner execution kind")
    runner_assets = runner_before.get("referenced_asset_sha256")
    required_assets = {
        "runner",
        "runner.interpreter",
        "runner.environment.0",
        "runner.tool.RUSTC",
        "runner.exec",
    }
    if execution_kind == "wrapper":
        required_assets.add("runner.tool.RUSTC_WRAPPER")
    else:
        required_assets.update({"runner.tool.CARGO", "runner.tool.RUSTDOC"})
    if not isinstance(runner_assets, dict) or not required_assets.issubset(runner_assets):
        fail("application receipt is missing hashes for pinned runner and toolchain references")
    for role, digest in runner_assets.items():
        if not isinstance(role, str) or not role or role in {"path", "paths"}:
            fail("Cargo runner asset roles must not contain local paths")
        require_sha(digest, f"Cargo runner asset {role}")
    if runner_assets.get("runner") != runner_before["sha256"]:
        fail("Cargo runner asset manifest does not match its top-level content pin")
    if execution_kind == "direct-cargo":
        snapshot_names = runner_before.get("environment_variables")
        effective_names = runner_before.get("effective_environment_variables")
        if (
            not isinstance(runner_before.get("environment_sha256"), str)
            or re.fullmatch(r"[0-9a-f]{64}", runner_before["environment_sha256"]) is None
            or runner_assets.get("runner.environment.snapshot") != runner_before["environment_sha256"]
            or not isinstance(snapshot_names, list)
            or not snapshot_names
            or any(not isinstance(name, str) for name in snapshot_names)
            or snapshot_names != sorted(set(snapshot_names))
            or not isinstance(runner_before.get("effective_environment_sha256"), str)
            or re.fullmatch(r"[0-9a-f]{64}", runner_before["effective_environment_sha256"]) is None
            or not isinstance(effective_names, list)
            or any(not isinstance(name, str) for name in effective_names)
            or effective_names != sorted(set(effective_names))
            or not {"CARGO_BUILD_JOBS", "CARGO_HOME", "CARGO_TARGET_DIR", "HOME", "PATH", "PWD", "RUSTC", "RUSTDOC", "TMPDIR"}.issubset(
                effective_names
            )
            or any(
                name in {"CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC_WORKSPACE_WRAPPER"}
                or name.startswith(("CARGO_PROFILE_", "CARGO_NET_", "CARGO_REGISTRY_", "CARGO_REGISTRIES_"))
                or re.search(r"(?:TOKEN|SECRET|PASSWORD|CREDENTIAL|PRIVATE_KEY|AUTH)", name, re.I)
                for name in effective_names
            )
        ):
            fail("direct Cargo receipt is missing its literal environment snapshot identity")
    invocation_prefix = runner_before.get("invocation_prefix")
    if (
        not isinstance(invocation_prefix, list)
        or len(invocation_prefix) < 2
        or invocation_prefix[0] != "$CARGO_RUNNER_INTERPRETER"
        or invocation_prefix[-1] != "$CARGO_RUNNER"
        or any(not isinstance(item, str) for item in invocation_prefix)
        or any(Path(item).is_absolute() for item in invocation_prefix[1:-1])
    ):
        fail("application receipt must record the pinned runner's interpreter/script invocation prefix")
    if (
        receipt.get("source_unchanged") is not True
        or receipt.get("source_before") != expected_source
        or receipt.get("source_after") != expected_source
    ):
        fail("application build receipt must prove the exact clean source was unchanged across the build")
    command = receipt.get("command")
    expected_command = [
        *invocation_prefix,
        "build",
        "--locked",
        "--manifest-path",
        "$SOURCE_ROOT/Cargo.toml",
        "--target-dir",
        "$SOURCE_ROOT/.local/target",
        "--target",
        target,
        "--profile",
        "release",
        "-p",
        "backend-desktop",
        "-p",
        "backend-mcp",
        "-p",
        "backend-locald",
        "-p",
        "backend-cli",
    ]
    if execution_kind == "direct-cargo":
        expected_command.append("--message-format=json-render-diagnostics")
    if command != expected_command:
        fail("application receipt must record the exact locked release build through the pinned runner")
    provenance = receipt.get("cargo_provenance")
    if not isinstance(provenance, dict):
        fail("application receipt is missing its Cargo invocation provenance")
    require_sha(provenance.get("record_sha256"), "Cargo provenance record")
    if execution_kind == "wrapper":
        expected_features = {
            "features": [],
            "all_features": False,
            "no_default_features": False,
            "targets": [target],
        }
        if (
            provenance.get("schema") != 2
            or provenance.get("git_head") != source["git_revision"]
            or not isinstance(provenance.get("source_dirty_sha256"), str)
            or re.fullmatch(r"[0-9a-f]{64}", provenance["source_dirty_sha256"]) is None
            or provenance.get("source_unchanged") is not True
            or provenance.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
            or provenance.get("cargo_exit_status") != 0
            or provenance.get("features") != expected_features
            or provenance.get("toolchain_capture_complete") is not True
            or provenance.get("toolchain_unchanged") is not True
        ):
            fail("schema-2 wrapper provenance does not bind a successful unchanged build to this source/target")
    else:
        validate_direct_cargo_provenance(provenance, source, target, runner_before)
    versions = provenance.get("toolchain")
    if execution_kind == "wrapper":
        if not isinstance(versions, dict) or any(
            not isinstance(versions.get(name), str) or not versions[name].strip()
            for name in ("cargo", "rustc", "rustdoc")
        ):
            fail("schema-2 Cargo provenance must identify Cargo, rustc, and rustdoc versions")
        wrapper_hashes = provenance.get("wrapper_sha256")
        if not isinstance(wrapper_hashes, dict) or set(wrapper_hashes) != {"runtime", "source", "rustc"}:
            fail("schema-2 Cargo provenance must identify the pinned Cargo wrapper files")
        for name, digest in wrapper_hashes.items():
            require_sha(digest, f"Cargo wrapper {name}")
    outputs = provenance.get("outputs")
    if not isinstance(outputs, list) or any(
        not isinstance(item, dict)
        or not isinstance(item.get("path"), str)
        or not isinstance(item.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
        for item in outputs
    ):
        fail("Cargo provenance output manifest is malformed")
    if len({item["path"] for item in outputs}) != len(outputs):
        fail("Cargo provenance output manifest contains duplicate paths")
    provenance_outputs = {item["path"]: item["sha256"] for item in outputs}
    expected_output_names = {f"{target}/release/{name}" for name in EXECUTABLES}
    if not expected_output_names.issubset(provenance_outputs):
        fail("Cargo provenance is missing one or more exact application executable outputs")
    if execution_kind == "direct-cargo" and set(provenance_outputs) != expected_output_names:
        fail("direct Cargo provenance must contain exactly the three admitted executable outputs")
    listed = receipt.get("executables")
    if not isinstance(listed, dict) or set(listed) != set(EXECUTABLES):
        fail(f"application build receipt must hash exactly {', '.join(EXECUTABLES)}")
    entries = list(artifact_dir.iterdir())
    if any(path.is_symlink() or not path.is_file() for path in entries) or {
        path.name for path in entries
    } != set(EXECUTABLES):
        fail("application artifact directory must contain exactly the three receipt-bound regular files")
    paths: dict[str, Path] = {}
    for name in EXECUTABLES:
        path = artifact_dir / name
        if not path.is_file() or not os.access(path, os.X_OK):
            fail(f"missing executable build output: {path}")
        if sha256(path) != require_sha(listed[name], f"application receipt {name}"):
            fail(f"application build receipt hash mismatch for {name}")
        target_output = f"{target}/release/{name}"
        if sha256(path) != provenance_outputs[target_output]:
            fail(f"Cargo provenance output hash mismatch for {name}")
        paths[name] = path
    return receipt, paths


def validate_roslyn_build(
    receipt_path: Path,
    output_dir: Path,
    source_root: Path,
    source: dict[str, str],
    target: str,
) -> tuple[dict[str, Any], dict[str, str]]:
    receipt, _ = origin_receipt(receipt_path, "roslyn")
    project = source_root / "frontends/csharp/src/legacy/helper/oracle.csproj"
    lock = source_root / "frontends/csharp/src/legacy/helper/packages.lock.json"
    expected_source = {
        "git_revision": source["git_revision"],
        "git_tree": source["git_tree"],
        "project_sha256": sha256(project),
        "packages_lock_sha256": sha256(lock),
    }
    expected_runtime = "osx-arm64" if target.startswith("aarch64-") else "osx-x64"
    if receipt.get("schema") != 1 or receipt.get("source") != expected_source:
        fail("Roslyn receipt does not bind the helper project and locked packages from pinned source")
    if receipt.get("runtime_identifier") != expected_runtime:
        fail(f"Roslyn helper must be published for {expected_runtime}")
    if receipt.get("framework_dependent") is not True or receipt.get("locked_restore") is not True:
        fail("Roslyn helper must be the real locked, framework-dependent oracle publish")
    if receipt.get("fresh_output_dir") is not True:
        fail("Roslyn helper publish must use a fresh output directory")
    commands = receipt.get("commands")
    if (
        not isinstance(commands, list)
        or not commands
        or not any(isinstance(command, list) and "--locked-mode" in command for command in commands)
        or not any(isinstance(command, list) and "publish" in command for command in commands)
    ):
        fail("Roslyn receipt must record a locked restore/publish command")
    expected_files = receipt.get("files")
    if not isinstance(expected_files, dict) or not expected_files:
        fail("Roslyn receipt must enumerate every published helper file")
    observed: dict[str, str] = {}
    for path in sorted(output_dir.rglob("*")):
        if path.is_symlink():
            fail(f"Roslyn output contains a symlink; publish must be self-contained: {path}")
        if path.is_file():
            relative = path.relative_to(output_dir).as_posix()
            observed[relative] = sha256(path)
    if observed != expected_files:
        fail("Roslyn publish output differs from its complete file/hash receipt")
    notices = receipt.get("notices")
    if not isinstance(notices, dict) or not notices:
        fail("Roslyn receipt must enumerate license and third-party notice files")
    for name, relative in notices.items():
        if not isinstance(relative, str) or relative not in observed:
            fail(f"Roslyn publish is missing its {name} notice")
    for required in ("oracle.dll", "oracle.deps.json", "oracle.runtimeconfig.json"):
        if required not in observed:
            fail(f"Roslyn helper publish is missing {required}")
    return receipt, observed


def sdk_file_modes(records: dict[str, str]) -> dict[str, int]:
    return {relative: 0o755 if relative == "typescript/node/bin/node" else 0o644
            for relative in records}


def sdk_file_digests(helper_dir: Path, records: dict[str, str]) -> dict[str, str]:
    """Refresh selected hashes only within remaining aggregate/package budgets."""
    if not isinstance(records, dict) or not records or len(records) > 512:
        fail("SDK helper receipt must contain a bounded complete file inventory")
    total = package_bytes = 0
    observed = {}
    for relative in records:
        if not isinstance(relative, str):
            fail("SDK helper receipt has an unsafe file path")
        path = Path(relative)
        if ("\\" in relative or path.is_absolute()
                or path.as_posix() != relative or any(part in {".", ".."} for part in path.parts)
                or (relative not in {"typescript/node/bin/node", "typescript/node/LICENSE"}
                    and not relative.startswith("typescript/node_modules/typescript/"))):
            fail("SDK helper receipt has an unsafe file path")
        selected = helper_dir / relative
        expected_mode = sdk_file_modes({relative: ""})[relative]
        if stat.S_IMODE(selected.lstat().st_mode) != expected_mode:
            fail("SDK helper file differs from its exact 0755/0644 mode policy")
        maximum = 512 * 1024**2 - total
        package = relative.startswith("typescript/node_modules/typescript/")
        if package:
            maximum = min(maximum, 96 * 1024**2 - package_bytes)
        if relative == "typescript/node/LICENSE":
            maximum = min(maximum, 1024**2)
        size, digest = admit_file_digest(selected, maximum)
        if stat.S_IMODE(selected.lstat().st_mode) != expected_mode:
            fail("SDK helper mode changed during bounded admission")
        total += size
        package_bytes += size if package else 0
        observed[relative] = digest
    return observed


def validate_sdk_helper_payload(
    receipt: dict[str, Any], helper_dir: Path, source: dict[str, str], target: str,
) -> tuple[dict[str, Any], dict[str, str]]:
    """Admit the finite Node/Compiler API payload without inventing other helpers."""
    if receipt.get("schema") != 1 or receipt.get("source") != source or receipt.get("target") != target:
        fail("SDK helper receipt must bind the complete clean application source and target")
    records = receipt.get("files")
    if not isinstance(records, dict) or not records or len(records) > 512:
        fail("SDK helper receipt must contain a bounded complete file inventory")
    if receipt.get("file_modes") != sdk_file_modes(records):
        fail("SDK helper receipt must bind exact 0755 Node and 0644 package/notice modes")
    directories: set[str] = set()
    for relative, digest in records.items():
        if not isinstance(relative, str) or "\\" in relative:
            fail("SDK helper receipt has an unsafe file path")
        path = Path(relative)
        if path.is_absolute() or path.as_posix() != relative or any(part in {".", ".."} for part in path.parts):
            fail("SDK helper receipt has an unsafe file path")
        if relative not in {"typescript/node/bin/node", "typescript/node/LICENSE"} and not relative.startswith("typescript/node_modules/typescript/"):
            fail("SDK-only payload must contain only the selected Node and TypeScript package")
        require_sha(digest, "SDK helper file")
        directories.update(parent.as_posix() for parent in path.parents if parent.parts)
    observed: dict[str, str] = {}
    total = package_bytes = entries = 0
    pending = [helper_dir]
    while pending:
        for path in pending.pop().iterdir():
            entries += 1
            if entries > 2048 or path.is_symlink() or not (path.is_dir() or path.is_file()):
                fail("SDK helper payload exceeds its entry bound or contains a link/special file")
            relative = path.relative_to(helper_dir).as_posix()
            if path.is_dir():
                if relative not in directories:
                    fail("SDK helper payload contains an unreceipted directory")
                pending.append(path)
                continue
            if relative not in records:
                fail("SDK helper payload contains an unreceipted file")
            expected_mode = receipt["file_modes"][relative]
            if stat.S_IMODE(path.lstat().st_mode) != expected_mode:
                fail("SDK helper file differs from its exact 0755/0644 mode policy")
            maximum = 512 * 1024**2 - total
            is_package = relative.startswith("typescript/node_modules/typescript/")
            if is_package:
                maximum = min(maximum, 96 * 1024**2 - package_bytes)
            if relative == "typescript/node/LICENSE":
                maximum = min(maximum, 1024**2)
            try:
                size, digest = admit_file_digest(path, maximum)
            except (OSError, ValueError) as error:
                fail(f"SDK helper file failed bounded identity admission: {error}")
            total += size
            if stat.S_IMODE(path.lstat().st_mode) != expected_mode:
                fail("SDK helper mode changed during bounded admission")
            if is_package:
                package_bytes += size
            observed[relative] = digest
    if observed != records:
        fail("SDK helper payload differs from its complete file/hash receipt")
    required = {"typescript/node/bin/node", "typescript/node/LICENSE",
                "typescript/node_modules/typescript/package.json", "typescript/node_modules/typescript/bin/tsc",
                "typescript/node_modules/typescript/lib/typescript.js", "typescript/node_modules/typescript/LICENSE.txt",
                "typescript/node_modules/typescript/ThirdPartyNoticeText.txt"}
    if not required.issubset(observed):
        fail("SDK helper payload omits its executable, Compiler API, or genuine notices")
    tools = receipt.get("tools")
    if not isinstance(tools, dict) or set(tools) != {"node", "typescript"} or any(not isinstance(v, dict) for v in tools.values()):
        fail("SDK-only receipt must identify exactly Node and TypeScript")
    for key in tools:
        if not isinstance(tools[key].get("version"), str) or not tools[key]["version"].strip():
            fail(f"SDK helper receipt has no {key} version")
    node = helper_dir / "typescript/node/bin/node"
    if tools["node"].get("sha256") != observed["typescript/node/bin/node"] or not os.access(node, os.X_OK):
        fail("SDK Node differs from its executable tool identity")
    try:
        package = parse_json_bytes(read_regular_bytes(helper_dir / "typescript/node_modules/typescript/package.json",
                                                     1024**2, "TypeScript package metadata"), "TypeScript package metadata")
    except (OSError, ValueError) as error:
        fail(f"SDK TypeScript metadata failed bounded identity admission: {error}")
    if package.get("name") != "typescript" or package.get("version") != tools["typescript"]["version"]:
        fail("SDK Compiler API package differs from its selected identity")
    notices = {"node": "typescript/node/LICENSE", "typescript": "typescript/node_modules/typescript/LICENSE.txt",
               "typescript_third_party": "typescript/node_modules/typescript/ThirdPartyNoticeText.txt"}
    if receipt.get("notices") != notices:
        fail("SDK helper receipt must identify the genuine Node and TypeScript notices")
    if (helper_dir / notices["node"]).stat().st_size > 1024**2:
        fail("SDK Node license exceeds its notice bound")
    return receipt, observed


def write_sdk_bundle_receipt(
    helper_resources: Path, provenance: Path, source: dict[str, str], target: str,
    original: dict[str, Any], original_receipt: Path,
) -> tuple[dict[str, Any], Path]:
    """Bind relocated bytes separately; retain the admitted original receipt unchanged."""
    receipt = dict(original)
    receipt["source"] = dict(source)
    receipt["files"] = sdk_file_digests(helper_resources, original["files"])
    receipt["file_modes"] = sdk_file_modes(receipt["files"])
    receipt["tools"] = {name: dict(record) for name, record in original["tools"].items()}
    receipt["tools"]["node"]["sha256"] = receipt["files"]["typescript/node/bin/node"]
    receipt["source_receipt_sha256"] = sha256(original_receipt)
    validate_sdk_helper_payload(receipt, helper_resources, source, target)
    path = provenance / "compiler-helpers-receipt.json"
    write_new_bytes(path, (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode(), label="relocated SDK helper receipt")
    return receipt, path


def bounded_sdk_probe(command: list[str], cwd: str, environment: dict[str, str], *, timeout: float = 30, maximum: int = 4096) -> subprocess.CompletedProcess:
    """Bound both pipe collection and the lifetime of this owned version probe."""
    child = subprocess.Popen(command, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    deadline = time.monotonic() + timeout
    collected = {"stdout": bytearray(), "stderr": bytearray()}
    with selectors.DefaultSelector() as selector:
        selector.register(child.stdout, selectors.EVENT_READ, "stdout")
        selector.register(child.stderr, selectors.EVENT_READ, "stderr")
        try:
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    fail("SDK version probe exceeded its deadline")
                for key, _ in selector.select(remaining):
                    buffer = collected[key.data]
                    block = os.read(key.fd, maximum + 1 - len(buffer))
                    buffer.extend(block)
                    if len(buffer) > maximum:
                        fail("SDK version probe exceeded its output bound")
                    if not block:
                        selector.unregister(key.fileobj)
            try:
                result = child.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                fail("SDK version probe exceeded its deadline")
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
            child.stdout.close()
            child.stderr.close()
    return subprocess.CompletedProcess(command, result, bytes(collected["stdout"]), bytes(collected["stderr"]))


def verify_sdk_runtime(helper_resources: Path, receipt: dict[str, Any]) -> dict[str, Any]:
    """Execute only the relocated Node and genuine Compiler API, with a private HOME."""
    node = helper_resources / "typescript/node/bin/node"
    package = helper_resources / "typescript/node_modules/typescript"
    results: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(prefix="nudox-sdk-probe-") as temporary:
        environment = {"PATH": "/usr/bin:/bin", "HOME": temporary, "TMPDIR": temporary}
        for name, arguments, expected in [
            ("node", ["--version"], receipt["tools"]["node"]["version"]),
            ("typescript", ["-e", "process.stdout.write(require(process.argv[1]).version)", str(package)], receipt["tools"]["typescript"]["version"]),
        ]:
            completed = bounded_sdk_probe([str(node), *arguments], temporary, environment)
            if completed.returncode or len(completed.stdout) > 4096 or len(completed.stderr) > 4096 or completed.stdout.decode().strip() != expected:
                fail(f"relocated {name} version probe did not match the admitted SDK identity")
            results[name] = {"exit": completed.returncode, "version": completed.stdout.decode().strip(),
                             "stdout_sha256": hashlib.sha256(completed.stdout).hexdigest(),
                             "stderr_sha256": hashlib.sha256(completed.stderr).hexdigest()}
    validate_sdk_helper_payload(receipt, helper_resources, receipt["source"], receipt["target"])
    return results


def validate_helper_payload(
    receipt_path: Path,
    helper_dir: Path,
    source_root: Path,
    source: dict[str, str],
    target: str,
    *, sdk_only: bool = False,
) -> tuple[dict[str, Any], dict[str, str]]:
    receipt, _ = origin_receipt(receipt_path, "helpers")
    if sdk_only:
        return validate_sdk_helper_payload(receipt, helper_dir, source, target)
    if receipt.get("schema") != 1 or receipt.get("source") != {
        "git_revision": source["git_revision"],
        "git_tree": source["git_tree"],
    }:
        fail("compiler-helper receipt does not bind the exact pinned application source")
    if receipt.get("target") != target:
        fail("compiler-helper receipt target differs from the application target")
    expected_files = receipt.get("files")
    if not isinstance(expected_files, dict) or not expected_files:
        fail("compiler-helper receipt must hash every file in its payload")
    observed = {
        path.relative_to(helper_dir).as_posix(): sha256(path)
        for path in sorted(helper_dir.rglob("*"))
        if path.is_file()
    }
    if any(path.is_symlink() for path in helper_dir.rglob("*")):
        fail("compiler-helper payload must not contain symlinks to machine-local files")
    if observed != expected_files:
        fail("compiler-helper payload differs from its complete file/hash receipt")

    tools = receipt.get("tools")
    if not isinstance(tools, dict):
        fail("compiler-helper receipt must identify the real Go, Python, Node, and TypeScript helpers")
    for key, relative in HELPER_EXECUTABLES.items():
        path = helper_dir / relative
        record = tools.get(key)
        if not path.is_file() or not os.access(path, os.X_OK) or not isinstance(record, dict):
            fail(f"missing executable compiler helper {key}: {path}")
        if require_sha(record.get("sha256"), f"helper {key}") != sha256(path):
            fail(f"compiler-helper receipt hash mismatch for {key}")
        if not isinstance(record.get("version"), str) or not record["version"].strip():
            fail(f"compiler-helper receipt must record a version for {key}")

    typescript = tools.get("typescript")
    if not isinstance(typescript, dict):
        fail("compiler-helper receipt is missing the TypeScript npm package identity")
    package_root = helper_dir / "typescript/node_modules/typescript"
    package_json = package_root / "package.json"
    if not package_json.is_file() or not (package_root / "bin/tsc").is_file():
        fail("TypeScript helper must include the real npm package and its tsc program")
    package = load_json(package_json, "TypeScript package")
    version = package.get("version")
    if not isinstance(version, str) or version != typescript.get("version"):
        fail("TypeScript package version differs from its helper receipt")
    if not (package_root / "LICENSE.txt").is_file():
        fail("TypeScript npm package is missing its LICENSE.txt notice")
    driver = source_root / "frontends/typescript/src/legacy/checker/main.cjs"
    if not driver.is_file():
        fail("the pinned source is missing its TypeScript checker driver")
    if receipt.get("typescript_driver_sha256") != sha256(driver):
        fail("compiler-helper receipt does not bind the checked-in TypeScript driver")

    notices = receipt.get("notices")
    notice_names = {"go_oracle", "pyrefly", "node", "typescript"}
    if not isinstance(notices, dict) or set(notices) != notice_names:
        fail("compiler-helper receipt must enumerate Go, Pyrefly, Node, and TypeScript notices")
    for key, relative in notices.items():
        if not isinstance(relative, str) or relative not in observed or not (helper_dir / relative).is_file():
            fail(f"compiler-helper notice is missing for {key}")

    go_sources = source_root / "frontends/go/src/legacy/oracle"
    source_files = {
        (Path("frontends/go/src/legacy/oracle") / path.relative_to(go_sources)).as_posix(): sha256(path)
        for path in sorted(go_sources.rglob("*"))
        if path.is_file()
    }
    if receipt.get("go_source_files") != source_files:
        fail("Go oracle receipt does not bind every checked-in producer source file")
    return receipt, observed


def copy_runtime(
    dotnet_root: Path, destination: Path, receipt: dict[str, Any]
) -> dict[str, str]:
    dotnet = dotnet_root / "dotnet"
    host = dotnet_root / "host"
    shared = dotnet_root / "shared"
    if not dotnet.is_file() or not (host / "fxr").is_dir():
        fail("dotnet root must contain the real dotnet host and host/fxr runtime")
    runtimes = [
        path
        for path in (shared / "Microsoft.NETCore.App").glob("*")
        if path.is_dir() and re.fullmatch(r"\d+\.\d+\.\d+", path.name)
    ]
    if not runtimes:
        fail("dotnet root must contain a shared Microsoft.NETCore.App runtime")
    notice_names = {
        "license": "LICENSE.txt",
        "third_party": "ThirdPartyNotices.txt",
    }
    destination.mkdir(parents=True)
    shutil.copy2(dotnet, destination / "dotnet")
    shutil.copytree(host, destination / "host", symlinks=False)
    shutil.copytree(shared, destination / "shared", symlinks=False)
    copied: dict[str, str] = {}
    for key, name in notice_names.items():
        relative = receipt["notices"][key]["path"]
        shutil.copy2(dotnet_root / relative, destination / name)
        copied[name] = sha256(destination / name)
    return copied


def macos_tools() -> None:
    if platform.system() != "Darwin":
        fail("bundle assembly and Mach-O closure verification require macOS")
    required = (
        "codesign",
        "ditto",
        "dyld_info",
        "file",
        "install_name_tool",
        "lipo",
        "otool",
        "plutil",
        "vtool",
    )
    missing = [name for name in required if shutil.which(name) is None]
    if missing:
        fail(f"missing required macOS verification tools: {', '.join(missing)}")


def is_system_path(value: str) -> bool:
    return value.startswith(("/System/Library/", "/usr/lib/", "/System/Volumes/Preboot/Cryptexes/OS/System/Library/"))


def path_within(path: Path, root: Path) -> bool:
    try:
        path.resolve(strict=False).relative_to(root.resolve())
    except ValueError:
        return False
    return True


def expand_image_path(value: str, image: Path, owner: Path) -> Path | None:
    image_dir = image.parent
    executable_dir = owner.parent
    for prefix, base in (("@loader_path", image_dir), ("@executable_path", executable_dir)):
        if value == prefix:
            return base
        if value.startswith(prefix + "/"):
            return base / value[len(prefix) + 1 :]
    if value.startswith("/"):
        return Path(value)
    return None


def classify_resolved_path(
    candidate: Path, bundle: Path, expected_arch: str
) -> tuple[str, str] | None:
    rendered = os.path.normpath(str(candidate))
    if is_system_path(rendered):
        if system_cache_has_arch(Path(rendered), expected_arch):
            return rendered, "apple-system"
        return None
    try:
        resolved = candidate.resolve(strict=True)
    except OSError:
        return None
    if path_within(resolved, bundle):
        return str(resolved), "bundle"
    return None


@functools.lru_cache(maxsize=512)
def system_cache_has_arch(candidate: Path, expected_arch: str) -> bool:
    try:
        completed = subprocess.run(
            ["dyld_info", "-arch", expected_arch, "-platform", str(candidate)],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError as error:
        fail(f"cannot inspect Apple system image {candidate} with dyld_info: {error}")
    header = re.search(
        rf"^{re.escape(str(candidate))} \[([^\]]+)\]:$",
        completed.stdout,
        re.MULTILINE,
    )
    if completed.returncode == 0 and header:
        image_arch = header.group(1)
        return image_arch == expected_arch or (
            expected_arch == "arm64" and image_arch.startswith("arm64")
        )
    if "does not contain specified arch" in completed.stdout:
        return False
    fail(f"cannot verify Apple system @rpath candidate {candidate}: {completed.stdout.strip()}")


def classify_rpath_candidate(
    candidate: Path, bundle: Path, expected_arch: str
) -> tuple[str, str] | None:
    rendered = os.path.normpath(str(candidate))
    if is_system_path(rendered):
        if system_cache_has_arch(Path(rendered), expected_arch):
            return rendered, "apple-system"
        return None
    try:
        resolved = candidate.resolve(strict=True)
    except OSError:
        return None
    if path_within(resolved, bundle):
        return str(resolved), "bundle"
    return None


def expand_runpath(value: str, image: Path, owner: Path, bundle: Path) -> str:
    expanded = expand_image_path(value, image, owner)
    if expanded is None:
        fail(f"unsupported LC_RPATH origin in {image}: {value}")
    normalized = Path(os.path.normpath(str(expanded)))
    rendered = str(normalized)
    if not is_system_path(rendered) and not path_within(normalized, bundle):
        fail(f"external LC_RPATH in {image}: {value} resolves to {rendered}")
    return rendered


def expand_install_name(
    value: str,
    image: Path,
    owner: Path,
    bundle: Path,
    effective_rpaths: list[str],
    expected_arch: str,
) -> tuple[str, str] | None:
    if value.startswith("@rpath/"):
        suffix = value[len("@rpath/") :]
        for rpath in effective_rpaths:
            resolved = classify_rpath_candidate(
                Path(rpath) / suffix, bundle, expected_arch
            )
            if resolved is not None:
                return resolved
        return None
    expanded = expand_image_path(value, image, owner)
    if expanded is None:
        return None
    return classify_resolved_path(expanded, bundle, expected_arch)


def inspect_load_metadata(image: Path) -> tuple[list[str], str | None]:
    output = run(["otool", "-l", str(image)])
    lines = output.splitlines()
    paths: list[str] = []
    identifiers: list[str] = []
    for index, line in enumerate(lines):
        command = line.strip()
        if command not in {"cmd LC_RPATH", "cmd LC_ID_DYLIB"}:
            continue
        field = "path" if command == "cmd LC_RPATH" else "name"
        value = None
        for following in lines[index + 1 : index + 6]:
            match = re.match(rf"\s*{field}\s+(.+?)\s+\(offset\s+\d+\)", following)
            if match:
                value = match.group(1)
                break
        if value is None:
            fail(f"cannot parse {command.removeprefix('cmd ')} from {image}")
        if command == "cmd LC_RPATH":
            paths.append(value)
        else:
            identifiers.append(value)
    if len(identifiers) > 1:
        fail(f"Mach-O image has multiple LC_ID_DYLIB commands: {image}")
    return paths, identifiers[0] if identifiers else None


def inspect_dependencies(
    images: list[Path], bundle: Path, process_roots: list[Path], expected_arch: str
) -> dict[Path, dict[str, Any]]:
    image_set = {path.resolve(strict=True) for path in images}
    root_set = {path.resolve(strict=True) for path in process_roots}
    load_cache: dict[Path, tuple[list[str], list[str], str | None]] = {}
    resolved_by_image: dict[
        Path, dict[tuple[str, str, str, tuple[str, ...], str], dict[str, Any]]
    ] = {
        path.resolve(strict=True): {} for path in images
    }
    loaded_by_process: dict[Path, set[Path]] = {owner: set() for owner in root_set}

    def load_commands(image: Path) -> tuple[list[str], list[str], str | None]:
        canonical = image.resolve(strict=True)
        if canonical not in load_cache:
            output = run(["otool", "-L", str(canonical)])
            load_paths = []
            for line in output.splitlines()[1:]:
                line = line.strip()
                if line:
                    load_paths.append(line.split(" (compatibility version ", 1)[0])
            rpaths, dylib_id = inspect_load_metadata(canonical)
            dependencies = [name for name in load_paths if name != dylib_id]
            load_cache[canonical] = (dependencies, rpaths, dylib_id)
        return load_cache[canonical]

    def visit(
        image: Path,
        owner: Path,
        inherited_rpaths: tuple[str, ...],
        active_images: set[Path],
    ) -> None:
        image = image.resolve(strict=True)
        owner = owner.resolve(strict=True)
        loaded_images = loaded_by_process.setdefault(owner, set())
        if image in active_images or image in loaded_images:
            return
        dependencies, declared_rpaths, _ = load_commands(image)
        own_rpaths = tuple(
            expand_runpath(rpath, image, owner, bundle) for rpath in declared_rpaths
        )
        # dyld walks the run-path linked list from the current loader back
        # through its parents. The current image's LC_RPATH entries therefore
        # precede inherited paths from its loader chain.
        effective_rpaths = own_rpaths + inherited_rpaths
        # dyld reuses an already-loaded image by path; marking it before
        # walking dependencies also terminates A -> B -> A cycles.
        loaded_images.add(image)
        active_images.add(image)
        try:
            for install_name in dependencies:
                resolution = expand_install_name(
                    install_name,
                    image,
                    owner,
                    bundle,
                    list(effective_rpaths),
                    expected_arch,
                )
                if resolution is None:
                    fail(f"unresolved or external Mach-O dependency in {image}: {install_name}")
                resolved_path, source = resolution
                owner_relative = owner.relative_to(bundle).as_posix()
                key = (install_name, resolved_path, source, effective_rpaths, owner_relative)
                resolved_by_image[image][key] = {
                    "install_name": install_name,
                    "resolved_path": resolved_path,
                    "source": source,
                    "runpath_stack": list(effective_rpaths),
                    "owner_executable": owner_relative,
                }
                if source == "bundle":
                    dependency_path = Path(resolved_path).resolve(strict=True)
                    if dependency_path not in image_set:
                        fail(f"bundled Mach-O dependency is not a recognized Mach-O image: {resolved_path}")
                    visit(dependency_path, owner, effective_rpaths, active_images)
        finally:
            active_images.remove(image)

    roots = [path.resolve(strict=True) for path in process_roots]
    if not roots or any(root not in image_set for root in roots):
        fail("cannot identify the app and helper process roots for Mach-O closure analysis")
    for root in sorted(roots):
        visit(root, root, (), set())

    # Some runtime libraries are loaded dynamically and are not reachable from
    # LC_LOAD_* edges. Their process context comes from the bundle's explicit
    # runtime payload directory, or from every process that could load a shared
    # Contents/Frameworks image. Evaluate the dependency graph per such owner;
    # do not assign a shared library one permanent executable identity.
    for image in sorted(images):
        canonical = image.resolve(strict=True)
        candidates = process_contexts(image, bundle, roots)
        for owner in candidates:
            if canonical == owner:
                initial = ()
            else:
                owner_dependencies, owner_rpaths, _ = load_commands(owner)
                del owner_dependencies
                initial = tuple(
                    expand_runpath(rpath, owner, owner, bundle) for rpath in owner_rpaths
                )
            visit(canonical, owner, initial, set())

    return {
        image.resolve(strict=True): {
            "dylib_id": load_commands(image)[2],
            "dependencies": [value for _, value in sorted(entries.items())],
        }
        for image, entries in resolved_by_image.items()
    }


def process_contexts(image: Path, bundle: Path, process_roots: list[Path]) -> list[Path]:
    canonical = image.resolve(strict=True)
    root_set = {path.resolve(strict=True) for path in process_roots}
    relative = image.relative_to(bundle).as_posix()
    root_relatives = {root.relative_to(bundle).as_posix(): root for root in root_set}
    relatives = process_contexts_for_relative(relative, set(root_relatives))
    owners = [root_relatives[owner] for owner in relatives]
    if any(owner.resolve(strict=True) not in root_set for owner in owners):
        fail(f"runtime image {relative} refers to a non-root process owner")
    return sorted(owners)


def process_contexts_for_relative(relative: str, process_root_relatives: set[str]) -> list[str]:
    if relative in process_root_relatives:
        return [relative]
    helper_owners = {
        "Contents/Resources/dotnet/": "Contents/Resources/dotnet/dotnet",
        "Contents/Resources/Helpers/csharp/": "Contents/Resources/dotnet/dotnet",
        "Contents/Resources/Helpers/go/": "Contents/Resources/Helpers/go/oracle",
        "Contents/Resources/Helpers/python/": "Contents/Resources/Helpers/python/pyrefly",
        "Contents/Resources/Helpers/typescript/": "Contents/Resources/Helpers/typescript/node/bin/node",
    }
    for prefix, owner_relative in helper_owners.items():
        if relative.startswith(prefix):
            if owner_relative not in process_root_relatives:
                fail(f"runtime image {relative} refers to a non-root process owner {owner_relative}")
            return [owner_relative]
    if relative.startswith(("Contents/Frameworks/", "Contents/MacOS/")):
        return sorted(process_root_relatives)
    fail(f"Mach-O image has no declared process context in the bundle layout: {relative}")


def inspect_signature(path: Path) -> str:
    completed = subprocess.run(
        ["/usr/bin/codesign", "--display", "--verbose=4", str(path)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    output = completed.stdout
    if "code object is not signed at all" in output:
        return "unsigned"
    is_adhoc = "signature=adhoc" in output.lower()
    is_signed = "Authority=" in output or "TeamIdentifier=" in output
    if not completed.returncode and not is_adhoc and not is_signed:
        fail(f"cannot classify code-signature state for {path}: {output.strip()}")
    verification = subprocess.run(
        ["/usr/bin/codesign", "--verify", "--strict", str(path)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if verification.returncode != 0:
        return "invalidated" if is_adhoc or is_signed else "invalid"
    if is_adhoc:
        return "ad-hoc"
    if is_signed:
        return "signed"
    return "unsigned"


def minimum_macos(image: Path) -> str:
    output = run(["vtool", "-show-build", str(image)])
    match = re.search(r"^\s*minos\s+(\d+(?:\.\d+){1,2})\s*$", output, re.MULTILINE)
    if match:
        return match.group(1)
    output = run(["otool", "-l", str(image)])
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if line.strip() == "cmd LC_VERSION_MIN_MACOSX":
            for following in lines[index + 1 : index + 8]:
                version = re.match(r"\s*version\s+(\d+(?:\.\d+){1,2})\s*$", following)
                if version:
                    return version.group(1)
    fail(f"cannot determine the minimum macOS load-command version for {image}")


def version_tuple(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in value.split("."))


def inspect_macho_tree(
    bundle: Path,
    expected_arch: str,
    bundle_minimum: str,
    required_images: set[str],
) -> list[dict[str, Any]]:
    images: list[Path] = []
    for path in bundle.rglob("*"):
        if not path.is_file():
            continue
        with path.open("rb") as stream:
            magic = stream.read(4)
        if magic not in MACHO_MAGICS:
            continue
        description = run(["file", "-b", str(path)])
        if "Mach-O" not in description:
            fail(f"Mach-O magic candidate is not recognized by file(1): {path.relative_to(bundle)}")
        images.append(path)
    if not images:
        fail("bundle contains no Mach-O images")
    detected = {path.relative_to(bundle).as_posix() for path in images}
    missing = sorted(required_images - detected)
    if missing:
        fail(f"required native executables are missing or not Mach-O: {', '.join(missing)}")
    process_roots = [bundle / relative for relative in sorted(required_images)]
    dependency_records = inspect_dependencies(images, bundle, process_roots, expected_arch)
    records: list[dict[str, Any]] = []
    for image in sorted(images):
        architectures = run(["lipo", "-archs", str(image)]).split()
        if architectures != [expected_arch]:
            fail(f"{image.relative_to(bundle)} has architecture slices {architectures}, expected only {expected_arch}")
        minimum = minimum_macos(image)
        padded_bundle_version = bundle_minimum + ".0" if len(bundle_minimum.split(".")) == 2 else bundle_minimum
        if version_tuple(minimum) > version_tuple(padded_bundle_version):
            fail(f"{image.relative_to(bundle)} requires macOS {minimum}, above bundle minimum {bundle_minimum}")
        relative = image.relative_to(bundle).as_posix()
        load_metadata = dependency_records[image.resolve(strict=True)]
        records.append(
            {
                "path": relative,
                "sha256": sha256(image),
                "architectures": architectures,
                "minimum_macos": minimum,
                "signature": inspect_signature(image),
                "dylib_id": load_metadata["dylib_id"],
                "dependencies": load_metadata["dependencies"],
            }
        )
    return records


def write_application_launcher(macos: Path, *, sdk_only: bool = False) -> None:
    """Preserve caller TypeScript settings for the shared Rust host resolver.

    The host admits TypeScript from the selected package. Inherited explicit
    runtime and compiler paths remain available. Bundled Node is admitted
    through the executable's manifest, rather than an ambient override.
    """
    environment = (
        "#!/bin/sh\n"
        "set -eu\n"
        'self=$0\n'
        'while [ -L "$self" ]; do\n'
        '  link=$(/usr/bin/readlink "$self")\n'
        '  case "$link" in\n'
        '    /*) self=$link ;;\n'
        '    *) self="$(dirname "$self")/$link" ;;\n'
        '  esac\n'
        'done\n'
        'contents=$(CDPATH= cd "$(dirname "$self")/.." && pwd -P)\n'
    )
    if not sdk_only:
        environment += (
            'export NUDOX_DOTNET="${NUDOX_DOTNET-$contents/Resources/dotnet/dotnet}"\n'
            'export NUDOX_ROSLYN_HELPER="${NUDOX_ROSLYN_HELPER-$contents/Resources/Helpers/csharp/oracle.dll}"\n'
            'export NUDOX_GO_ORACLE="${NUDOX_GO_ORACLE-$contents/Resources/Helpers/go/oracle}"\n'
            'export NUDOX_GO_ORACLE_BIN="${NUDOX_GO_ORACLE_BIN-$contents/Resources/Helpers/go/oracle}"\n'
            'export NUDOX_PYREFLY="${NUDOX_PYREFLY-$contents/Resources/Helpers/python/pyrefly}"\n'
        )
    # Package-manager links must initialize the same bundled helpers as Finder.
    for name, executable in {
        "Nudox": "backend-desktop", "nudox-cli": "backend-cli",
        "nudox-mcp": "backend-mcp", "nudox-locald": "backend-locald",
    }.items():
        launcher = macos / name
        launcher.write_text(environment + f'exec "$contents/MacOS/{executable}" "$@"\n', encoding="utf-8")
        launcher.chmod(0o755)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path, help="clean checkout at the pinned application revision")
    parser.add_argument("--cargo-bundle", type=Path, help="pinned cargo-bundle 0.12.0 executable; use for release app skeleton")
    parser.add_argument("--minimum-os", default="12.0", help="explicitly reviewed minimum macOS version")
    parser.add_argument("--icon", type=Path, help="approved .icns file for public distribution")
    parser.add_argument("--expected-icon-sha256", help="content pin for the approved icon")
    parser.add_argument("--expected-revision", required=True, help="full operator-reviewed application Git commit SHA")
    parser.add_argument("--expected-tree", required=True, help="full operator-reviewed application Git tree SHA")
    parser.add_argument("--expected-runner-sha256", required=True, help="full operator-reviewed Cargo runner SHA-256 from the application build")
    parser.add_argument("--artifact-dir", required=True, type=Path, help="directory containing the three admitted application binaries")
    parser.add_argument("--build-receipt", required=True, type=Path, help="Root-produced application build receipt JSON")
    parser.add_argument("--sdk-only", action="store_true", help="bundle only genuine Node/TypeScript; no legacy language-helper claims or overrides")
    parser.add_argument("--defer-sdk-runtime-probes", action="store_true", help="SDK release driver will probe after inner Developer ID signing, before the outer app seal")
    parser.add_argument("--dotnet-root", type=Path, help="real macOS .NET runtime installation root")
    parser.add_argument("--dotnet-receipt", type=Path, help="content-bound .NET runtime origin receipt")
    parser.add_argument("--dotnet-pin", type=Path, help="reviewed source/version/tree pin for the .NET distribution")
    parser.add_argument("--dotnet-source-archive", type=Path, help="exact source archive when the .NET pin uses verified-archive")
    parser.add_argument("--roslyn-dir", type=Path, help="real framework-dependent oracle publish output directory")
    parser.add_argument("--roslyn-receipt", type=Path, help="Root-produced locked Roslyn publish receipt JSON")
    parser.add_argument("--helpers-dir", required=True, type=Path, help="receipted real Go, Pyrefly, Node, and TypeScript helper payload")
    parser.add_argument("--helpers-receipt", required=True, type=Path, help="Root-produced compiler-helper receipt JSON")
    parser.add_argument("--relocation-plan", type=Path, help="collector-produced plan referenced by all four admitted origin receipts")
    parser.add_argument("--relocation-package-root", action="append", default=[], metavar="PACKAGE=PATH", help="exact pinned source root for a third-party Mach-O package")
    parser.add_argument("--output-dir", required=True, type=Path, help="new directory for .app, zip, and receipt")
    parser.add_argument("--target", choices=MACOS_TARGETS, default="aarch64-apple-darwin")
    args = parser.parse_args()

    legacy_inputs = [args.dotnet_root, args.dotnet_receipt, args.dotnet_pin, args.dotnet_source_archive, args.roslyn_dir, args.roslyn_receipt]
    if args.defer_sdk_runtime_probes and not args.sdk_only:
        fail("deferred SDK probes require SDK-only assembly")
    if args.sdk_only and any(value is not None for value in legacy_inputs):
        fail("SDK-only assembly must not include unrelated legacy helper inputs")
    if not args.sdk_only and any(value is None for value in [args.dotnet_root, args.dotnet_receipt, args.dotnet_pin, args.roslyn_dir, args.roslyn_receipt]):
        fail("full-helper assembly requires all .NET and Roslyn inputs")
    macos_tools()
    target_arch = MACOS_TARGETS[args.target]
    host_arch = platform.machine().lower()
    if host_arch == "aarch64":
        host_arch = "arm64"
    if host_arch != target_arch:
        fail(f"native packaging host is {host_arch}; requested single-architecture target is {target_arch}")

    source_root = args.source_root.resolve(strict=True)
    artifact_dir = args.artifact_dir.resolve(strict=True)
    roslyn_dir = args.roslyn_dir.resolve(strict=True) if args.roslyn_dir else None
    helpers_dir = args.helpers_dir.resolve(strict=True)
    dotnet_root = args.dotnet_root.resolve(strict=True) if args.dotnet_root else None
    if args.output_dir.exists() or args.output_dir.is_symlink():
        fail(f"output directory already exists; refusing to reuse it: {args.output_dir}")
    output_dir = args.output_dir.absolute()
    if output_dir.exists() or output_dir.is_symlink():
        fail(f"output directory already exists; refusing to reuse it: {output_dir}")

    if re.fullmatch(r"[0-9a-f]{40}", args.expected_revision) is None:
        fail("--expected-revision must be a full lowercase Git commit SHA")
    if re.fullmatch(r"[0-9a-f]{40}", args.expected_tree) is None:
        fail("--expected-tree must be a full lowercase Git tree SHA")
    if re.fullmatch(r"[0-9a-f]{64}", args.expected_runner_sha256) is None:
        fail("--expected-runner-sha256 must be a full lowercase SHA-256 digest")
    source = validate_source(source_root, args.expected_revision, args.expected_tree)
    app_receipt, binaries = validate_app_build(
        args.build_receipt.resolve(strict=True), artifact_dir, source, args.target, args.expected_runner_sha256
    )
    helpers_receipt, helper_files = validate_helper_payload(
        args.helpers_receipt.resolve(strict=True), helpers_dir, source_root, source, args.target, sdk_only=args.sdk_only
    )
    if args.sdk_only and any(path.stat().st_size > 512 * 1024**2 for path in binaries.values()):
        fail("SDK application executable exceeds the existing host 512 MiB admission bound")
    roslyn_receipt = dotnet_receipt = None
    roslyn_files = dotnet_files = {}
    dotnet_receipt_path = None
    if not args.sdk_only:
        roslyn_receipt, roslyn_files = validate_roslyn_build(
            args.roslyn_receipt.resolve(strict=True), roslyn_dir, source_root, source, args.target
        )
        dotnet_receipt_path = args.dotnet_receipt.resolve(strict=True)
        dotnet_receipt, _ = origin_receipt(dotnet_receipt_path, "dotnet-runtime")
        try:
            dotnet_files = validate_dotnet_runtime_receipt(
                dotnet_receipt,
                dotnet_root,
                args.target,
                args.dotnet_pin.resolve(strict=True),
                args.dotnet_source_archive.resolve(strict=True) if args.dotnet_source_archive else None,
            )
        except RelocationInputError as error:
            fail(str(error))
        dotnet = dotnet_root / "dotnet"
        if not dotnet.is_file() or not os.access(dotnet, os.X_OK):
            fail(f"dotnet host is not executable: {dotnet}")

    source_root_paths = {"application": artifact_dir, "helpers": helpers_dir}
    admitted_receipt_paths = {"application": args.build_receipt.resolve(strict=True), "helpers": args.helpers_receipt.resolve(strict=True)}
    if not args.sdk_only:
        source_root_paths.update({"roslyn": roslyn_dir, "dotnet-runtime": dotnet_root})
        admitted_receipt_paths.update({"roslyn": args.roslyn_receipt.resolve(strict=True), "dotnet-runtime": dotnet_receipt_path})
    plan, plan_digest = relocation_inputs(
        admitted_receipt_paths,
        args.relocation_plan.resolve(strict=True) if args.relocation_plan else None,
        source,
        args.target,
        source_root_paths,
    )
    relocation_package_roots = parse_named_paths(
        args.relocation_package_root, "relocation package root"
    ) if args.relocation_package_root else {}
    if plan is None and relocation_package_roots:
        fail("relocation package roots were supplied without admitted origin receipts")

    plist_path = source_root / "apps/desktop/macos/Info.plist"
    with plist_path.open("rb") as stream:
        plist = plistlib.load(stream)
    if plist.get("CFBundleExecutable") != "Nudox" or plist.get("CFBundlePackageType") != "APPL":
        fail("Info.plist does not describe the expected Nudox application bundle")
    if re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", args.minimum_os) is None:
        fail("invalid minimum macOS version")
    plist["LSMinimumSystemVersion"] = args.minimum_os
    version = tomllib.loads((source_root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    plist["CFBundleShortVersionString"] = version
    plist["CFBundleVersion"] = version
    if args.icon:
        if args.icon.suffix != ".icns" or sha256(args.icon) != args.expected_icon_sha256:
            fail("approved .icns icon does not match its content pin")
        plist["CFBundleIconFile"] = "Nudox.icns"
    elif args.expected_icon_sha256:
        fail("icon digest provided without its file")

    output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nudox-package-", dir=output_dir.parent) as temporary:
        staging = Path(temporary) / "Nudox.app"
        if args.cargo_bundle:
            tool = args.cargo_bundle.resolve(strict=True)
            if "0.12.0" not in run([str(tool), "--version"]):
                fail("release packaging requires pinned cargo-bundle 0.12.0")
            environment = dict(os.environ)
            # cargo-bundle 0.12 warning output cannot reset a dumb terminal.
            environment["TERM"] = "xterm-256color"
            environment["CARGO_TARGET_DIR"] = str(Path(temporary) / "target")
            command = [str(tool), "bundle", "--package", "backend-desktop", "--bin", "backend-desktop", "--release", "--target", args.target, "--format", "osx", "--binary-path", str(binaries["backend-desktop"])]
            completed = subprocess.run(command, cwd=source_root, env=environment, check=False)
            if completed.returncode:
                fail("cargo-bundle failed to create the release app skeleton")
            generated = Path(temporary) / "target" / args.target / "release/bundle/osx/Nudox.app"
            if not generated.is_dir():
                fail("cargo-bundle did not produce the expected Nudox.app")
            shutil.copytree(generated, staging, symlinks=True)
        macos = staging / "Contents/MacOS"
        resources = staging / "Contents/Resources"
        macos.mkdir(parents=True, exist_ok=True)
        resources.mkdir(parents=True, exist_ok=True)
        (staging / "Contents/Info.plist").write_bytes(plistlib.dumps(plist))
        if args.icon:
            shutil.copyfile(args.icon, resources / "Nudox.icns")

        for name, source_binary in binaries.items():
            destination_name = "backend-desktop" if name == "backend-desktop" else name
            destination = macos / destination_name
            shutil.copy2(source_binary, destination)
            destination.chmod(0o755)

        write_application_launcher(macos, sdk_only=args.sdk_only)

        helper_resources = resources / "Helpers"
        shutil.copytree(helpers_dir, helper_resources, symlinks=False)
        runtime_notices = {}
        if not args.sdk_only:
            shutil.copytree(roslyn_dir, helper_resources / "csharp", symlinks=False)
            shutil.copyfile(
                source_root / "frontends/typescript/src/legacy/checker/main.cjs",
                helper_resources / "typescript/checker-main.cjs",
            )
            typescript_dir = helper_resources / "typescript"
            (typescript_dir / "report-program").write_text(
                "#!/bin/sh\nset -eu\n"
                'here=$(CDPATH= cd "$(dirname "$0")" && pwd)\n'
                'node=${NUDOX_TYPESCRIPT_NODE-$here/node/bin/node}\n'
                'exec "$node" "$here/checker-main.cjs" "$@"\n',
                encoding="utf-8",
            )
            (typescript_dir / "report-program").chmod(0o755)
            (typescript_dir / "tsc").write_text(
                "#!/bin/sh\nset -eu\n"
                'here=$(CDPATH= cd "$(dirname "$0")" && pwd)\n'
                'node=${NUDOX_TYPESCRIPT_NODE-$here/node/bin/node}\n'
                'exec "$node" "$here/node_modules/typescript/bin/tsc" "$@"\n',
                encoding="utf-8",
            )
            (typescript_dir / "tsc").chmod(0o755)
            runtime_notices = copy_runtime(dotnet_root, resources / "dotnet", dotnet_receipt)
        licenses = resources / "Font Licenses"
        licenses.mkdir()
        for name in FONT_LICENSES:
            source_license = source_root / "apps/facet/resources/fonts" / name
            if not source_license.is_file():
                fail(f"required embedded-font license is missing from source: {source_license}")
            shutil.copy2(source_license, licenses / name)

        provenance = resources / "Build Evidence"
        provenance.mkdir()
        shutil.copy2(args.build_receipt, provenance / "application-build-receipt.json")
        if args.sdk_only:
            shutil.copy2(args.helpers_receipt, provenance / "compiler-helpers-source-receipt.json")
        else:
            shutil.copy2(args.roslyn_receipt, provenance / "roslyn-build-receipt.json")
            shutil.copy2(args.helpers_receipt, provenance / "compiler-helpers-receipt.json")
            shutil.copy2(dotnet_receipt_path, provenance / "dotnet-runtime-receipt.json")

        info = run(["plutil", "-lint", str(staging / "Contents/Info.plist")])
        if "OK" not in info:
            fail("Info.plist did not pass plutil validation")
        required_images = {f"Contents/MacOS/{name}" for name in EXECUTABLES}
        required_images.add("Contents/Resources/Helpers/typescript/node/bin/node")
        if not args.sdk_only:
            required_images.add("Contents/Resources/dotnet/dotnet")
            required_images.update(f"Contents/Resources/Helpers/{relative}" for relative in HELPER_EXECUTABLES.values())
        relocation_record = {
            "plan_sha256": None,
            "images": [],
            "notices": [],
            "signature_policy": "no relocation plan was attached; only already-bundled or Apple system dependencies can pass closure audit",
        }
        if plan is not None:
            relocation_record = apply_macho_relocation(
                staging, plan, relocation_package_roots
            )
            relocation_record["plan_sha256"] = plan_digest
            shutil.copy2(
                args.relocation_plan.resolve(strict=True),
                provenance / "macho-relocation-plan.json",
            )
        macho_records = inspect_macho_tree(
            staging,
            target_arch,
            plist["LSMinimumSystemVersion"],
            required_images,
        )
        bundle_helper_receipt_path = args.helpers_receipt
        sdk_runtime_probes = None
        if args.sdk_only:
            helpers_receipt, bundle_helper_receipt_path = write_sdk_bundle_receipt(
                helper_resources, provenance, source, args.target, helpers_receipt, args.helpers_receipt
            )
            helper_files = helpers_receipt["files"]
            if not args.defer_sdk_runtime_probes:
                sdk_runtime_probes = verify_sdk_runtime(helper_resources, helpers_receipt)
        bundle_signature = inspect_signature(staging)

        manifest = {
            "schema": 1,
            "product": "Nudox",
            "source": source,
            "target": {"triple": args.target, "architecture": target_arch},
            "bundle": {
                "identifier": plist["CFBundleIdentifier"],
                "version": plist["CFBundleShortVersionString"],
                "build": plist["CFBundleVersion"],
                "minimum_macos": plist["LSMinimumSystemVersion"],
                "info_plist_sha256": sha256(staging / "Contents/Info.plist"),
                "signature": bundle_signature,
                "notarization": "not performed",
            },
            "application_build": {
                "receipt_sha256": sha256(args.build_receipt),
                "profile": app_receipt["profile"],
                "command": app_receipt["command"],
                "cargo_runner": app_receipt["cargo_runner_before"],
                "cargo_runner_unchanged": app_receipt["cargo_runner_unchanged"],
                "cargo_provenance": app_receipt["cargo_provenance"],
                "executables": {name: sha256(path) for name, path in binaries.items()},
            },
            "compiler_helpers": {
                "receipt_sha256": sha256(bundle_helper_receipt_path),
                "files": helper_files,
                "tools": helpers_receipt["tools"],
                "notices": helpers_receipt["notices"],
                "typescript_driver_sha256": sha256(helper_resources / "typescript/checker-main.cjs") if not args.sdk_only else None,
                "external_language_prerequisites": {
                    "rust": "paired rustc and cargo plus Cargo registry cache for offline package resolution",
                    "clang": "clang plus a compatible libclang selected through LIBCLANG_PATH",
                    "python": "python3; pyrefly is bundled",
                    "go": "go toolchain and Go module cache; the relocatable Go oracle is bundled",
                    "java": "a JDK containing java and javac",
                },
            },
            "macho_relocation": relocation_record,
            "font_licenses": {name: sha256(licenses / name) for name in FONT_LICENSES},
            "macho_images": macho_records,
            "files": file_inventory(staging),
            "distribution_status": f"app bundle {bundle_signature}; notarization not performed; native QA pending",
        }
        if not args.sdk_only:
            manifest["roslyn_build"] = {
                "receipt_sha256": sha256(args.roslyn_receipt),
                "runtime_identifier": roslyn_receipt["runtime_identifier"],
                "framework_dependent": True,
                "commands": roslyn_receipt["commands"],
                "notices": roslyn_receipt["notices"],
                "files": roslyn_files,
            }
            manifest["dotnet_runtime"] = {
                "receipt_sha256": sha256(dotnet_receipt_path),
                "package": dotnet_receipt["package"],
                "distribution": dotnet_receipt["distribution"],
                "pin_descriptor_sha256": dotnet_receipt["pin_descriptor_sha256"],
                "root_tree_sha256": dotnet_receipt["root_tree_sha256"],
                "files": dotnet_files,
                "notices": runtime_notices,
            }
        if args.sdk_only:
            helpers = manifest["compiler_helpers"]
            del helpers["typescript_driver_sha256"]
            helpers["assembly_mode"] = "typescript-sdk-only"
            helpers["source_receipt_sha256"] = sha256(args.helpers_receipt)
            helpers["runtime_probes"] = sdk_runtime_probes
            helpers["runtime_probe_status"] = ("pending post-sign native probes" if args.defer_sdk_runtime_probes
                                                else "passed relocated Node and Compiler API probes")
            helpers["external_language_prerequisites"].update({
                "python": "native in-process Python authority; no Pyrefly executable bundled",
                "go": "ordinary external Go toolchain and dependency cache; no legacy oracle bundled",
                "csharp": "not bundled; requires separately selected external authority",
            })
        manifest_path = resources / "build-manifest.json"
        manifest_bytes = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode("utf-8")
        write_new_bytes(manifest_path, manifest_bytes, label="bundle manifest")

        final_app = output_dir / "Nudox.app"
        zip_path = output_dir / "Nudox-macOS.zip"
        output_dir.mkdir()
        shutil.copytree(staging, final_app, symlinks=True)
        run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(final_app), str(zip_path)])
        receipt = {
            "schema": 1,
            "bundle_manifest_sha256": sha256(final_app / "Contents/Resources/build-manifest.json"),
            "zip_sha256": sha256(zip_path),
            "zip_size_bytes": zip_path.stat().st_size,
            "application_source": source,
            "package_status": f"app bundle {bundle_signature}; notarization not performed; native QA pending",
        }
        receipt_path = output_dir / "Nudox-macOS.receipt.json"
        receipt_bytes = (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode("utf-8")
        write_new_bytes(receipt_path, receipt_bytes, label="bundle receipt")

    print(f"Created {output_dir / 'Nudox.app'}")
    print(f"Created {output_dir / 'Nudox-macOS.zip'}")
    print(f"Created {output_dir / 'Nudox-macOS.receipt.json'}")
    print("This receipt records packaging evidence; it does not claim signing, notarization, or native QA.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (PackageError, RelocationInputError, OSError, ValueError) as error:
        print(f"package refused: {error}", file=sys.stderr)
        sys.exit(2)
