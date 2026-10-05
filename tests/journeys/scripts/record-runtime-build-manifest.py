#!/usr/bin/env python3
"""Capture or adapt a verified Root Cargo build receipt for journey acceptance."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from runtime_build_receipt import (
    EXECUTABLE_NAMES,
    MANIFEST_SCHEMA,
    RECEIPT_SCHEMA,
    ReceiptError,
    _is_build_command,
    canonical_json,
    sha256_bytes,
    source_snapshot,
    stable_file,
    tool_version,
    execution_layout,
    verify_architecture_parser_fixtures,
    verify_runtime_build_receipt,
)


def fail(message: str) -> None:
    raise ReceiptError(message)


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def output_path(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.exists() or path.is_symlink():
        fail(f"{label} must be a new absolute path")
    if not path.parent.is_dir() or path.parent.is_symlink():
        fail(f"{label} parent must be an existing non-symlink directory")
    return path


def write_new_json(path: Path, value: Any) -> bytes:
    encoded = canonical_json(value) + b"\n"
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(encoded)
        stream.flush()
        os.fsync(stream.fileno())
    parent_fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(parent_fd)
    finally:
        os.close(parent_fd)
    return encoded


def parse_artifacts(values: list[str]) -> dict[str, Path]:
    artifacts: dict[str, Path] = {}
    for value in values:
        name, separator, raw_path = value.partition("=")
        if not separator or name not in EXECUTABLE_NAMES or name in artifacts:
            fail("each artifact must be one unique backend-locald/backend-cli/backend-mcp path")
        path = Path(raw_path)
        if not path.is_absolute():
            fail("artifact paths must be absolute")
        artifacts[name] = path
    if set(artifacts) != set(EXECUTABLE_NAMES):
        fail("exactly one locald, CLI, and MCP artifact path is required")
    return artifacts


def terminate_owned_group(process: subprocess.Popen[bytes]) -> bool:
    """Retire descendants of the exact Cargo invocation session."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return True
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
        time.sleep(0.025)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        return True
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return True
        except PermissionError:
            return False
        time.sleep(0.025)
    return False


def execute_build(command: list[str], source: Path) -> tuple[int, bool]:
    try:
        process = subprocess.Popen(
            command,
            cwd=source,
            stdin=subprocess.DEVNULL,
            close_fds=True,
            start_new_session=True,
        )
    except OSError as error:
        fail(f"the exact Cargo build command could not start ({type(error).__name__})")
    caught_signal: int | None = None
    previous_handlers: dict[int, Any] = {}

    def interrupt(signum: int, _frame: Any) -> None:
        nonlocal caught_signal
        caught_signal = signum
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass

    try:
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            previous_handlers[signum] = signal.signal(signum, interrupt)
        while True:
            if caught_signal is not None:
                clean_group = terminate_owned_group(process)
                process.wait()
                if not clean_group:
                    return 125, False
                return 128 + caught_signal, True
            try:
                return_code = process.wait(timeout=0.1)
                break
            except subprocess.TimeoutExpired:
                continue
    except BaseException:
        terminate_owned_group(process)
        process.wait()
        raise
    finally:
        for signum, handler in previous_handlers.items():
            signal.signal(signum, handler)
    clean_group = terminate_owned_group(process)
    if caught_signal is not None:
        return 128 + caught_signal, clean_group
    if return_code < 0:
        return 128 + (-return_code), clean_group
    return return_code if clean_group else 125, clean_group


def capture(args: argparse.Namespace) -> int:
    verify_architecture_parser_fixtures()
    source = source_snapshot(args.source_checkout)
    if not source["clean"]:
        fail("refusing to start Cargo because the source checkout is not clean")
    runner_before = stable_file(args.runner, "pinned Cargo runner")
    if not os.access(runner_before["path"], os.X_OK):
        fail("pinned Cargo runner is not executable")
    cargo_before = stable_file(args.cargo, "Cargo", executable=True)
    rustc_before = stable_file(args.rustc, "rustc", executable=True)
    cargo_version_before = tool_version(args.cargo, "Cargo")
    rustc_version_before = tool_version(args.rustc, "rustc")
    command = list(args.command)
    if command and command[0] == "--":
        command.pop(0)
    layout = execution_layout(command, runner_before["path"])
    if layout is None or not _is_build_command(
        command, cargo_before["path"], runner_before["path"]
    ):
        fail("command must be a locked Cargo build of all three runtime executables")
    _, interpreter_path = layout
    interpreter_before = (
        stable_file(Path(interpreter_path), "pinned runner interpreter", executable=True)
        if interpreter_path is not None
        else None
    )

    receipt_path = output_path(args.receipt_output, "Root Cargo build receipt")
    manifest_path = output_path(args.manifest_output, "runtime build manifest") if args.manifest_output else None
    artifact_paths = parse_artifacts(args.artifact)
    output_paths = [receipt_path, *([manifest_path] if manifest_path is not None else [])]
    resolved_outputs = [path.resolve(strict=False) for path in output_paths]
    if len(set(resolved_outputs)) != len(resolved_outputs):
        fail("receipt and manifest outputs must be distinct paths")
    resolved_artifacts = [path.resolve(strict=False) for path in artifact_paths.values()]
    if len(set(resolved_artifacts)) != len(resolved_artifacts):
        fail("runtime artifact paths must be distinct")
    if set(resolved_outputs) & set(resolved_artifacts):
        fail("receipt outputs must not collide with runtime executable paths")
    if any(path.is_symlink() for path in artifact_paths.values()):
        fail("runtime artifact paths must not be final symlinks")
    if any(not path.parent.is_dir() for path in artifact_paths.values()):
        fail("runtime artifact parent directories must already exist")
    source_path = source["path"].resolve(strict=True)
    if any(path == source_path or source_path in path.parents for path in resolved_outputs):
        fail("receipt and manifest outputs must be outside the source checkout")
    started = utc_now()
    exit_status, clean_group = execute_build(command, source["path"])
    finished = utc_now()
    source_after = source_snapshot(source["path"])
    runner_after = stable_file(args.runner, "pinned Cargo runner")
    if not os.access(runner_after["path"], os.X_OK):
        fail("pinned Cargo runner stopped being executable")
    cargo_version_after = tool_version(args.cargo, "Cargo")
    rustc_version_after = tool_version(args.rustc, "rustc")
    cargo_after = stable_file(args.cargo, "Cargo", executable=True)
    rustc_after = stable_file(args.rustc, "rustc", executable=True)
    interpreter_after = (
        stable_file(Path(interpreter_path), "pinned runner interpreter", executable=True)
        if interpreter_path is not None
        else None
    )
    runner_unchanged = (
        runner_before["path"] == runner_after["path"]
        and runner_before["sha256"] == runner_after["sha256"]
        and (
            interpreter_before is None
            or (
                interpreter_after is not None
                and interpreter_before["path"] == interpreter_after["path"]
                and interpreter_before["sha256"] == interpreter_after["sha256"]
            )
        )
    )
    toolchain_unchanged = (
        cargo_before["path"] == cargo_after["path"]
        and cargo_before["sha256"] == cargo_after["sha256"]
        and rustc_before["path"] == rustc_after["path"]
        and rustc_before["sha256"] == rustc_after["sha256"]
        and cargo_version_before == cargo_version_after
        and rustc_version_before == rustc_version_after
        and runner_unchanged
    )

    artifacts: dict[str, dict[str, Any]] = {}
    if exit_status == 0 and clean_group:
        for name, path in artifact_paths.items():
            try:
                identity = stable_file(path, name, executable=True)
            except ReceiptError:
                continue
            artifacts[name] = {
                "path": identity["path"],
                "sha256": identity["sha256"],
                "bytes": identity["bytes"],
                "architecture": identity["architecture"],
            }

    receipt = {
        "schema": RECEIPT_SCHEMA,
        "source": {
            "commit": source["commit"],
            "tree": source["tree"],
            "clean_before": source["clean"],
            "clean_after": (
                source_after["clean"]
                and source_after["commit"] == source["commit"]
                and source_after["tree"] == source["tree"]
                and source_after["cargo_lock_sha256"] == source["cargo_lock_sha256"]
            ),
            "cargo_lock_sha256": source["cargo_lock_sha256"],
        },
        "command": command,
        "runner": {
            "path": runner_before["path"],
            "sha256": runner_before["sha256"],
            **(
                {
                    "interpreter": {
                        "path": interpreter_before["path"],
                        "sha256": interpreter_before["sha256"],
                    }
                }
                if interpreter_before is not None
                else {}
            ),
        },
        "toolchain": {
            "cargo": {"path": cargo_before["path"], "version": cargo_version_before},
            "rustc": {"path": rustc_before["path"], "version": rustc_version_before},
            "unchanged": toolchain_unchanged,
        },
        "started_utc": started,
        "finished_utc": finished,
        "exit": exit_status,
        "check_only": False,
        "artifacts": artifacts,
    }
    write_new_json(receipt_path, receipt)
    print(f"Root receipt: {receipt_path}")
    if exit_status != 0:
        return exit_status
    if not receipt["source"]["clean_after"] or not toolchain_unchanged:
        print("BLOCKED: source or toolchain changed during Cargo", file=sys.stderr)
        return 3
    if set(artifacts) != set(EXECUTABLE_NAMES):
        print("BLOCKED: Cargo completed without all three exact executable artifacts", file=sys.stderr)
        return 3
    if runner_after["path"] != runner_before["path"] or runner_after["sha256"] != runner_before["sha256"]:
        print("BLOCKED: pinned Cargo runner changed during Cargo", file=sys.stderr)
        return 3
    if manifest_path is not None:
        manifest = adapt(receipt_path, source["path"])
        write_new_json(manifest_path, manifest)
        print(f"Runtime manifest: {manifest_path}")
    return 0


def adapt(receipt_path: Path, source_checkout: Path) -> dict[str, Any]:
    verified = verify_runtime_build_receipt(receipt_path, source_checkout)
    return {
        "schema": MANIFEST_SCHEMA,
        "source": {
            "commit": verified["source"]["commit"],
            "tree": verified["source"]["tree"],
            "clean": True,
        },
        "executables": {
            name: {
                "path": artifact["path"],
                "sha256": artifact["sha256"],
                "bytes": artifact["bytes"],
            }
            for name, artifact in verified["artifacts"].items()
        },
        "root_receipt": {
            "schema": RECEIPT_SCHEMA,
            "path": verified["receipt_path"],
            "sha256": verified["receipt_sha256"],
        },
    }


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="mode", required=True)
    capture_parser = subparsers.add_parser(
        "capture", help="run one exact cargo build and record its source/tool/output evidence"
    )
    capture_parser.add_argument("--source-checkout", type=Path, required=True)
    capture_parser.add_argument("--runner", type=Path, required=True)
    capture_parser.add_argument("--cargo", type=Path, required=True)
    capture_parser.add_argument("--rustc", type=Path, required=True)
    capture_parser.add_argument("--artifact", action="append", default=[])
    capture_parser.add_argument("--receipt-output", type=Path, required=True)
    capture_parser.add_argument("--manifest-output", type=Path)
    capture_parser.add_argument("command", nargs=argparse.REMAINDER)
    adapt_parser = subparsers.add_parser(
        "adapt", help="verify an existing Root receipt and emit runtime-build-manifest.v1"
    )
    adapt_parser.add_argument("--source-checkout", type=Path, required=True)
    adapt_parser.add_argument("--receipt", type=Path, required=True)
    adapt_parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_arguments()
    try:
        if args.mode == "capture":
            if not args.command:
                fail("capture requires `-- cargo build ...`")
            return capture(args)
        verify_architecture_parser_fixtures()
        manifest_path = output_path(args.output, "runtime build manifest")
        manifest = adapt(args.receipt, args.source_checkout)
        write_new_json(manifest_path, manifest)
        print(f"Runtime manifest: {manifest_path}")
        return 0
    except ReceiptError as error:
        print(f"BLOCKED: {error}", file=sys.stderr)
        return 3
    except OSError:
        print("FAIL: required receipt filesystem operation failed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
