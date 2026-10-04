#!/usr/bin/env python3
"""Assemble an auditable macOS investor bundle from admitted build outputs.

This script does not build, sign, notarize, or run the application. It accepts
only outputs carrying build receipts for the pinned, clean application source
revision and refuses unresolved non-system Mach-O dependencies.
"""

from __future__ import annotations

import argparse
import functools
import hashlib
import json
import os
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


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
EXECUTABLES = ("backend-desktop", "backend-mcp", "backend-locald")
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


def file_inventory(root: Path) -> dict[str, dict[str, Any]]:
    inventory: dict[str, dict[str, Any]] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            inventory[relative] = {"kind": "symlink", "target": os.readlink(path)}
        elif path.is_file():
            inventory[relative] = {
                "kind": "file",
                "sha256": sha256(path),
                "size_bytes": path.stat().st_size,
            }
    return inventory


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
    receipt = load_json(receipt_path, "application build receipt")
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
    runner_assets = runner_before.get("referenced_asset_sha256")
    if not isinstance(runner_assets, dict) or not {
        "runner",
        "runner.interpreter",
        "runner.environment.0",
        "runner.tool.RUSTC",
        "runner.tool.RUSTC_WRAPPER",
        "runner.exec",
    }.issubset(runner_assets):
        fail("application receipt is missing hashes for pinned runner and toolchain references")
    for role, digest in runner_assets.items():
        if not isinstance(role, str) or not role or role in {"path", "paths"}:
            fail("Cargo runner asset roles must not contain local paths")
        require_sha(digest, f"Cargo runner asset {role}")
    if runner_assets.get("runner") != runner_before["sha256"]:
        fail("Cargo runner asset manifest does not match its top-level content pin")
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
    ]
    if command != expected_command:
        fail("application receipt must record one locked release build through the pinned shebang runner")
    provenance = receipt.get("cargo_provenance")
    if not isinstance(provenance, dict):
        fail("application receipt is missing the pinned Cargo wrapper provenance")
    require_sha(provenance.get("record_sha256"), "Cargo provenance record")
    if (
        provenance.get("schema") != 2
        or provenance.get("git_head") != source["git_revision"]
        or not isinstance(provenance.get("source_dirty_sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", provenance["source_dirty_sha256"]) is None
        or provenance.get("source_unchanged") is not True
        or provenance.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
        or provenance.get("cargo_exit_status") != 0
        or provenance.get("features")
        != {
            "features": [],
            "all_features": False,
            "no_default_features": False,
            "targets": [target],
        }
        or provenance.get("toolchain_capture_complete") is not True
        or provenance.get("toolchain_unchanged") is not True
    ):
        fail("Cargo provenance does not bind a successful unchanged locked build to this source/target")
    versions = provenance.get("toolchain")
    if not isinstance(versions, dict) or any(
        not isinstance(versions.get(name), str) or not versions[name].strip()
        for name in ("cargo", "rustc", "rustdoc")
    ):
        fail("Cargo provenance must identify Cargo, rustc, and rustdoc versions")
    wrapper_hashes = provenance.get("wrapper_sha256")
    if not isinstance(wrapper_hashes, dict) or set(wrapper_hashes) != {"runtime", "source", "rustc"}:
        fail("Cargo provenance must identify the pinned Cargo wrapper files")
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
    listed = receipt.get("executables")
    if not isinstance(listed, dict) or set(listed) != set(EXECUTABLES):
        fail(f"application build receipt must hash exactly {', '.join(EXECUTABLES)}")
    entries = list(artifact_dir.iterdir())
    if any(path.is_symlink() or not path.is_file() for path in entries) or {
        path.name for path in entries
    } != set(EXECUTABLES):
        fail("application artifact directory must contain exactly the three receipt-bound regular files")
    paths: dict[str, Path] = {}
    provenance_outputs = {item["path"]: item["sha256"] for item in outputs}
    for name in EXECUTABLES:
        path = artifact_dir / name
        if not path.is_file() or not os.access(path, os.X_OK):
            fail(f"missing executable build output: {path}")
        if sha256(path) != require_sha(listed[name], f"application receipt {name}"):
            fail(f"application build receipt hash mismatch for {name}")
        target_output = f"{target}/release/{name}"
        if target_output in provenance_outputs and sha256(path) != provenance_outputs[target_output]:
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
    receipt = load_json(receipt_path, "Roslyn helper build receipt")
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


def validate_helper_payload(
    receipt_path: Path,
    helper_dir: Path,
    source_root: Path,
    source: dict[str, str],
    target: str,
) -> tuple[dict[str, Any], dict[str, str]]:
    receipt = load_json(receipt_path, "compiler-helper receipt")
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


def copy_runtime(dotnet_root: Path, destination: Path) -> dict[str, str]:
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
    notices = ("LICENSE.txt", "ThirdPartyNotices.txt")
    for name in notices:
        if not (dotnet_root / name).is_file():
            fail(f"dotnet distribution is missing its {name} notice")
    destination.mkdir(parents=True)
    shutil.copy2(dotnet, destination / "dotnet")
    shutil.copytree(host, destination / "host", symlinks=False)
    shutil.copytree(shared, destination / "shared", symlinks=False)
    for name in notices:
        shutil.copy2(dotnet_root / name, destination / name)
    return {name: sha256(dotnet_root / name) for name in notices}


def macos_tools() -> None:
    if platform.system() != "Darwin":
        fail("bundle assembly and Mach-O closure verification require macOS")
    missing = [name for name in ("codesign", "ditto", "dyld_info", "file", "lipo", "otool", "plutil", "vtool") if shutil.which(name) is None]
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
    if canonical in root_set:
        return [canonical]
    relative = image.relative_to(bundle).as_posix()
    helper_owners = {
        "Contents/Resources/dotnet/": "Contents/Resources/dotnet/dotnet",
        "Contents/Resources/Helpers/csharp/": "Contents/Resources/dotnet/dotnet",
        "Contents/Resources/Helpers/go/": "Contents/Resources/Helpers/go/oracle",
        "Contents/Resources/Helpers/python/": "Contents/Resources/Helpers/python/pyrefly",
        "Contents/Resources/Helpers/typescript/": "Contents/Resources/Helpers/typescript/node/bin/node",
    }
    for prefix, owner_relative in helper_owners.items():
        if relative.startswith(prefix):
            owner = (bundle / owner_relative).resolve(strict=True)
            if owner not in root_set:
                fail(f"runtime image {relative} refers to a non-root process owner {owner_relative}")
            return [owner]
    if relative.startswith(("Contents/Frameworks/", "Contents/MacOS/")):
        return sorted(root_set)
    fail(f"Mach-O image has no declared process context in the bundle layout: {relative}")


def inspect_signature(path: Path) -> str:
    completed = subprocess.run(
        ["codesign", "--display", "--verbose=4", str(path)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    output = completed.stdout
    if "code object is not signed at all" in output:
        return "unsigned"
    if "signature=adhoc" in output.lower():
        return "ad-hoc"
    if completed.returncode == 0 and ("Authority=" in output or "TeamIdentifier=" in output):
        return "signed"
    fail(f"cannot classify code-signature state for {path}: {output.strip()}")


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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path, help="clean checkout at the pinned application revision")
    parser.add_argument("--expected-revision", required=True, help="full operator-reviewed application Git commit SHA")
    parser.add_argument("--expected-tree", required=True, help="full operator-reviewed application Git tree SHA")
    parser.add_argument("--expected-runner-sha256", required=True, help="full operator-reviewed Cargo runner SHA-256 from the application build")
    parser.add_argument("--artifact-dir", required=True, type=Path, help="directory containing the three admitted application binaries")
    parser.add_argument("--build-receipt", required=True, type=Path, help="Root-produced application build receipt JSON")
    parser.add_argument("--dotnet-root", required=True, type=Path, help="real macOS .NET runtime installation root")
    parser.add_argument("--roslyn-dir", required=True, type=Path, help="real framework-dependent oracle publish output directory")
    parser.add_argument("--roslyn-receipt", required=True, type=Path, help="Root-produced locked Roslyn publish receipt JSON")
    parser.add_argument("--helpers-dir", required=True, type=Path, help="receipted real Go, Pyrefly, Node, and TypeScript helper payload")
    parser.add_argument("--helpers-receipt", required=True, type=Path, help="Root-produced compiler-helper receipt JSON")
    parser.add_argument("--output-dir", required=True, type=Path, help="new directory for .app, zip, and receipt")
    parser.add_argument("--target", choices=MACOS_TARGETS, default="aarch64-apple-darwin")
    args = parser.parse_args()

    macos_tools()
    target_arch = MACOS_TARGETS[args.target]
    host_arch = platform.machine().lower()
    if host_arch == "aarch64":
        host_arch = "arm64"
    if host_arch != target_arch:
        fail(f"native packaging host is {host_arch}; requested single-architecture target is {target_arch}")

    source_root = args.source_root.resolve(strict=True)
    artifact_dir = args.artifact_dir.resolve(strict=True)
    roslyn_dir = args.roslyn_dir.resolve(strict=True)
    helpers_dir = args.helpers_dir.resolve(strict=True)
    dotnet_root = args.dotnet_root.resolve(strict=True)
    if args.output_dir.exists() or args.output_dir.is_symlink():
        fail(f"output directory already exists; refusing to reuse it: {args.output_dir}")
    output_dir = args.output_dir.resolve(strict=False)
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
    roslyn_receipt, roslyn_files = validate_roslyn_build(
        args.roslyn_receipt.resolve(strict=True), roslyn_dir, source_root, source, args.target
    )
    helpers_receipt, helper_files = validate_helper_payload(
        args.helpers_receipt.resolve(strict=True), helpers_dir, source_root, source, args.target
    )
    dotnet = dotnet_root / "dotnet"
    if not dotnet.is_file() or not os.access(dotnet, os.X_OK):
        fail(f"dotnet host is not executable: {dotnet}")

    plist_path = source_root / "apps/desktop/macos/Info.plist"
    with plist_path.open("rb") as stream:
        plist = plistlib.load(stream)
    if plist.get("CFBundleExecutable") != "Nudox" or plist.get("CFBundlePackageType") != "APPL":
        fail("Info.plist does not describe the expected Nudox application bundle")
    if plist.get("LSMinimumSystemVersion") != "12.0":
        fail("unexpected LSMinimumSystemVersion; update and review the packaging floor deliberately")

    output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nudox-package-", dir=output_dir.parent) as temporary:
        staging = Path(temporary) / "Nudox.app"
        macos = staging / "Contents/MacOS"
        resources = staging / "Contents/Resources"
        macos.mkdir(parents=True)
        resources.mkdir(parents=True)
        shutil.copyfile(plist_path, staging / "Contents/Info.plist")

        for name, source_binary in binaries.items():
            destination_name = "backend-desktop" if name == "backend-desktop" else name
            destination = macos / destination_name
            shutil.copy2(source_binary, destination)
            destination.chmod(0o755)

        launcher = macos / "Nudox"
        launcher.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            'contents=$(CDPATH= cd "$(dirname "$0")/.." && pwd)\n'
            'export NUDOX_DOTNET="${NUDOX_DOTNET-$contents/Resources/dotnet/dotnet}"\n'
            'export NUDOX_ROSLYN_HELPER="${NUDOX_ROSLYN_HELPER-$contents/Resources/Helpers/csharp/oracle.dll}"\n'
            'export NUDOX_GO_ORACLE="${NUDOX_GO_ORACLE-$contents/Resources/Helpers/go/oracle}"\n'
            'export NUDOX_GO_ORACLE_BIN="${NUDOX_GO_ORACLE_BIN-$contents/Resources/Helpers/go/oracle}"\n'
            'export NUDOX_PYREFLY="${NUDOX_PYREFLY-$contents/Resources/Helpers/python/pyrefly}"\n'
            'export NUDOX_TYPESCRIPT_NODE="${NUDOX_TYPESCRIPT_NODE-$contents/Resources/Helpers/typescript/node/bin/node}"\n'
            'export NUDOX_TYPESCRIPT_MODULE_ROOT="${NUDOX_TYPESCRIPT_MODULE_ROOT-$contents/Resources/Helpers/typescript/node_modules}"\n'
            'export NUDOX_TYPESCRIPT_REPORT_PROGRAM="${NUDOX_TYPESCRIPT_REPORT_PROGRAM-$contents/Resources/Helpers/typescript/report-program}"\n'
            'export NUDOX_TSC="${NUDOX_TSC-$contents/Resources/Helpers/typescript/tsc}"\n'
            'exec "$contents/MacOS/backend-desktop" "$@"\n',
            encoding="utf-8",
        )
        launcher.chmod(0o755)

        helper_resources = resources / "Helpers"
        shutil.copytree(helpers_dir, helper_resources, symlinks=False)
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
        runtime_notices = copy_runtime(dotnet_root, resources / "dotnet")
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
        shutil.copy2(args.roslyn_receipt, provenance / "roslyn-build-receipt.json")
        shutil.copy2(args.helpers_receipt, provenance / "compiler-helpers-receipt.json")

        info = run(["plutil", "-lint", str(staging / "Contents/Info.plist")])
        if "OK" not in info:
            fail("Info.plist did not pass plutil validation")
        required_images = {
            "Contents/MacOS/backend-desktop",
            "Contents/MacOS/backend-mcp",
            "Contents/MacOS/backend-locald",
            "Contents/Resources/dotnet/dotnet",
            *(
                f"Contents/Resources/Helpers/{relative}"
                for relative in HELPER_EXECUTABLES.values()
            ),
        }
        macho_records = inspect_macho_tree(
            staging,
            target_arch,
            plist["LSMinimumSystemVersion"],
            required_images,
        )
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
            "roslyn_build": {
                "receipt_sha256": sha256(args.roslyn_receipt),
                "runtime_identifier": roslyn_receipt["runtime_identifier"],
                "framework_dependent": True,
                "commands": roslyn_receipt["commands"],
                "notices": roslyn_receipt["notices"],
                "files": roslyn_files,
            },
            "compiler_helpers": {
                "receipt_sha256": sha256(args.helpers_receipt),
                "files": helper_files,
                "tools": helpers_receipt["tools"],
                "notices": helpers_receipt["notices"],
                "typescript_driver_sha256": sha256(helper_resources / "typescript/checker-main.cjs"),
                "external_language_prerequisites": {
                    "rust": "paired rustc and cargo plus Cargo registry cache for offline package resolution",
                    "clang": "clang plus a compatible libclang selected through LIBCLANG_PATH",
                    "python": "python3; pyrefly is bundled",
                    "go": "go toolchain and Go module cache; the relocatable Go oracle is bundled",
                    "java": "a JDK containing java and javac",
                },
            },
            "dotnet_runtime": {
                "root_path_recorded_as_hashes_only": True,
                "notices": runtime_notices,
            },
            "font_licenses": {name: sha256(licenses / name) for name in FONT_LICENSES},
            "macho_images": macho_records,
            "files": file_inventory(staging),
            "distribution_status": f"app bundle {bundle_signature}; notarization not performed; native QA pending",
        }
        manifest_path = resources / "build-manifest.json"
        manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")

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
        receipt_path.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    print(f"Created {output_dir / 'Nudox.app'}")
    print(f"Created {output_dir / 'Nudox-macOS.zip'}")
    print(f"Created {output_dir / 'Nudox-macOS.receipt.json'}")
    print("This receipt records packaging evidence; it does not claim signing, notarization, or native QA.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except PackageError as error:
        print(f"package refused: {error}", file=sys.stderr)
        sys.exit(2)
