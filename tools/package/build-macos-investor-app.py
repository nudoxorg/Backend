#!/usr/bin/env python3
"""Build the three macOS app executables through an operator-pinned Cargo runner."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


TARGETS = {
    "aarch64-apple-darwin": "arm64",
    "x86_64-apple-darwin": "x86_64",
}
EXECUTABLES = ("backend-desktop", "backend-mcp", "backend-locald")
RUNNER_TOOL_VARIABLES = {"RUSTC", "RUSTDOC", "RUSTC_WRAPPER", "CARGO"}


class BuildError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise BuildError(message)


def run(command: list[str], cwd: Path) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError as error:
        fail(f"cannot run {command[0]}: {error}")
    if result.returncode:
        fail(f"command failed ({result.returncode}): {command!r}\n{result.stdout.strip()}")
    return result.stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def source_manifest(source: Path, revision: str, tree: str) -> dict[str, str]:
    actual_revision = run(["git", "rev-parse", "HEAD"], source)
    actual_tree = run(["git", "rev-parse", "HEAD^{tree}"], source)
    status = run(["git", "status", "--porcelain", "--untracked-files=all"], source)
    if actual_revision != revision or actual_tree != tree or status:
        fail(
            "source must remain at the exact operator-pinned clean commit; "
            f"expected {revision} ({tree}), got {actual_revision} ({actual_tree}); status={status!r}"
        )
    lock = source / "Cargo.lock"
    if not lock.is_file():
        fail("pinned source is missing Cargo.lock")
    return {
        "git_revision": actual_revision,
        "git_tree": actual_tree,
        "cargo_lock_sha256": sha256(lock),
        "working_tree": "clean",
    }


def inspect_runner(runner_argument: Path, expected_sha256: str) -> dict[str, object]:
    if not runner_argument.is_absolute():
        fail("--cargo-runner must be an absolute path to the Root-supplied pinned runner")
    try:
        runner = runner_argument.resolve(strict=True)
    except OSError as error:
        fail(f"cannot resolve the pinned Cargo runner: {error}")
    if not runner.is_file():
        fail(f"pinned Cargo runner is not a regular file: {runner}")
    actual_runner_sha256 = sha256(runner)
    if actual_runner_sha256 != expected_sha256:
        fail("pinned Cargo runner content differs from --expected-runner-sha256")

    assets: dict[str, str] = {"runner": actual_runner_sha256}

    def capture(path_text: str, role: str, *, executable: bool) -> Path:
        candidate = Path(path_text)
        if not candidate.is_absolute():
            fail(f"pinned runner reference {role} must use an absolute path")
        try:
            path = candidate.resolve(strict=True)
        except OSError as error:
            fail(f"pinned runner reference {role} is unavailable: {error}")
        if not path.is_file() or (executable and not os.access(path, os.X_OK)):
            fail(f"pinned runner reference {role} is not a usable file: {path}")
        assets[role] = sha256(path)
        return path

    try:
        with runner.open("rb") as stream:
            header = stream.read(2)
    except OSError as error:
        fail(f"cannot inspect pinned Cargo runner: {error}")
    if header != b"#!":
        fail("pinned Cargo runner must be a shebang script with a directly named absolute interpreter")
    try:
        lines = runner.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        fail(f"cannot read pinned Cargo runner: {error}")
    try:
        interpreter_words = shlex.split(lines[0][2:].strip())
    except (IndexError, ValueError) as error:
        fail(f"invalid shebang in pinned Cargo runner: {error}")
    if not interpreter_words or not Path(interpreter_words[0]).is_absolute():
        fail("pinned Cargo runner must name an absolute shebang interpreter")
    interpreter = capture(interpreter_words[0], "runner.interpreter", executable=True)
    command_prefix = [str(interpreter), *interpreter_words[1:], str(runner)]
    normalized_prefix = ["$CARGO_RUNNER_INTERPRETER", *interpreter_words[1:], "$CARGO_RUNNER"]

    source_index = 0
    for line in lines[1:]:
        try:
            words = shlex.split(line, comments=True)
        except ValueError as error:
            fail(f"cannot parse pinned Cargo runner line: {error}")
        if not words:
            continue
        if words[0] in {"source", "."}:
            if len(words) != 2:
                fail("pinned Cargo runner uses a dynamic source expression")
            capture(words[1], f"runner.environment.{source_index}", executable=False)
            source_index += 1
            continue

        assignments = words[1:] if words[0] == "export" else words[:1]
        for assignment in assignments:
            if "=" not in assignment:
                continue
            name, value = assignment.split("=", 1)
            if name in RUNNER_TOOL_VARIABLES and value.startswith("/"):
                capture(value, f"runner.tool.{name}", executable=True)

        if words[0] == "exec":
            if len(words) < 2 or not Path(words[1]).is_absolute():
                fail("pinned Cargo runner must exec an absolute wrapper path")
            capture(words[1], "runner.exec", executable=True)

    if "runner.interpreter" not in assets:
        fail("pinned Cargo runner must expose its absolute shebang interpreter")
    if not any(role.startswith("runner.environment.") for role in assets):
        fail("pinned Cargo runner must name its saved environment script directly")
    if not {"runner.tool.RUSTC", "runner.tool.RUSTC_WRAPPER", "runner.exec"}.issubset(assets):
        fail("pinned Cargo runner must expose its compiler, Rust wrapper, and Cargo wrapper paths")
    return {
        "sha256": actual_runner_sha256,
        "expected_sha256": expected_sha256,
        "referenced_asset_sha256": dict(sorted(assets.items())),
        "_command_prefix": command_prefix,
        "invocation_prefix": normalized_prefix,
    }


def cargo_provenance(
    provenance_dir: Path,
    previous_records: set[str],
    source_root: Path,
    source: dict[str, str],
    target: str,
    target_dir: Path,
) -> dict[str, object]:
    records = {path.name: path for path in provenance_dir.glob("*.json")}
    new_names = set(records) - previous_records
    if len(new_names) != 1:
        fail(f"expected one new pinned Cargo provenance record, found {len(new_names)}")
    path = records[new_names.pop()]
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read the Cargo wrapper provenance record: {error}")
    if not isinstance(value, dict):
        fail("Cargo wrapper provenance record must be a JSON object")
    toolchain = value.get("toolchain")
    wrapper = value.get("wrapper")
    features = value.get("features")
    if (
        value.get("schema") != 2
        or value.get("workspace_root") != str(source_root)
        or value.get("cargo_target_dir") != str(target_dir)
        or value.get("git_head") != source["git_revision"]
        or value.get("cargo_lock_sha256") != source["cargo_lock_sha256"]
        or value.get("cargo_lock_sha256_after") != source["cargo_lock_sha256"]
        or value.get("source_changed_during_build") is not False
        or value.get("source_dirty_sha256") != value.get("source_dirty_sha256_after")
        or value.get("cargo_exit_status") != 0
        or not isinstance(toolchain, dict)
        or toolchain.get("capture_complete") is not True
        or toolchain.get("changed_during_build") is not False
        or not isinstance(wrapper, dict)
        or not isinstance(features, dict)
        or features.get("targets") != [target]
    ):
        fail("Cargo provenance does not prove an unchanged, successful build from the pinned source/toolchain")
    if not isinstance(value.get("source_dirty_sha256"), str) or not re.fullmatch(
        r"[0-9a-f]{64}", value["source_dirty_sha256"]
    ):
        fail("Cargo provenance has no valid clean-source digest")
    if not isinstance(value.get("run_id"), str) or not value["run_id"]:
        fail("Cargo provenance is missing the unique invocation identity")
    versions = {name: toolchain.get(name) for name in ("cargo", "rustc", "rustdoc")}
    if any(not isinstance(version, str) or not version.strip() for version in versions.values()):
        fail("Cargo provenance is missing a pinned Cargo/Rust tool version")
    if features != {
        "features": [],
        "all_features": False,
        "no_default_features": False,
        "targets": [target],
    }:
        fail("Cargo provenance does not match the requested target-specific release build")
    wrapper_hashes = {
        "runtime": wrapper.get("runtime_sha256"),
        "source": wrapper.get("source_sha256"),
        "rustc": wrapper.get("rustc_sha256"),
    }
    if any(not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest) for digest in wrapper_hashes.values()):
        fail("Cargo provenance is missing a pinned wrapper hash")
    outputs = value.get("outputs")
    if not isinstance(outputs, list) or any(
        not isinstance(item, dict)
        or not isinstance(item.get("path"), str)
        or not isinstance(item.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) is None
        for item in outputs
    ):
        fail("Cargo provenance has an invalid executable-output manifest")
    return {
        "schema": 2,
        "record_sha256": sha256(path),
        "run_id": value.get("run_id"),
        "git_head": value["git_head"],
        "source_dirty_sha256": value["source_dirty_sha256"],
        "source_unchanged": True,
        "cargo_lock_sha256": value["cargo_lock_sha256"],
        "cargo_exit_status": 0,
        "features": features,
        "toolchain": versions,
        "toolchain_capture_complete": True,
        "toolchain_unchanged": True,
        "wrapper_sha256": wrapper_hashes,
        "outputs": outputs,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--expected-revision", required=True, help="full operator-reviewed Git commit SHA")
    parser.add_argument("--expected-tree", required=True, help="full operator-reviewed Git tree SHA")
    parser.add_argument("--cargo-runner", required=True, type=Path, help="absolute path to the Root-supplied pinned Cargo runner")
    parser.add_argument("--expected-runner-sha256", required=True, help="full operator-reviewed Cargo runner SHA-256")
    parser.add_argument("--target", choices=TARGETS, default="aarch64-apple-darwin")
    parser.add_argument("--output-dir", required=True, type=Path, help="new directory for copied binaries and receipt")
    args = parser.parse_args()

    if re.fullmatch(r"[0-9a-f]{40}", args.expected_revision) is None:
        fail("--expected-revision must be a full lowercase Git commit SHA")
    if re.fullmatch(r"[0-9a-f]{40}", args.expected_tree) is None:
        fail("--expected-tree must be a full lowercase Git tree SHA")
    if re.fullmatch(r"[0-9a-f]{64}", args.expected_runner_sha256) is None:
        fail("--expected-runner-sha256 must be a full lowercase SHA-256 digest")
    if platform.system() != "Darwin":
        fail("application build requires a native macOS host")
    host_arch = platform.machine().lower()
    host_arch = "arm64" if host_arch == "aarch64" else host_arch
    if host_arch != TARGETS[args.target]:
        fail(f"native build host is {host_arch}; requested target is {TARGETS[args.target]}")

    source = args.source_root.resolve(strict=True)
    if args.output_dir.exists() or args.output_dir.is_symlink():
        fail(f"output directory already exists; refusing reuse: {args.output_dir}")
    output_dir = args.output_dir.resolve(strict=False)
    if output_dir.exists() or output_dir.is_symlink():
        fail(f"output directory already exists; refusing reuse: {output_dir}")
    try:
        output_dir.relative_to(source)
    except ValueError:
        pass
    else:
        fail("receipt output must be outside the pinned source checkout")

    source_before = source_manifest(source, args.expected_revision, args.expected_tree)
    runner = inspect_runner(args.cargo_runner, args.expected_runner_sha256)
    target_dir = source / ".local/target"
    provenance_dir = target_dir / ".nudox-provenance"
    previous_provenance = {
        path.name for path in provenance_dir.glob("*.json")
    } if provenance_dir.is_dir() else set()
    cargo_arguments = [
        "build",
        "--locked",
        "--manifest-path",
        str(source / "Cargo.toml"),
        "--target-dir",
        str(target_dir),
        "--target",
        args.target,
        "--profile",
        "release",
        "-p",
        "backend-desktop",
        "-p",
        "backend-mcp",
        "-p",
        "backend-locald",
    ]
    command = [*runner["_command_prefix"], *cargo_arguments]
    build_env = os.environ.copy()
    # The admitted Root runner forms its worktree-owned target path from $PWD.
    # subprocess(cwd=...) does not rewrite an inherited PWD value.
    build_env["PWD"] = str(source)
    try:
        completed = subprocess.run(command, cwd=source, env=build_env, check=False)
    except OSError as error:
        fail(f"cannot start the operator-pinned Cargo runner: {error}")
    if completed.returncode:
        fail(f"operator-pinned Cargo build failed with status {completed.returncode}")

    source_after = source_manifest(source, args.expected_revision, args.expected_tree)
    if source_after != source_before:
        fail("source manifest changed during the build; refusing an application receipt")
    runner_after = inspect_runner(args.cargo_runner, args.expected_runner_sha256)
    if runner_after != runner:
        fail("pinned Cargo runner or one of its referenced environment/tool files changed during the build")
    build_provenance = cargo_provenance(
        provenance_dir,
        previous_provenance,
        source,
        source_before,
        args.target,
        target_dir,
    )

    build_dir = target_dir / args.target / "release"
    binaries = {name: build_dir / name for name in EXECUTABLES}
    for name, path in binaries.items():
        if not path.is_file() or not os.access(path, os.X_OK):
            fail(f"build completed without the required executable {name}: {path}")
    cargo_outputs = {
        item["path"]: item["sha256"]
        for item in build_provenance["outputs"]
    }
    for name, path in binaries.items():
        output_name = f"{args.target}/release/{name}"
        if output_name in cargo_outputs and cargo_outputs[output_name] != sha256(path):
            fail(f"Cargo provenance output hash differs from the current {name} artifact")

    output_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="nudox-app-inputs-", dir=output_dir.parent) as temporary:
        staged = Path(temporary) / "inputs"
        artifacts = staged / "artifacts"
        artifacts.mkdir(parents=True)
        for name, binary in binaries.items():
            shutil.copy2(binary, artifacts / name)
        receipt = {
            "schema": 1,
            "source": source_before,
            "source_before": source_before,
            "source_after": source_after,
            "source_unchanged": True,
            "target": args.target,
            "profile": "release",
            "locked_build": True,
            "target_cache_policy": "worktree-owned .local/target; the operator-pinned Cargo wrapper decides safe reuse",
            "cargo_runner_before": {
                key: value for key, value in runner.items() if not key.startswith("_")
            },
            "cargo_runner_after": {
                key: value for key, value in runner_after.items() if not key.startswith("_")
            },
            "cargo_runner_unchanged": True,
            "cargo_provenance": build_provenance,
            "command": [
                *runner["invocation_prefix"],
                "build",
                "--locked",
                "--manifest-path",
                "$SOURCE_ROOT/Cargo.toml",
                "--target-dir",
                "$SOURCE_ROOT/.local/target",
                "--target",
                args.target,
                "--profile",
                "release",
                "-p",
                "backend-desktop",
                "-p",
                "backend-mcp",
                "-p",
                "backend-locald",
            ],
            "executables": {
                name: sha256(artifacts / name)
                for name in EXECUTABLES
            },
        }
        (staged / "application-build-receipt.json").write_text(
            json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        output_dir.mkdir()
        shutil.copytree(staged / "artifacts", output_dir / "artifacts")
        shutil.copy2(staged / "application-build-receipt.json", output_dir / "application-build-receipt.json")

    print(f"Created {output_dir / 'artifacts'}")
    print(f"Created {output_dir / 'application-build-receipt.json'}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BuildError as error:
        print(f"build refused: {error}", file=sys.stderr)
        sys.exit(2)
