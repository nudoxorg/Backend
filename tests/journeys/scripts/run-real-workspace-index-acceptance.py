#!/usr/bin/env python3
"""Verify real-workspace indexing through the durable CLI and MCP surfaces.

This runner is deliberately a protocol acceptance tool, not a source-file
counter or a compiler benchmark. It requires a clean source checkout, exact
build-manifest entries for all three binaries, a closed compiler-host
snapshot, and an operator-supplied manifest of real local projects.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import platform
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
import threading
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO

from runtime_build_receipt import (
    ReceiptError,
    architecture_from_binary_header,
    normalize_arch,
    verify_architecture_parser_fixtures,
    verify_runtime_build_receipt,
)
from registry_artifact_origin import OriginError, archive_members, verify_registry_metadata


MANIFEST_SCHEMA = "nudox.real-workspace-index-acceptance-manifest.v1"
RESULT_SCHEMA = "nudox.real-workspace-index-acceptance-result.v1"
BUILD_SCHEMA = "nudox.runtime-build-manifest.v1"
MAX_BUILD_MANIFEST_BYTES = 1024 * 1024
MAX_CORPUS_MANIFEST_BYTES = 1024 * 1024
MAX_COMPILER_SNAPSHOT_BYTES = 30 * 1024
MAX_RUNTIME_POLICY_BYTES = 512
MAX_PROJECTS = 32
MAX_SYMBOLS = 2_000
MAX_SOURCE_CANDIDATES = 150_000
MAX_SOURCE_FILE_BYTES = 32 * 1024 * 1024
MAX_SOURCE_BYTES_TOTAL = 4 * 1024 * 1024 * 1024
MAX_CLIENT_STDOUT_BYTES = 2 * 1024 * 1024
MAX_CLIENT_STDERR_BYTES = 1024 * 1024
MAX_CLIENT_EVIDENCE_ITEMS = 20_000
MAX_CLIENT_EVIDENCE_BYTES = 64 * 1024 * 1024
MAX_CLIENT_CAPTURE_PREFIX_BYTES = 1024
MAX_CLIENT_CAPTURE_TAIL_BYTES = 1024
MAX_CAPTURE_PREFIX_BYTES = 4096
MAX_CAPTURE_TAIL_BYTES = 4096
MAX_SEARCH_RESULTS = 200
DEFAULT_DEADLINE_SECONDS = 5_400
MAX_DEADLINE_SECONDS = 21_600
POLL_SECONDS = 1.0
OWNER_STOP_GRACE_SECONDS = 8.0
LANGUAGE_CORPUS_PROFILES = {
    "typescript": frozenset({"typescript", "tsx", "javascript"}),
    "python": frozenset({"python"}),
    "go": frozenset({"go"}),
}

IGNORED_DIRECTORIES = frozenset(
    {
        ".git",
        ".backend",
        "target",
        "bin",
        "obj",
        "out",
        ".gradle",
        "node_modules",
        "dist",
        "build",
        ".angular",
        ".next",
        ".nuxt",
        ".svelte-kit",
        ".turbo",
        ".cache",
        ".vite",
        ".parcel-cache",
        ".webpack",
        ".rollup.cache",
        ".nx",
        "coverage",
        "storybook-static",
        ".nyc_output",
        "Library",
        ".venv",
        "venv",
        "__pycache__",
        ".mypy_cache",
        ".pytest_cache",
        ".tox",
        ".ruff_cache",
        ".idea",
        ".vscode",
        ".vs",
        ".yarn",
        ".pnpm-store",
        "bower_components",
        "jspm_packages",
        "vendor",
    }
)


def ignored_source_directory(parts: tuple[str, ...]) -> bool:
    """Keep generated roots excluded without dropping the Python src/build package."""
    for position, name in enumerate(parts):
        if name in IGNORED_DIRECTORIES:
            if name == "build" and position == 1 and parts[0] == "src":
                continue
            return True
    return False


SOURCE_EXTENSIONS = frozenset(
    {
        ".rs",
        ".ts",
        ".tsx",
        ".mts",
        ".cts",
        ".js",
        ".jsx",
        ".mjs",
        ".cjs",
        ".py",
        ".pyi",
        ".pyw",
        ".go",
        ".java",
        ".cs",
        ".c",
        ".h",
        ".cc",
        ".cpp",
        ".cxx",
        ".hh",
        ".hpp",
        ".hxx",
        ".c++",
        ".h++",
        ".ipp",
    }
)
PROFILE_LANGUAGE_VARIANT = {
    "rust": ("Rust", "rust"),
    "csharp": ("CSharp", "csharp"),
    "java": ("Java", "java"),
    "javascript": ("TypeScript", "typescript"),
    "typescript": ("TypeScript", "typescript"),
    "tsx": ("TypeScript", "typescript"),
    "python": ("Python", "python"),
    "go": ("Go", "go"),
    "c": ("C", "c"),
    "cpp": ("Cxx", "cpp"),
}
PROFILE_EXTENSIONS = {
    "rust": frozenset({".rs"}),
    "csharp": frozenset({".cs"}),
    "java": frozenset({".java"}),
    "javascript": frozenset({".js", ".jsx", ".mjs", ".cjs"}),
    "typescript": frozenset({".ts", ".mts", ".cts"}),
    "tsx": frozenset({".tsx"}),
    "python": frozenset({".py", ".pyi", ".pyw"}),
    "go": frozenset({".go"}),
    "c": frozenset({".c", ".h"}),
    "cpp": frozenset(
        {".cc", ".cpp", ".cxx", ".c++", ".hh", ".hpp", ".hxx", ".h++", ".ipp"}
    ),
}
PROFILE_REQUIRED_ROLES = {
    "rust": ("NUDOX_RUSTC", "NUDOX_CARGO", "NUDOX_CARGO_HOME"),
    "csharp": ("NUDOX_DOTNET", "NUDOX_ROSLYN_HELPER"),
    "java": ("NUDOX_JAVAC", "NUDOX_JDK"),
    "javascript": (),
    "typescript": (),
    "tsx": (),
    "python": ("NUDOX_PYTHON", "NUDOX_PYREFLY"),
    "go": ("NUDOX_GO", "NUDOX_GO_ROOT"),
    "c": ("NUDOX_CLANG", "LIBCLANG_PATH"),
    "cpp": ("NUDOX_CLANG", "LIBCLANG_PATH"),
}
PROFILE_DIRECTORY_ROLES = frozenset(
    {
        "NUDOX_CARGO_HOME",
        "NUDOX_JDK",
        "NUDOX_TYPESCRIPT_MODULE_ROOT",
        "NUDOX_GO_ROOT",
        "NUDOX_CARGO_ROOT",
        "NUDOX_NPM_ROOT",
        "NUDOX_PYPI_ROOT",
        "NUDOX_MAVEN_ROOT",
        "NUDOX_NUGET_ROOT",
        "NUDOX_GENERIC_ROOT",
    }
)
PROFILE_EXECUTABLE_ROLES = frozenset(
    {
        "NUDOX_RUSTC",
        "NUDOX_CARGO",
        "NUDOX_DOTNET",
        "NUDOX_JAVAC",
        "NUDOX_PYTHON",
        "NUDOX_PYREFLY",
        "NUDOX_GO",
        "NUDOX_CLANG",
        "NUDOX_TSC",
        "NUDOX_TYPESCRIPT_NODE",
        "NUDOX_TYPESCRIPT_REPORT_PROGRAM",
    }
)


class AcceptanceError(Exception):
    """A sanitized acceptance failure suitable for the result artifact."""

    status = "FAIL"


class Blocked(AcceptanceError):
    """A required real input or execution capability is not available."""

    status = "BLOCKED"


class Deadline:
    def __init__(self, seconds: int) -> None:
        self.started = time.monotonic()
        self.end = self.started + seconds
        self.seconds = seconds

    def remaining(self) -> float:
        return self.end - time.monotonic()

    def check(self, activity: str) -> None:
        if self.remaining() <= 0:
            raise AcceptanceError(f"absolute suite deadline expired during {activity}")


@dataclass(frozen=True)
class ProjectCase:
    project_id: str
    path: Path
    large: bool
    min_candidates: int
    symbols: tuple[dict[str, Any], ...]
    package: dict[str, Any] | None = None
    acquisition: dict[str, Any] | None = None


@dataclass(frozen=True)
class BinaryIdentity:
    name: str
    path: Path
    sha256: str
    size: int
    architecture: str
    device: int
    inode: int


@dataclass(frozen=True)
class Census:
    sha256: str
    files: int
    bytes: int
    extension_counts: dict[str, int]
    root_device: int
    root_inode: int


class ClientEvidence(list[dict[str, Any]]):
    """Bound both the number and serialized size of retained client evidence."""

    def __init__(self) -> None:
        super().__init__()
        self.retained_bytes = 0


class BoundedCapture:
    """Continuously drain one owner log while retaining bounded diagnostics."""

    def __init__(self) -> None:
        self.total = 0
        self.digest = hashlib.sha256()
        self.prefix = bytearray()
        self.tail = bytearray()
        self.read_error: str | None = None

    def drain(self, stream: BinaryIO) -> None:
        try:
            while block := stream.read(65_536):
                self.total += len(block)
                self.digest.update(block)
                if len(self.prefix) < MAX_CAPTURE_PREFIX_BYTES:
                    take = MAX_CAPTURE_PREFIX_BYTES - len(self.prefix)
                    self.prefix.extend(block[:take])
                self.tail.extend(block)
                if len(self.tail) > MAX_CAPTURE_TAIL_BYTES:
                    del self.tail[: len(self.tail) - MAX_CAPTURE_TAIL_BYTES]
        except OSError as error:
            self.read_error = type(error).__name__

    def evidence(self) -> dict[str, Any]:
        return {
            "bytes": self.total,
            "sha256": self.digest.hexdigest(),
            "truncated": self.total > MAX_CAPTURE_PREFIX_BYTES,
            "prefix_base64": base64.b64encode(self.prefix).decode("ascii"),
            "tail_base64": base64.b64encode(self.tail).decode("ascii"),
            "read_error": self.read_error,
        }


class Owner:
    def __init__(
        self,
        binary: BinaryIdentity,
        arguments: list[str],
        environment: dict[str, str],
        deadline: Deadline,
        label: str,
    ) -> None:
        self.binary = binary
        self.arguments = arguments
        self.environment = environment
        self.deadline = deadline
        self.label = label
        self.process: subprocess.Popen[bytes] | None = None
        self.stdout_capture = BoundedCapture()
        self.stderr_capture = BoundedCapture()
        self.log_threads: list[threading.Thread] = []
        self.log_evidence: dict[str, Any] | None = None

    def start(self) -> None:
        if self.process is not None:
            raise AcceptanceError("owner start was requested twice")
        self.deadline.check("owner start")
        try:
            self.process = subprocess.Popen(
                [str(self.binary.path), *self.arguments],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=self.environment,
                start_new_session=True,
                close_fds=True,
            )
        except OSError as error:
            raise Blocked("the verified locald binary could not be started") from error
        assert self.process.stdout is not None and self.process.stderr is not None
        self.log_threads = [
            threading.Thread(
                target=self.stdout_capture.drain,
                args=(self.process.stdout,),
                daemon=True,
                name="locald-stdout-drain",
            ),
            threading.Thread(
                target=self.stderr_capture.drain,
                args=(self.process.stderr,),
                daemon=True,
                name="locald-stderr-drain",
            ),
        ]
        for thread in self.log_threads:
            thread.start()
        self.require_running("immediately after start")

    def require_running(self, point: str) -> None:
        self.deadline.check(f"owner check {point}")
        if self.process is None or self.process.poll() is not None:
            raise AcceptanceError(f"the owned locald process exited {point}")

    def stop(self) -> None:
        process = self.process
        self.process = None
        if process is None:
            return
        process.poll()
        try:
            # Each owner starts in a fresh session. Signalling its exact process
            # group also retires compiler children that outlive locald, without
            # enumerating or killing processes by name.
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        until = time.monotonic() + OWNER_STOP_GRACE_SECONDS
        while time.monotonic() < until:
            process.poll()
            if not process_group_exists(process.pid):
                break
            time.sleep(0.05)
        if process_group_exists(process.pid):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        try:
            process.wait(timeout=OWNER_STOP_GRACE_SECONDS)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        for thread in self.log_threads:
            thread.join(timeout=OWNER_STOP_GRACE_SECONDS)
        self.log_evidence = {
            "run": self.label,
            "binary_sha256": self.binary.sha256,
            "exit_code": process.returncode,
            "stdout": self.stdout_capture.evidence(),
            "stderr": self.stderr_capture.evidence(),
            "readers_complete": all(not thread.is_alive() for thread in self.log_threads),
        }
        if not self.log_evidence["readers_complete"]:
            raise AcceptanceError("owned locald log streams did not close after process-group cleanup")
        if process.stdout is not None and not process.stdout.closed:
            process.stdout.close()
        if process.stderr is not None and not process.stderr.closed:
            process.stderr.close()


def stop_owner_and_record(owner: Owner, result: dict[str, Any], output: Path) -> None:
    try:
        owner.stop()
    finally:
        if owner.log_evidence is not None:
            result.setdefault("owner_runs", []).append(owner.log_evidence)
            write_json_atomic(output / "run.json", result)


def process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def sha256_bytes(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def read_bounded_regular(path: Path, maximum: int, label: str) -> bytes:
    try:
        before = path.lstat()
    except OSError as error:
        raise Blocked(f"required {label} is unavailable") from error
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1:
        raise Blocked(f"{label} must be a single-link regular file")
    if before.st_size > maximum:
        raise Blocked(f"{label} exceeds its {maximum}-byte input bound")
    try:
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_CLOEXEC", 0)
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as stream:
            opened = os.fstat(stream.fileno())
            if not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1 or (
                before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns
            ) != (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns):
                raise AcceptanceError(f"{label} changed before its descriptor was admitted")
            payload = stream.read(maximum + 1)
            descriptor_after = os.fstat(stream.fileno())
        after = path.lstat()
    except OSError as error:
        raise Blocked(f"{label} could not be read") from error
    if (
        len(payload) != before.st_size
        or len(payload) > maximum
        or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or not stat.S_ISREG(after.st_mode)
        or after.st_nlink != 1
        or (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns, opened.st_nlink)
        != (descriptor_after.st_dev, descriptor_after.st_ino, descriptor_after.st_size,
            descriptor_after.st_mtime_ns, descriptor_after.st_nlink)
    ):
        raise AcceptanceError(f"{label} changed while it was read")
    return payload


def json_no_duplicate_keys(payload: bytes, label: str) -> Any:
    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in items:
            if key in result:
                raise ValueError("duplicate key")
            result[key] = value
        return result

    try:
        return json.loads(payload, object_pairs_hook=pairs)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise Blocked(f"{label} is not valid duplicate-free JSON") from error


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")


def bounded_client_payload_evidence(payload: bytes) -> dict[str, Any]:
    prefix = payload[:MAX_CLIENT_CAPTURE_PREFIX_BYTES]
    tail = (
        payload[-MAX_CLIENT_CAPTURE_TAIL_BYTES:]
        if len(payload) > len(prefix)
        else b""
    )
    return {
        "bytes": len(payload),
        "sha256": sha256_bytes(payload),
        "truncated": len(payload) > len(prefix),
        "prefix_base64": base64.b64encode(prefix).decode("ascii"),
        "tail_base64": base64.b64encode(tail).decode("ascii"),
    }


def append_client_evidence(evidence: list[dict[str, Any]], item: dict[str, Any]) -> None:
    if len(evidence) >= MAX_CLIENT_EVIDENCE_ITEMS:
        raise Blocked(
            f"client-call evidence exceeded its {MAX_CLIENT_EVIDENCE_ITEMS}-call bound"
        )
    encoded_bytes = len(canonical_json(item))
    retained_bytes = getattr(evidence, "retained_bytes", None)
    if not isinstance(retained_bytes, int):
        raise AcceptanceError("client-call evidence collector is not bounded")
    if retained_bytes + encoded_bytes > MAX_CLIENT_EVIDENCE_BYTES:
        raise Blocked(
            "client-call evidence exceeded its "
            f"{MAX_CLIENT_EVIDENCE_BYTES}-byte retained-output bound"
        )
    evidence.append(item)
    evidence.retained_bytes = retained_bytes + encoded_bytes


def require_owner_spawn_disabled(environment: dict[str, str], label: str) -> None:
    selected = environment.get("BACKEND_LOCALD_BIN")
    if not selected or not Path(selected).is_absolute():
        raise Blocked(f"{label} lacks the closed no-spawn locald selection")
    disabled = Path(selected)
    if disabled.exists() or disabled.is_symlink():
        raise Blocked(f"{label} no-spawn locald path unexpectedly became available")


def run_bounded_process(
    arguments: list[str],
    environment: dict[str, str],
    stdin_bytes: bytes | None,
    deadline: Deadline,
    label: str,
) -> tuple[bytes, bytes, int, float]:
    deadline.check(label)
    started = time.monotonic()
    try:
        process = subprocess.Popen(
            arguments,
            stdin=subprocess.PIPE if stdin_bytes is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            start_new_session=True,
            close_fds=True,
        )
    except OSError as error:
        raise Blocked(f"{label} could not be started") from error

    stdout = bytearray()
    stderr = bytearray()
    selector: selectors.BaseSelector | None = None
    streams: dict[int, tuple[BinaryIO, bytearray, int, str]] = {}
    input_offset = 0
    input_stream = process.stdin
    completed = False
    try:
        if process.stdout is None or process.stderr is None:
            raise AcceptanceError(f"{label} did not expose bounded output pipes")
        selector = selectors.DefaultSelector()
        streams = {
            process.stdout.fileno(): (
                process.stdout,
                stdout,
                MAX_CLIENT_STDOUT_BYTES,
                "stdout",
            ),
            process.stderr.fileno(): (
                process.stderr,
                stderr,
                MAX_CLIENT_STDERR_BYTES,
                "stderr",
            ),
        }
        for descriptor, (stream, _, _, _) in streams.items():
            os.set_blocking(descriptor, False)
            selector.register(stream, selectors.EVENT_READ, ("read", descriptor))
        if input_stream is not None and stdin_bytes:
            os.set_blocking(input_stream.fileno(), False)
            selector.register(
                input_stream,
                selectors.EVENT_WRITE,
                ("write", input_stream.fileno()),
            )
        elif input_stream is not None:
            input_stream.close()
            input_stream = None
        while selector.get_map() or process.poll() is None:
            remaining = deadline.remaining()
            if remaining <= 0:
                raise AcceptanceError(f"absolute suite deadline expired during {label}")
            for event, _ in selector.select(min(0.1, remaining)):
                direction, descriptor = event.data
                if direction == "write":
                    assert input_stream is not None and stdin_bytes is not None
                    try:
                        written = os.write(
                            descriptor,
                            stdin_bytes[input_offset : input_offset + 65_536],
                        )
                    except BlockingIOError:
                        continue
                    except BrokenPipeError as error:
                        raise AcceptanceError(
                            f"{label} closed its input before the request completed"
                        ) from error
                    if written <= 0:
                        raise AcceptanceError(f"{label} made no progress consuming its input")
                    input_offset += written
                    if input_offset == len(stdin_bytes):
                        selector.unregister(input_stream)
                        input_stream.close()
                        input_stream = None
                    continue

                stream, sink, cap, name = streams[descriptor]
                try:
                    block = os.read(descriptor, 65_536)
                except BlockingIOError:
                    continue
                if not block:
                    selector.unregister(stream)
                    stream.close()
                    continue
                if len(sink) + len(block) > cap:
                    raise AcceptanceError(f"{label} exceeded its {name} output bound")
                sink.extend(block)
        return_code = process.wait()
        # Retire any child that escaped the protocol process while preserving
        # an open descriptor. This is scoped to this invocation's new session.
        stop_process_group(process)
        completed = True
    finally:
        if selector is not None:
            selector.close()
        for stream in (process.stdout, process.stderr):
            if stream is not None and not stream.closed:
                stream.close()
        if input_stream is not None and not input_stream.closed:
            input_stream.close()
        if not completed:
            stop_process_group(process)
    elapsed = time.monotonic() - started
    return bytes(stdout), bytes(stderr), return_code, elapsed


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    signal_denied = False
    if process_group_exists(process.pid):
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except PermissionError:
            signal_denied = True
        until = time.monotonic() + 0.5
        while time.monotonic() < until and process_group_exists(process.pid):
            time.sleep(0.01)
        if process_group_exists(process.pid):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                signal_denied = True
    if process.poll() is None:
        if signal_denied:
            process.kill()
        else:
            try:
                process.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                process.kill()
    process.wait()
    if signal_denied and process_group_exists(process.pid):
        raise AcceptanceError("could not signal every member of the owned client process group")
    until = time.monotonic() + OWNER_STOP_GRACE_SECONDS
    while time.monotonic() < until and process_group_exists(process.pid):
        time.sleep(0.025)
    if process_group_exists(process.pid):
        raise AcceptanceError("an owned client process group survived SIGKILL cleanup")


def parse_binary_architecture(path: Path) -> str:
    try:
        with path.open("rb") as stream:
            header = stream.read(4096)
        file_size = path.stat().st_size
    except OSError as error:
        raise Blocked("a verified executable could not be inspected") from error
    try:
        return architecture_from_binary_header(
            header, file_size, normalize_arch(platform.machine())
        )
    except ReceiptError as error:
        raise Blocked(str(error)) from error


def executable_identity(name: str, input_path: Path) -> BinaryIdentity:
    try:
        if input_path.is_symlink():
            raise Blocked(f"{name} must be supplied as its final regular path")
        path = input_path.resolve(strict=True)
        before = path.lstat()
    except OSError as error:
        raise Blocked(f"required executable {name} is unavailable") from error
    if not stat.S_ISREG(before.st_mode) or not os.access(path, os.X_OK):
        raise Blocked(f"required executable {name} is not a runnable regular file")
    digest = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            while block := stream.read(1024 * 1024):
                digest.update(block)
        after = path.lstat()
    except OSError as error:
        raise Blocked(f"required executable {name} could not be hashed") from error
    if (
        (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_nlink)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_nlink)
        or not stat.S_ISREG(after.st_mode)
    ):
        raise AcceptanceError(f"required executable {name} changed during provenance capture")
    architecture = parse_binary_architecture(path)
    return BinaryIdentity(
        name,
        path,
        digest.hexdigest(),
        before.st_size,
        architecture,
        before.st_dev,
        before.st_ino,
    )


def git_output(git: Path, source: Path, args: list[str], label: str) -> str:
    try:
        result = subprocess.run(
            [str(git), "-C", str(source), *args],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Blocked(f"could not verify source checkout {label}") from error
    if result.returncode != 0:
        raise Blocked(f"could not verify source checkout {label}")
    try:
        return result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise Blocked(f"source checkout {label} is not ASCII Git metadata") from error


def capture_source(source_input: Path, build_manifest_path: Path) -> dict[str, Any]:
    if source_input.is_symlink():
        raise Blocked("source checkout must not be selected through a symlink")
    try:
        source = source_input.resolve(strict=True)
    except OSError as error:
        raise Blocked("source checkout is unavailable") from error
    if not source.is_dir():
        raise Blocked("source checkout is not a directory")
    git_name = shutil.which("git")
    if git_name is None:
        raise Blocked("Git is required to verify the exact build source")
    git = Path(git_name).resolve(strict=True)
    revision = git_output(git, source, ["rev-parse", "HEAD"], "commit")
    tree = git_output(git, source, ["rev-parse", "HEAD^{tree}"], "tree")
    dirty = git_output(
        git,
        source,
        ["status", "--porcelain", "--untracked-files=all"],
        "working-tree cleanliness",
    )
    if dirty:
        raise Blocked("source checkout has modified or untracked files")
    lock_path = source / "Cargo.lock"
    lock_bytes = read_bounded_regular(lock_path, 16 * 1024 * 1024, "Cargo.lock")
    build_bytes = read_bounded_regular(
        build_manifest_path, MAX_BUILD_MANIFEST_BYTES, "runtime build manifest"
    )
    manifest = json_no_duplicate_keys(build_bytes, "runtime build manifest")
    if not isinstance(manifest, dict) or manifest.get("schema") != BUILD_SCHEMA:
        raise Blocked("runtime build manifest schema is unsupported")
    source_record = manifest.get("source")
    if not isinstance(source_record, dict) or source_record.get("clean") is not True:
        raise Blocked("runtime build manifest does not certify a clean source revision")
    if source_record.get("commit") != revision:
        raise Blocked("runtime build manifest commit differs from the verified checkout")
    recorded_tree = source_record.get("tree")
    if recorded_tree is not None and recorded_tree != tree:
        raise Blocked("runtime build manifest tree differs from the verified checkout")
    executable_records = manifest.get("executables")
    if not isinstance(executable_records, dict):
        raise Blocked("runtime build manifest has no executable map")
    receipt_binding = manifest.get("root_receipt")
    if not isinstance(receipt_binding, dict) or set(receipt_binding) != {
        "schema",
        "path",
        "sha256",
    }:
        raise Blocked("runtime build manifest has no Root-produced build receipt binding")
    if receipt_binding.get("schema") != "nudox.runtime-artifact-build-receipt.v1":
        raise Blocked("runtime build manifest is not bound to the admitted Root receipt schema")
    receipt_path = receipt_binding.get("path")
    if not isinstance(receipt_path, str) or not Path(receipt_path).is_absolute():
        raise Blocked("runtime build manifest Root receipt path is invalid")
    try:
        verified_receipt = verify_runtime_build_receipt(Path(receipt_path), source)
    except ReceiptError as error:
        raise Blocked(str(error)) from error
    if verified_receipt["receipt_sha256"] != receipt_binding.get("sha256"):
        raise Blocked("Root Cargo receipt digest differs from the runtime manifest binding")
    if (
        verified_receipt["source"]["commit"] != revision
        or verified_receipt["source"]["tree"] != tree
        or verified_receipt["source"]["cargo_lock_sha256"] != sha256_bytes(lock_bytes)
    ):
        raise Blocked("Root Cargo receipt identifies a different source or lockfile")
    for name, artifact in verified_receipt["artifacts"].items():
        recorded = executable_records.get(name)
        if not isinstance(recorded, dict) or any(
            recorded.get(key) != artifact[key] for key in ("path", "sha256", "bytes")
        ):
            raise Blocked(f"runtime manifest {name} does not match the verified Root receipt")
    return {
        "path": source,
        "git": git,
        "commit": revision,
        "tree": tree,
        "cargo_lock_sha256": sha256_bytes(lock_bytes),
        "build_manifest_path": build_manifest_path.resolve(strict=True),
        "build_manifest_sha256": sha256_bytes(build_bytes),
        "root_receipt_sha256": verified_receipt["receipt_sha256"],
        "build_manifest": manifest,
    }


def verify_build_executables(
    manifest: dict[str, Any],
    requested: dict[str, Path],
) -> dict[str, BinaryIdentity]:
    source_record = manifest["build_manifest"].get("executables")
    identities = {
        name: executable_identity(name, path) for name, path in requested.items()
    }
    architectures = {identity.architecture for identity in identities.values()}
    host_arch = normalize_arch(platform.machine())
    if len(architectures) != 1 or architectures != {host_arch}:
        raise Blocked("locald, CLI, MCP, and host architectures do not match exactly")
    for name, identity in identities.items():
        recorded = source_record.get(name)
        if not isinstance(recorded, dict):
            raise Blocked(f"runtime build manifest has no {name} entry")
        current = {
            "path": str(identity.path),
            "sha256": identity.sha256,
            "bytes": identity.size,
        }
        if any(recorded.get(key) != value for key, value in current.items()):
            raise Blocked(f"{name} differs from its exact runtime build-manifest entry")
    return identities


def capture_runtime_policy(source: Path) -> tuple[str, str, str]:
    process_source = (source / "crates/local-service/src/process.rs").read_text(encoding="utf-8")
    policy_source = (
        source / "crates/local-service/src/runtime_policy.rs"
    ).read_text(encoding="utf-8")
    compiler_match = re.search(
        r'pub const COMPILER_ENVIRONMENT_ENV: &str = "([^"]+)";', process_source
    )
    policy_match = re.search(
        r'pub const RUNTIME_POLICY_ENV: &str = "([^"]+)";', policy_source
    )
    if compiler_match is None or policy_match is None:
        raise Blocked("closed compiler or runtime policy environment key is absent from source")
    policy = {
        "version": 1,
        "registry_network_allowed": False,
        "discovery_network_allowed": False,
        "advisory_network_allowed": False,
        "advisory_refresh_enabled": False,
        "registry_cache_max_age_millis": None,
    }
    encoded = canonical_json(policy).decode("utf-8")
    if len(encoded.encode("utf-8")) > MAX_RUNTIME_POLICY_BYTES:
        raise AcceptanceError("strict offline runtime policy exceeds the source codec bound")
    return compiler_match.group(1), policy_match.group(1), encoded


def parse_language_contract(source: Path) -> tuple[dict[str, str], set[str]]:
    language_path = source / "crates/present/language.rs"
    host_path = source / "crates/engine/src/application/host.rs"
    try:
        language_source = language_path.read_text(encoding="utf-8")
        host_source = host_path.read_text(encoding="utf-8")
    except OSError as error:
        raise Blocked("language or compiler-host vocabulary source is unavailable") from error

    name_match = re.search(
        r"pub const fn name\(self\).*?match self \{(.*?)\n\s*\}",
        language_source,
        re.DOTALL,
    )
    extension_match = re.search(
        r'pub fn from_extension\(extension: &str\).*?match extension \{(.*?)\n\s*\}',
        language_source,
        re.DOTALL,
    )
    if name_match is None or extension_match is None:
        raise Blocked("source language vocabulary could not be read safely")
    display_names = dict(
        re.findall(r"Self::(\w+)\s*=>\s*\"([^\"]+)\"", name_match.group(1))
    )
    extension_languages: dict[str, str] = {}
    clause = re.compile(
        r'((?:\s*\"[^\"]+\"\s*\|\s*)*\s*\"[^\"]+\")\s*=>\s*Self::(\w+)',
        re.MULTILINE,
    )
    for extensions, variant in clause.findall(extension_match.group(1)):
        for extension in re.findall(r'\"([^\"]+)\"', extensions):
            extension_languages[f".{extension}"] = variant
    expected_variants = {variant for variant, _ in PROFILE_LANGUAGE_VARIANT.values()}
    if not expected_variants.issubset(display_names):
        raise Blocked("a required language label is absent from the source contract")

    all_match = re.search(
        r"pub const ALL: \[Self;\s*(\d+)\]\s*=\s*\[(.*?)\];",
        host_source,
        re.DOTALL,
    )
    names_match = re.search(
        r"const fn variable_name\(variable: LocalHostVariable\).*?match variable \{(.*?)\n\s*\}",
        host_source,
        re.DOTALL,
    )
    if all_match is None or names_match is None:
        raise Blocked("compiler-host role vocabulary could not be read safely")
    variants = re.findall(r"Self::(\w+)", all_match.group(2))
    declared_count = int(all_match.group(1))
    names = {
        variant: name
        for variant, name in re.findall(
            r"LocalHostVariable::(\w+)\s*=>\s*\"([^\"]+)\"",
            names_match.group(1),
        )
    }
    if len(variants) != declared_count or len(set(variants)) != len(variants):
        raise Blocked("compiler-host role list has duplicate or inconsistent entries")
    if any(variant not in names for variant in variants):
        raise Blocked("compiler-host role list has an unmapped environment name")
    all_names = {names[variant] for variant in variants}
    if "NUDOX_DATA_ROOT" not in all_names:
        raise Blocked("workspace-owned data root is absent from compiler-host source")
    return extension_languages, all_names - {"NUDOX_DATA_ROOT"}


def source_capacity_contract(source: Path) -> dict[str, int]:
    try:
        relation = (source / "crates/engine/src/builtin/relation.rs").read_text(
            encoding="utf-8"
        )
        tree = (source / "crates/version/src/tree/mod.rs").read_text(encoding="utf-8")
        cut = (source / "crates/version/src/tree/cut.rs").read_text(encoding="utf-8")
        surface = (source / "crates/library/surface.rs").read_text(encoding="utf-8")
        capture = (source / "crates/engine/src/compiler_input_capture_v2.rs").read_text(
            encoding="utf-8"
        )
    except OSError as error:
        raise Blocked("source capacity contracts are unavailable") from error

    def integer(source_text: str, pattern: str, label: str) -> int:
        match = re.search(pattern, source_text)
        if match is None:
            raise Blocked(f"could not read the source-defined {label}")
        return int(match.group(1).replace("_", ""))

    max_project_files = integer(
        surface,
        r"pub const MAX_SELECTED_PROJECT_FRONTIER_FILES: usize = ([0-9][0-9_]*);",
        "project file count",
    )
    if not re.search(
        r"pub const MAX_PROJECT_FILES: usize = backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES;",
        relation,
    ):
        raise Blocked("engine Project membership cap no longer shares the surface limit")
    max_label = integer(
        relation,
        r"pub const MAX_LABEL_BYTES: usize = ([0-9][0-9_]*);",
        "project label bound",
    )
    max_node_kib = integer(
        tree,
        r"const DEFAULT_MAX_ENCODED_BYTES: usize = ([0-9][0-9_]*) \* 1024;",
        "canonical tree node size",
    )
    node_overhead = integer(cut, r"const NODE_OVERHEAD_BYTES: usize = ([0-9][0-9_]*);", "node overhead")
    field_frame = integer(cut, r"const FIELD_FRAME_BYTES: usize = ([0-9][0-9_]*);", "field framing")
    key_bytes = integer(
        relation,
        r"type Key = \[u8; ([0-9][0-9_]*)\];",
        "source relation key width",
    )
    if not re.search(
        r"max_row_value_bytes\(size_of::<\s*<ProductSourceRelation as Relation>::Key\s*>\(\)\)",
        relation,
    ):
        raise Blocked("source project-row capacity no longer uses the canonical tree bound")
    if not re.search(
        r"\(Self::ROW_VALUE_CAPACITY - Self::MAX_LABEL_BYTES - 64\)\s*/\s*32",
        relation,
    ):
        raise Blocked("source project frontier formula changed and needs review")
    workspace_charge_mib = integer(
        capture,
        r"const MAX_WORKSPACE_BUILD_CHARGE_BYTES_V2: usize = (\d+) \* 1024 \* 1024;",
        "compiler workspace build-charge bound",
    )
    workspace_total_gib = integer(
        capture,
        r"const MAX_WORKSPACE_TOTAL_FILE_BYTES_V2: u64 = (\d+) \* 1024 \* 1024 \* 1024;",
        "compiler workspace total-file bound",
    )
    row_bytes = max_node_kib * 1024 - key_bytes - node_overhead - 2 * field_frame
    frontier_files = (row_bytes - max_label - 64) // 32
    absolute_inline_files = row_bytes // key_bytes if key_bytes else 0
    if (
        row_bytes <= max_label + 64
        or frontier_files <= 0
        or key_bytes <= 0
        or absolute_inline_files <= frontier_files
    ):
        raise Blocked("source project-row capacity arithmetic is invalid")
    return {
        "project_file_record_maximum": max_project_files,
        "project_row_value_maximum_bytes": row_bytes,
        "project_frontier_file_conservative_maximum": frontier_files,
        # This deliberately ignores label and field overhead. It is the
        # absolute theoretical upper bound (row capacity / key width) on the
        # old inline format, so a large-project pass must exceed it.
        "project_frontier_absolute_inline_maximum": absolute_inline_files,
        # Candidate counts only establish a plausible corpus size. This
        # source-derived floor tracks the separately checked accepted-member
        # bound without treating candidates as indexed files.
        "large_project_candidate_census_minimum": absolute_inline_files + 1,
        "compiler_workspace_build_charge_maximum_bytes": workspace_charge_mib
        * 1024
        * 1024,
        "compiler_workspace_total_file_maximum_bytes": workspace_total_gib
        * 1024
        * 1024
        * 1024,
    }


def parse_semantic_profile_codes(source: Path) -> dict[str, tuple[int, int]]:
    """Read the exact canonical profile byte pairs from the checked-in vocabulary."""
    profile_path = source / "crates/semantic/src/vocabulary/profile.rs"
    authority_path = source / "crates/semantic/src/vocabulary/authority.rs"
    product_path = source / "crates/library/surface.rs"
    try:
        profile_source = profile_path.read_text(encoding="utf-8")
        authority_source = authority_path.read_text(encoding="utf-8")
        product_source = product_path.read_text(encoding="utf-8")
    except OSError as error:
        raise Blocked("canonical semantic profile source is unavailable") from error

    expected = [
        ("Rust", "RustEdition", "Rust2024", "rust"),
        ("Python", "PythonVersion", "Python314", "python"),
        ("TypeScript", "TypeScriptSource", "TypeScript", "typescript"),
        ("TypeScript", "TypeScriptSource", "Tsx", "tsx"),
        ("Go", "GoVersion", "Go125", "go"),
        ("Java", "JavaRelease", "Java25", "java"),
        ("CSharp", "CSharpVersion", "CSharp14", "csharp"),
        ("C", "CStandard", "C23", "c"),
        ("Cxx", "CxxStandard", "Cxx23", "cpp"),
    ]
    profiles_match = re.search(
        r"pub const PRODUCT_PROFILES: \[Self;\s*(\d+)\]\s*=\s*\[(.*?)\];",
        profile_source,
        re.DOTALL,
    )
    if profiles_match is None or int(profiles_match.group(1)) != len(expected):
        raise Blocked("canonical semantic product profile list changed")
    entries = re.findall(
        r"Self::(\w+)\((\w+)::(\w+)\)", profiles_match.group(2)
    )
    if entries != [(family, enum_name, variant) for family, enum_name, variant, _ in expected]:
        raise Blocked("canonical semantic profile selection needs explicit acceptance review")

    language_method = re.search(
        r"pub const fn language\(self\).*?match self \{(.*?)\n\s*\}",
        profile_source,
        re.DOTALL,
    )
    language_codes = re.search(
        r"impl From<Language> for u8 \{.*?match value \{(.*?)\n\s*\}",
        authority_source,
        re.DOTALL,
    )
    if language_method is None or language_codes is None:
        raise Blocked("semantic language/profile discriminant mapping changed")
    family_language = dict(
        re.findall(
            r"Self::(\w+)(?:\(_\))?\s*=>\s*Language::(\w+)",
            language_method.group(1),
        )
    )
    if re.search(
        r"Self::C\(_\)\s*\|\s*Self::Cxx\(_\)\s*=>\s*Language::Clang",
        language_method.group(1),
    ):
        family_language["C"] = "Clang"
        family_language["Cxx"] = "Clang"
    language_values = {
        name: int(value)
        for name, value in re.findall(
            r"Language::(\w+)\s*=>\s*(\d+)", language_codes.group(1)
        )
    }
    enum_values: dict[str, dict[str, int]] = {}
    for enum_name in {enum_name for _, enum_name, _, _ in expected}:
        enum_match = re.search(
            rf"pub enum {enum_name} \{{(.*?)\n\}}",
            profile_source,
            re.DOTALL,
        )
        if enum_match is None:
            raise Blocked(f"semantic profile enum {enum_name} changed")
        enum_values[enum_name] = {
            name: int(value)
            for name, value in re.findall(r"(\w+)\s*=\s*(\d+)", enum_match.group(1))
        }
    if not re.search(r"const CXX_PROFILE_FLAG: u8 = 1 << 7;", profile_source):
        raise Blocked("C++ semantic profile encoding changed")

    names_method = re.search(
        r"pub fn name\(self\).*?match self\.profile\(\)\.ok\(\)\? \{(.*?)\n\s*\}",
        product_source,
        re.DOTALL,
    )
    if names_method is None:
        raise Blocked("canonical semantic profile names changed")
    name_arms: dict[tuple[str, str | None, str | None], str] = {}
    detailed_arms = re.findall(
        r"LanguageProfile::(?P<family>\w+)(?:\((?:(?P<enum>\w+)::(?P<variant>\w+)|_)\))?\s*=>\s*Some\(\"(?P<name>[^\"]+)\"\)",
        names_method.group(1),
    )
    for family, enum_name, variant, name in detailed_arms:
        name_arms[(family, enum_name or None, variant or None)] = name

    result: dict[str, tuple[int, int]] = {}
    for family, enum_name, variant, name in expected:
        language = family_language.get(family)
        profile_value = enum_values[enum_name].get(variant)
        if language is None or language not in language_values or profile_value is None:
            raise Blocked("semantic profile code is not represented by its source enums")
        if name_arms.get((family, enum_name, variant)) != name:
            # Non-TypeScript profiles use wildcard name arms; TypeScript has
            # distinct typed arms for ordinary TS and TSX.
            if not (
                family != "TypeScript"
                and name_arms.get((family, None, None)) == name
            ):
                raise Blocked(f"semantic profile spelling {name} changed")
        if family == "Cxx":
            profile_value |= 1 << 7
        result[name] = (language_values[language], profile_value)
    result["javascript"] = result["typescript"]
    if set(result) != set(PROFILE_LANGUAGE_VARIANT):
        raise Blocked("semantic profile names differ from the acceptance vocabulary")
    return result


def parse_closed_snapshot(
    path: Path,
    compiler_key: str,
    source: Path,
    role_names: set[str],
) -> tuple[str, dict[str, str], str]:
    raw = read_bounded_regular(
        path, MAX_COMPILER_SNAPSHOT_BYTES, "closed compiler-host snapshot"
    )
    try:
        snapshot = json_no_duplicate_keys(raw, "closed compiler-host snapshot")
    except Blocked:
        raise
    if not isinstance(snapshot, dict) or list(snapshot) != ["version", "paths"]:
        raise Blocked("closed compiler-host snapshot has an unsupported shape")
    if snapshot["version"] != 1 or not isinstance(snapshot["paths"], list):
        raise Blocked("closed compiler-host snapshot version or paths are invalid")

    host_source = (source / "crates/engine/src/application/host.rs").read_text(
        encoding="utf-8"
    )
    all_match = re.search(
        r"pub const ALL: \[Self;\s*\d+\]\s*=\s*\[(.*?)\];",
        host_source,
        re.DOTALL,
    )
    names_match = re.search(
        r"const fn variable_name\(variable: LocalHostVariable\).*?match variable \{(.*?)\n\s*\}",
        host_source,
        re.DOTALL,
    )
    if all_match is None or names_match is None:
        raise Blocked("compiler-host role ordering could not be verified")
    variants = re.findall(r"Self::(\w+)", all_match.group(1))
    names = dict(
        re.findall(
            r"LocalHostVariable::(\w+)\s*=>\s*\"([^\"]+)\"",
            names_match.group(1),
        )
    )
    canonical_roles = [
        names[variant] for variant in variants if names[variant] != "NUDOX_DATA_ROOT"
    ]
    selected: dict[str, str] = {}
    wire_paths: list[tuple[str, str]] = []
    for entry in snapshot["paths"]:
        if not isinstance(entry, dict) or list(entry) != ["variable", "path"]:
            raise Blocked("closed compiler-host snapshot has a malformed role entry")
        variable = entry["variable"]
        path_wire = entry["path"]
        if not isinstance(variable, str) or variable not in role_names:
            raise Blocked("closed compiler-host snapshot contains an unknown role")
        if variable in selected:
            raise Blocked("closed compiler-host snapshot repeats a role")
        if not isinstance(path_wire, dict) or list(path_wire) != ["encoding", "units"]:
            raise Blocked("closed compiler-host snapshot has a malformed native path")
        if os.name == "posix":
            if path_wire["encoding"] != "unix" or not isinstance(path_wire["units"], list):
                raise Blocked("closed compiler-host snapshot uses another platform path encoding")
            units = path_wire["units"]
            if any(type(unit) is not int or not 0 <= unit <= 255 for unit in units):
                raise Blocked("closed compiler-host snapshot has invalid Unix path units")
            if not units or 0 in units:
                raise Blocked("closed compiler-host snapshot has an empty or NUL path")
            native = bytes(units)
            decoded_path = os.fsdecode(native)
            if not os.path.isabs(decoded_path):
                raise Blocked("closed compiler-host snapshot contains a relative path")
            selected[variable] = decoded_path
            wire_paths.append((variable, decoded_path))
        else:
            raise Blocked("this acceptance runner currently requires Unix native paths")
    positions = [canonical_roles.index(name) for name in selected]
    if positions != sorted(positions):
        raise Blocked("closed compiler-host snapshot role order is not canonical")
    if canonical_json(snapshot) != raw:
        raise Blocked("closed compiler-host snapshot is valid JSON but not canonical")
    canonical_hash = sha256_bytes(raw)
    return raw.decode("utf-8"), selected, canonical_hash


def validate_snapshot_toolchains(
    selected: dict[str, str],
    cases: list[ProjectCase],
    extension_languages: dict[str, str],
) -> dict[str, list[str]]:
    def has_admitted_path_kind(role: str) -> bool:
        raw_path = selected.get(role)
        if raw_path is None:
            return False
        path = Path(raw_path)
        try:
            info = path.stat()
        except OSError:
            return False
        if role in PROFILE_DIRECTORY_ROLES:
            return stat.S_ISDIR(info.st_mode)
        if role in PROFILE_EXECUTABLE_ROLES:
            return stat.S_ISREG(info.st_mode) and os.access(path, os.X_OK)
        return stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)

    required_profiles = {symbol["profile"] for case in cases for symbol in case.symbols}
    blocked: dict[str, list[str]] = {}
    for profile in sorted(required_profiles):
        missing: list[str] = []
        for role in PROFILE_REQUIRED_ROLES[profile]:
            if not has_admitted_path_kind(role):
                missing.append(role)
        if profile in {"javascript", "typescript", "tsx"}:
            # Match LocalCompilerHost::package_authority: the compiler is
            # mandatory, plus either a direct report program or both Node and
            # its module root. Every configured role must also keep its path
            # kind valid, even when the alternate route is complete.
            typescript_roles = (
                "NUDOX_TSC",
                "NUDOX_TYPESCRIPT_REPORT_PROGRAM",
                "NUDOX_TYPESCRIPT_NODE",
                "NUDOX_TYPESCRIPT_MODULE_ROOT",
            )
            invalid_selected_roles = [
                role
                for role in typescript_roles
                if role in selected and not has_admitted_path_kind(role)
            ]
            missing.extend(invalid_selected_roles)
            compiler_available = has_admitted_path_kind("NUDOX_TSC")
            report_available = has_admitted_path_kind("NUDOX_TYPESCRIPT_REPORT_PROGRAM")
            node_route_available = (
                has_admitted_path_kind("NUDOX_TYPESCRIPT_NODE")
                and has_admitted_path_kind("NUDOX_TYPESCRIPT_MODULE_ROOT")
            )
            if not compiler_available and "NUDOX_TSC" not in missing:
                missing.append("NUDOX_TSC")
            if not (report_available or node_route_available):
                missing.append(
                    "NUDOX_TYPESCRIPT_REPORT_PROGRAM or "
                    "(NUDOX_TYPESCRIPT_NODE and NUDOX_TYPESCRIPT_MODULE_ROOT)"
                )
        if missing:
            blocked[profile] = missing

    for case in cases:
        for symbol in case.symbols:
            extension = Path(symbol["path"]).suffix.lower()
            variant, _ = PROFILE_LANGUAGE_VARIANT[symbol["profile"]]
            if extension not in PROFILE_EXTENSIONS[symbol["profile"]]:
                raise Blocked(
                    f"manifest symbol extension does not match profile {symbol['profile']}"
                )
            if extension_languages.get(extension) != variant:
                raise Blocked(
                    f"source language mapping differs for profile {symbol['profile']}"
                )
    if blocked:
        roles = sorted({role for missing in blocked.values() for role in missing})
        raise Blocked(
            "closed snapshot does not select every required toolchain role: "
            + ", ".join(roles)
        )
    return {profile: list(PROFILE_REQUIRED_ROLES[profile]) for profile in sorted(required_profiles)}


def validate_package_metadata(value: Any, language: str | None) -> dict[str, Any]:
    ecosystems = {"typescript": "npm", "python": "pypi", "go": "go"}
    if language not in ecosystems:
        raise Blocked("package provenance metadata requires an explicit language corpus scope")
    if not isinstance(value, dict) or set(value) != {"ecosystem", "id", "version", "provenance"}:
        raise Blocked("package metadata has missing or unknown fields")
    if value["ecosystem"] != ecosystems[language]:
        raise Blocked("package ecosystem does not match the selected language corpus")
    for field, maximum in [("id", 1024), ("version", 256)]:
        text = value[field]
        if not isinstance(text, str) or not text or len(text) > maximum or any(
            ord(char) < 33 or ord(char) > 126 for char in text
        ):
            raise Blocked("package identity and version must be bounded nonempty ASCII labels")
    if language == "typescript" and value["id"].lower() != value["id"]:
        raise Blocked("npm package identity is not canonical")
    if language == "python" and re.sub(r"[-_.]+", "-", value["id"]).lower() != value["id"]:
        raise Blocked("PyPI package identity is not canonical")
    provenance = value["provenance"]
    if (
        not isinstance(provenance, dict)
        or set(provenance) != {"kind", "sha256"}
        or provenance.get("kind") not in {"archive-sha256", "source-tree-sha256"}
        or not isinstance(provenance.get("sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", provenance["sha256"]) is None
    ):
        raise Blocked("package provenance requires one exact archive or source-tree SHA-256")
    return value


def validate_corpus_manifest(
    path: Path,
    output: Path,
    extension_languages: dict[str, str],
    large_candidate_census_minimum: int,
    language: str | None = None,
) -> tuple[list[ProjectCase], bytes]:
    if language is not None and language not in LANGUAGE_CORPUS_PROFILES:
        raise Blocked("language corpus scope must be typescript, python, or go")
    if (
        type(large_candidate_census_minimum) is not int
        or large_candidate_census_minimum <= 0
    ):
        raise Blocked("source-derived large-project candidate minimum is invalid")
    raw = read_bounded_regular(path, MAX_CORPUS_MANIFEST_BYTES, "real corpus manifest")
    manifest = json_no_duplicate_keys(raw, "real corpus manifest")
    if not isinstance(manifest, dict) or manifest.get("schema") != MANIFEST_SCHEMA:
        raise Blocked("real corpus manifest schema is unsupported")
    if set(manifest) != {"schema", "projects"}:
        raise Blocked("real corpus manifest has missing or unknown top-level fields")
    projects_value = manifest["projects"]
    if not isinstance(projects_value, list) or not projects_value or len(projects_value) > MAX_PROJECTS:
        raise Blocked(f"real corpus manifest must contain 1..{MAX_PROJECTS} projects")

    output_resolved = output.resolve(strict=False)
    cases: list[ProjectCase] = []
    project_ids: set[str] = set()
    canonical_roots: set[Path] = set()
    total_symbols = 0
    variants: set[str] = set()
    large_count = 0
    for item in projects_value:
        project_fields = {
            "id",
            "path",
            "large",
            "minimum_source_candidates",
            "symbols",
        }
        if not isinstance(item, dict) or not project_fields <= set(item) or set(item) - project_fields - {"package", "acquisition"}:
            raise Blocked("a project manifest entry has missing or unknown fields")
        package_metadata = validate_package_metadata(item["package"], language) if "package" in item else None
        acquisition = item.get("acquisition")
        if "acquisition" in item:
            acquisition_fields = {"inventory_path", "inventory_sha256", "target_subdir"}
            if package_metadata is None or not isinstance(acquisition, dict) or not acquisition_fields <= set(acquisition) or set(acquisition) - acquisition_fields - {"origin"}:
                raise Blocked("acquisition requires package metadata and one closed inventory binding")
            if not isinstance(acquisition["inventory_path"], str) or not os.path.isabs(acquisition["inventory_path"]):
                raise Blocked("acquired inventory path must be absolute")
            if not isinstance(acquisition["inventory_sha256"], str) or re.fullmatch(r"[0-9a-f]{64}", acquisition["inventory_sha256"]) is None:
                raise Blocked("acquired inventory digest is invalid")
            canonical_inventory_relative(acquisition["target_subdir"], allow_root=True)
            if "origin" in acquisition:
                origin = acquisition["origin"]
                if not isinstance(origin, dict) or set(origin) != {"receipt_path", "receipt_sha256"} or not isinstance(origin["receipt_path"], str) or not os.path.isabs(origin["receipt_path"]) or not isinstance(origin["receipt_sha256"], str) or re.fullmatch(r"[0-9a-f]{64}", origin["receipt_sha256"]) is None:
                    raise Blocked("registry origin requires a closed receipt path and digest")
        project_id = item["id"]
        raw_path = item["path"]
        large = item["large"]
        minimum = item["minimum_source_candidates"]
        symbols_value = item["symbols"]
        if (
            not isinstance(project_id, str)
            or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}", project_id)
            or project_id in project_ids
        ):
            raise Blocked("project identifiers must be unique bounded labels")
        if not isinstance(raw_path, str) or not os.path.isabs(raw_path):
            raise Blocked(f"project {project_id} must use an explicit absolute real path")
        if type(large) is not bool or type(minimum) is not int or minimum < 0:
            raise Blocked(f"project {project_id} has invalid candidate-count policy")
        if large:
            large_count += 1
            minimum = max(minimum, large_candidate_census_minimum)
        if (
            not isinstance(symbols_value, list)
            or not symbols_value
            or total_symbols + len(symbols_value) > MAX_SYMBOLS
        ):
            raise Blocked(f"project {project_id} has no symbols or exceeds the symbol bound")
        input_path = Path(raw_path)
        if input_path.is_symlink():
            raise Blocked(f"project {project_id} root must not be a symlink")
        try:
            root = input_path.resolve(strict=True)
        except OSError as error:
            raise Blocked(f"real project {project_id} is unavailable") from error
        if not root.is_dir():
            raise Blocked(f"real project {project_id} is not a directory")
        if output_resolved == root or output_resolved.is_relative_to(root) or root.is_relative_to(output_resolved):
            raise Blocked("fresh evidence output and real project roots must be disjoint")
        if root in canonical_roots:
            raise Blocked("project manifest contains duplicate canonical project roots")

        symbols: list[dict[str, Any]] = []
        for symbol in symbols_value:
            fields = {"profile", "path", "name"}
            if not isinstance(symbol, dict) or not fields <= set(symbol) or set(symbol) - fields - {"surface_contract"}:
                raise Blocked(f"project {project_id} has a malformed symbol expectation")
            profile = symbol["profile"]
            relative = symbol["path"]
            name = symbol["name"]
            if profile not in PROFILE_LANGUAGE_VARIANT:
                raise Blocked(f"project {project_id} uses an unsupported profile")
            if language is not None and profile not in LANGUAGE_CORPUS_PROFILES[language]:
                raise Blocked(f"project {project_id} symbol is outside the explicit {language} scope")
            if not isinstance(relative, str) or not isinstance(name, str) or not name.strip():
                raise Blocked(f"project {project_id} has an empty symbol expectation")
            relative_path = PurePosixPath(relative)
            if (
                relative_path.is_absolute()
                or not relative_path.parts
                or any(part in {"", ".", ".."} for part in relative_path.parts)
                or "\\" in relative
                or ignored_source_directory(relative_path.parts[:-1])
            ):
                raise Blocked(f"project {project_id} has a noncanonical relative source path")
            if len(relative.encode("utf-8")) > 4096 or len(name.encode("utf-8")) > 4096:
                raise Blocked(f"project {project_id} has an oversized symbol expectation")
            extension = Path(relative).suffix.lower()
            variant, _ = PROFILE_LANGUAGE_VARIANT[profile]
            if extension not in PROFILE_EXTENSIONS[profile]:
                raise Blocked(f"symbol source extension does not match profile {profile}")
            if extension_languages.get(extension) != variant:
                raise Blocked(f"source language contract does not match profile {profile}")
            source = root.joinpath(*relative_path.parts)
            current = root
            for part in relative_path.parts:
                current = current / part
                if current.is_symlink():
                    raise Blocked(f"project {project_id} expected source crosses a symlink")
            try:
                info = source.lstat()
            except OSError as error:
                raise Blocked(
                    f"project {project_id} expected source path does not exist"
                ) from error
            if not stat.S_ISREG(info.st_mode):
                raise Blocked(f"project {project_id} expected source is not a regular file")
            admitted = {"profile": profile, "path": relative, "name": name}
            if "surface_contract" in symbol:
                admitted["surface_contract"] = validate_surface_contract(symbol["surface_contract"], root, relative)
            symbols.append(admitted)
            variants.add(profile)
        if language == "typescript" and not any(
            symbol["profile"] in {"typescript", "tsx"} for symbol in symbols
        ):
            raise Blocked(f"project {project_id} has no TypeScript symbol expectation")
        project_ids.add(project_id)
        canonical_roots.add(root)
        total_symbols += len(symbols)
        cases.append(ProjectCase(project_id, root, large, minimum, tuple(symbols), package_metadata, acquisition))

    required_variants = set(PROFILE_LANGUAGE_VARIANT) if language is None else set()
    missing_variants = sorted(required_variants - variants)
    if missing_variants:
        raise Blocked(
            "real corpus manifest is missing supported-profile cases: "
            + ", ".join(missing_variants)
        )
    if language is None and not large_count:
        raise Blocked(
            f"real corpus manifest must identify a large project with at least "
            f"{large_candidate_census_minimum} recognized source candidates"
        )
    return cases, raw


def canonical_inventory_relative(value: Any, *, allow_root: bool = False) -> tuple[str, ...]:
    if value == "." and allow_root:
        return ()
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > 4096:
        raise Blocked("acquired inventory has an invalid relative path")
    path = PurePosixPath(value)
    if not path.parts or path.is_absolute() or path.as_posix() != value or "\\" in value or any(part in {".", ".."} for part in path.parts):
        raise Blocked("acquired inventory path is not canonical and confined")
    return path.parts


def verify_acquired_source_inventory(case: ProjectCase, manifest_sha: str, deadline: Deadline | None = None) -> dict[str, Any]:
    """Verify acquired bytes independently of recognized-source census and declarations."""
    if case.package is None or case.acquisition is None:
        raise Blocked("verified package source requires an acquired inventory")
    binding = case.acquisition
    inventory_path = Path(binding["inventory_path"])
    raw = read_bounded_regular(inventory_path, 32 * 1024 * 1024, "acquired package inventory")
    if sha256_bytes(raw) != binding["inventory_sha256"]:
        raise AcceptanceError("acquired inventory differs from its declared digest")
    inventory = json_no_duplicate_keys(raw, "acquired package inventory")
    if not isinstance(inventory, dict) or set(inventory) != {"schema", "package", "source_root", "files"} or inventory["schema"] != "nudox.acquired-package-source-inventory.v1":
        raise Blocked("acquired package inventory schema is unsupported")
    package = {key: case.package[key] for key in ("ecosystem", "id", "version")}
    if inventory["package"] != package:
        raise AcceptanceError("acquired inventory belongs to another package or version")
    root_value = inventory["source_root"]
    if not isinstance(root_value, str) or not os.path.isabs(root_value):
        raise Blocked("acquired source root must be absolute")
    root = Path(root_value)
    try:
        if not root.is_dir() or root.resolve(strict=True) != root:
            raise Blocked("acquired source root must be a canonical directory without symlinks")
    except OSError as error:
        raise Blocked("acquired source root is unavailable") from error
    target = root.joinpath(*canonical_inventory_relative(binding["target_subdir"], allow_root=True))
    if target != case.path:
        raise AcceptanceError("runtime project root is not the acquired package target subdirectory")
    files = inventory["files"]
    if not isinstance(files, list) or not 1 <= len(files) <= MAX_SOURCE_CANDIDATES:
        raise Blocked("acquired source inventory exceeds the file bound or is empty")
    paths: list[str] = []
    for row in files:
        if not isinstance(row, dict) or set(row) != {"path", "bytes", "sha256"}:
            raise Blocked("acquired source file record has unknown or missing fields")
        parts = canonical_inventory_relative(row["path"])
        if parts[0] == ".git":
            raise Blocked("acquired source inventory must exclude root Git metadata")
        if type(row["bytes"]) is not int or not 0 <= row["bytes"] <= MAX_SOURCE_FILE_BYTES or not isinstance(row["sha256"], str) or re.fullmatch(r"[0-9a-f]{64}", row["sha256"]) is None:
            raise Blocked("acquired source file size or digest is invalid")
        paths.append(row["path"])
    if paths != sorted(set(paths)):
        raise Blocked("acquired source inventory paths must be sorted and unique")
    observed: list[dict[str, Any]] = []
    stack = [root]
    total = 0
    directories = 0
    while stack:
        directory = stack.pop()
        directories += 1
        if directories > MAX_SOURCE_CANDIDATES:
            raise Blocked("acquired package directory traversal exceeds its bound")
        if deadline is not None:
            deadline.check("acquired package source verification")
        with os.scandir(directory) as entries:
            for entry in entries:
                if directory == root and entry.name == ".git":
                    continue
                if entry.is_symlink():
                    raise AcceptanceError("acquired package tree contains a symlink")
                path = Path(entry.path)
                if entry.is_dir(follow_symlinks=False):
                    stack.append(path)
                    if len(stack) > MAX_SOURCE_CANDIDATES:
                        raise Blocked("acquired package directory traversal exceeds its bound")
                elif entry.is_file(follow_symlinks=False):
                    size, digest, info = read_source_file(path, deadline)
                    if info.st_nlink != 1:
                        raise AcceptanceError("acquired package file has multiple links")
                    total += size
                    if total > MAX_SOURCE_BYTES_TOTAL or len(observed) >= MAX_SOURCE_CANDIDATES:
                        raise Blocked("acquired package tree exceeds its byte or file bound")
                    observed.append({"path": path.relative_to(root).as_posix(), "bytes": size, "sha256": digest.hex()})
                else:
                    raise AcceptanceError("acquired package tree contains a nonregular entry")
    observed.sort(key=lambda row: row["path"])
    if observed != files:
        raise AcceptanceError("acquired package source inventory has missing, extra, or changed files")
    tree_sha = sha256_bytes(canonical_json(observed))
    if case.package["provenance"]["kind"] == "source-tree-sha256" and case.package["provenance"]["sha256"] != tree_sha:
        raise AcceptanceError("declared source-tree hash differs from the independently verified inventory")
    proof = {"verification": "verified-source-inventory-v1", "package": package,
            "target_package": package,
            "origin_verification": "unverified-declared-package",
            "source_tree_sha256": tree_sha, "acquired_inventory_sha256": sha256_bytes(raw),
            "target_subdir": binding["target_subdir"],
            "target_root_identity_sha256": sha256_bytes(str(target).encode()),
            "corpus_manifest_sha256": manifest_sha, "declared": case.package}
    if "origin" in binding:
        proof.update(verify_registry_origin(binding["origin"], package, observed, deadline))
    return proof


def verify_registry_origin(binding: dict[str, Any], package: dict[str, Any], files: list[dict[str, Any]], deadline: Deadline | None) -> dict[str, Any]:
    raw = read_bounded_regular(Path(binding["receipt_path"]), MAX_CORPUS_MANIFEST_BYTES, "registry origin receipt")
    if sha256_bytes(raw) != binding["receipt_sha256"]:
        raise AcceptanceError("registry origin receipt differs from its declared digest")
    receipt = json_no_duplicate_keys(raw, "registry origin receipt")
    fields = {"schema", "package", "registry_metadata", "archive", "unpack"}
    if not isinstance(receipt, dict) or set(receipt) != fields or receipt["schema"] != "nudox.registry-artifact-origin.v1" or receipt["package"] != package:
        raise Blocked("registry origin receipt has an invalid closed package binding")
    payloads = {}
    for name in ("registry_metadata", "archive"):
        row = receipt[name]
        if not isinstance(row, dict) or set(row) != {"path", "sha256", "url"} or not isinstance(row["path"], str) or not os.path.isabs(row["path"]) or not isinstance(row["sha256"], str) or re.fullmatch(r"[0-9a-f]{64}", row["sha256"]) is None:
            raise Blocked("registry origin artifact binding is malformed")
        payloads[name] = read_bounded_regular(Path(row["path"]), MAX_SOURCE_FILE_BYTES, "registry origin " + name)
        if sha256_bytes(payloads[name]) != row["sha256"]:
            raise AcceptanceError("registry origin artifact differs from its retained digest")
    unpack = receipt["unpack"]
    if not isinstance(unpack, dict) or set(unpack) != {"strip_prefix"}:
        raise Blocked("registry origin extraction prefix is malformed")
    metadata = json_no_duplicate_keys(payloads["registry_metadata"], "official registry metadata")
    if not isinstance(metadata, dict):
        raise Blocked("official registry metadata is not an object")
    try:
        members, special = archive_members(payloads["archive"], unpack["strip_prefix"], MAX_SOURCE_CANDIDATES,
            MAX_SOURCE_FILE_BYTES, MAX_SOURCE_BYTES_TOTAL,
            (lambda: deadline.check("registry archive membership verification")) if deadline else (lambda: None))
        if members != files:
            raise OriginError("registry archive membership differs from the actual acquired source tree")
        verify_registry_metadata(package, metadata, receipt["registry_metadata"]["url"],
                                 payloads["archive"], receipt["archive"]["url"], special)
    except OriginError as error:
        raise AcceptanceError(str(error)) from error
    return {"origin_verification": "verified-registry-artifact-v1", "origin_evidence": {
        "metadata_sha256": sha256_bytes(payloads["registry_metadata"]),
        "archive_sha256": sha256_bytes(payloads["archive"]),
        "archive_membership_sha256": sha256_bytes(canonical_json(members)),
        "receipt_sha256": sha256_bytes(raw),
        "verification_source": "retained official metadata, archive identity and exact acquired-file membership",
    }}


def read_source_file(
    path: Path, deadline: Deadline | None = None
) -> tuple[int, bytes, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_CLOEXEC", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb", closefd=True) as stream:
            before = os.fstat(stream.fileno())
            if not stat.S_ISREG(before.st_mode):
                raise AcceptanceError("a recognized project source stopped being a regular file")
            if before.st_size > MAX_SOURCE_FILE_BYTES:
                raise Blocked(
                    f"a source candidate exceeded the {MAX_SOURCE_FILE_BYTES}-byte file bound"
                )
            content_digest = hashlib.sha256()
            bytes_read = 0
            while True:
                if deadline is not None:
                    deadline.check("source-file fingerprinting")
                block = stream.read(min(65_536, MAX_SOURCE_FILE_BYTES + 1 - bytes_read))
                if not block:
                    break
                content_digest.update(block)
                bytes_read += len(block)
                if bytes_read > MAX_SOURCE_FILE_BYTES:
                    break
            after = os.fstat(stream.fileno())
        current = path.lstat()
    except OSError as error:
        raise AcceptanceError("a recognized project source could not be read consistently") from error
    if bytes_read > MAX_SOURCE_FILE_BYTES:
        raise Blocked(f"a source candidate exceeded the {MAX_SOURCE_FILE_BYTES}-byte file bound")
    if (
        (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        or bytes_read != before.st_size
        or not stat.S_ISREG(current.st_mode)
        or (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
        != (current.st_dev, current.st_ino, current.st_size, current.st_mtime_ns)
    ):
        raise AcceptanceError("a project source changed while it was fingerprinted")
    return bytes_read, content_digest.digest(), after


def project_census(root: Path, deadline: Deadline | None = None) -> Census:
    try:
        root_info = root.stat()
    except OSError as error:
        raise AcceptanceError("a project root disappeared during fingerprinting") from error
    digest = hashlib.sha256()
    stack = [root]
    count = 0
    total = 0
    extension_counts: dict[str, int] = {}
    while stack:
        if deadline is not None:
            deadline.check("project source inventory")
        directory = stack.pop()
        try:
            with os.scandir(directory) as iterator:
                entries = sorted(iterator, key=lambda entry: entry.name, reverse=True)
        except OSError as error:
            raise AcceptanceError("a real project directory could not be traversed") from error
        for entry in entries:
            if deadline is not None:
                deadline.check("project source inventory")
            if entry.is_symlink():
                continue
            if entry.is_dir(follow_symlinks=False):
                if not ignored_source_directory(Path(entry.path).relative_to(root).parts):
                    stack.append(Path(entry.path))
                continue
            suffix = Path(entry.name).suffix.lower()
            if suffix not in SOURCE_EXTENSIONS or not entry.is_file(follow_symlinks=False):
                continue
            count += 1
            extension_counts[suffix] = extension_counts.get(suffix, 0) + 1
            if count > MAX_SOURCE_CANDIDATES:
                raise Blocked(
                    f"recognized source candidates exceeded the {MAX_SOURCE_CANDIDATES}-file inventory bound"
                )
            file_path = Path(entry.path)
            source_bytes, content_hash, info = read_source_file(file_path, deadline)
            total += source_bytes
            if total > MAX_SOURCE_BYTES_TOTAL:
                raise Blocked(
                    f"recognized source candidates exceeded the {MAX_SOURCE_BYTES_TOTAL}-byte inventory bound"
                )
            relative = file_path.relative_to(root).as_posix().encode("utf-8", errors="surrogateescape")
            digest.update(len(relative).to_bytes(4, "big"))
            digest.update(relative)
            digest.update(info.st_size.to_bytes(8, "big"))
            digest.update(content_hash)
    return Census(
        digest.hexdigest(),
        count,
        total,
        dict(sorted(extension_counts.items())),
        root_info.st_dev,
        root_info.st_ino,
    )


def verify_symbol_candidates(cases: list[ProjectCase], censuses: dict[str, Census]) -> None:
    for case in cases:
        if censuses[case.project_id].files < case.min_candidates:
            if case.large:
                raise AcceptanceError(
                    f"large project {case.project_id} has fewer than its minimum recognized "
                    f"source candidates ({censuses[case.project_id].files} < {case.min_candidates})"
                )
            raise Blocked(f"project {case.project_id} is below its declared source-candidate minimum")
        for symbol in case.symbols:
            expected = case.path.joinpath(*PurePosixPath(symbol["path"]).parts)
            if expected.is_symlink():
                raise Blocked(f"manifest symbol in {case.project_id} is a symlink")


def minimal_environment(
    *,
    compiler_snapshot: str | None = None,
    compiler_key: str | None = None,
    setup_mode: str = "configured",
    policy: str | None = None,
    policy_key: str | None = None,
    client: bool = False,
    temp_root: Path,
) -> tuple[dict[str, str], str]:
    if setup_mode not in {"stock", "configured"}:
        raise Blocked("runtime setup mode must be stock or configured")
    captured: dict[str, str] = {}
    for name in ("PATH", "LANG", "LC_ALL", "TMPDIR"):
        value = os.environ.get(name)
        if value is not None:
            captured[name] = value
    captured["TMPDIR"] = str(temp_root)
    if setup_mode == "configured" and compiler_snapshot is not None and compiler_key is not None:
        captured[compiler_key] = compiler_snapshot
    if policy is not None and policy_key is not None:
        captured[policy_key] = policy
    if client:
        # CLI --passive is deliberately limited to health/status. This closed
        # absolute path makes normal CLI/MCP commands fail instead of starting
        # an untracked locald if the exact owned endpoint disappears.
        captured["BACKEND_LOCALD_BIN"] = str(temp_root / "absent-backend-locald")
    safe_witness = {
        name: sha256_bytes(value.encode("utf-8", errors="surrogateescape"))
        for name, value in captured.items()
        if name not in {"BACKEND_LOCALD_COMPILER_ENVIRONMENT"}
    }
    return captured, sha256_bytes(canonical_json(safe_witness))


def runtime_setup_receipt(mode: str, environment: dict[str, str], environment_sha: str,
                          compiler_key: str, compiler_roles: list[str]) -> dict[str, Any]:
    overrides = sorted(set(environment) & {compiler_key, *compiler_roles})
    injected = compiler_key in environment
    if mode == "stock" and (injected or overrides):
        raise AcceptanceError("stock runtime environment contains compiler overrides")
    if mode == "configured" and (not injected or not overrides):
        raise AcceptanceError("configured runtime environment is missing its compiler snapshot")
    return {"mode": mode, "compiler_snapshot_injected": injected,
            "compiler_override_keys": overrides, "owner_environment_sha256": environment_sha}


def make_operation_keys(cases: list[ProjectCase], output: Path) -> dict[str, str]:
    keys = {case.project_id: secrets_hex_32() for case in cases}
    payload = {
        "schema": "nudox.local-workspace-index-keys.v1",
        "operations": [
            {"project_id": case.project_id, "key": keys[case.project_id]}
            for case in cases
        ],
    }
    path = output / "operation-keys.json"
    data = canonical_json(payload) + b"\n"
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    return keys


def secrets_hex_32() -> str:
    # Importing secrets here keeps the module's deterministic parsers free from
    # an ambient pseudorandom generator.
    import secrets

    return secrets.token_hex(32)


def make_surface_command(operation: str, key: str, package_path: Path) -> dict[str, Any]:
    if operation == "index-operation-start":
        return {
            "operation": operation,
            "operation_key": key,
            "package": {"kind": "local", "value": str(package_path)},
            "execution_intent": "interactive",
        }
    if operation == "index-operation-status":
        return {"operation": operation, "operation_key": key}
    raise AssertionError("unsupported internal surface operation")


def parse_json_output(stdout: bytes, label: str) -> Any:
    try:
        return json_no_duplicate_keys(stdout, label)
    except Blocked as error:
        raise AcceptanceError(f"{label} did not return one valid JSON value") from error


def cli_call(
    binary: BinaryIdentity,
    owner_workspace: Path,
    endpoint: Path,
    project: Path,
    command: list[str],
    environment: dict[str, str],
    deadline: Deadline,
    evidence: list[dict[str, Any]],
    label: str,
) -> Any:
    require_owner_spawn_disabled(environment, label)
    args = [
        str(binary.path),
        "--json",
        "--detail",
        "full",
        "--workspace",
        str(owner_workspace),
        "--project",
        str(project),
        "--endpoint",
        str(endpoint),
        *command,
    ]
    stdout, stderr, return_code, elapsed = run_bounded_process(
        args, environment, None, deadline, label
    )
    item: dict[str, Any] = {
        "client": "cli",
        "label": label,
        "return_code": return_code,
        "elapsed_seconds": round(elapsed, 3),
        "request_arguments": command,
        "stdout": bounded_client_payload_evidence(stdout),
        "stderr": bounded_client_payload_evidence(stderr),
    }
    if return_code != 0:
        item["failure"] = {
            "kind": "process-exit",
            "exit_code": return_code,
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} CLI process exited with code {return_code}")
    try:
        value = parse_json_output(stdout, label)
    except AcceptanceError as error:
        item["failure"] = {
            "kind": "invalid-json-response",
            "detail": str(error)[:2048],
        }
        append_client_evidence(evidence, item)
        raise
    append_client_evidence(evidence, item)
    return value


def mcp_call(
    binary: BinaryIdentity,
    owner_workspace: Path,
    endpoint: Path,
    project: Path,
    tool: str,
    arguments: dict[str, Any],
    environment: dict[str, str],
    deadline: Deadline,
    evidence: list[dict[str, Any]],
    label: str,
) -> Any:
    require_owner_spawn_disabled(environment, label)
    request = b"".join(
        [
            canonical_json(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {
                            "name": "real-workspace-index-acceptance",
                            "version": "1",
                        },
                    },
                }
            ),
            b"\n",
            canonical_json(
                {"jsonrpc": "2.0", "method": "notifications/initialized"}
            ),
            b"\n",
            canonical_json(
                {
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": tool, "arguments": arguments},
                }
            ),
            b"\n",
        ]
    )
    args = [
        str(binary.path),
        "--workspace",
        str(owner_workspace),
        "--project",
        str(project),
        "--endpoint",
        str(endpoint),
    ]
    stdout, stderr, return_code, elapsed = run_bounded_process(
        args, environment, request, deadline, label
    )
    item: dict[str, Any] = {
        "client": "mcp",
        "label": label,
        "tool": tool,
        "return_code": return_code,
        "elapsed_seconds": round(elapsed, 3),
        "request": bounded_client_payload_evidence(request),
        "stdout": bounded_client_payload_evidence(stdout),
        "stderr": bounded_client_payload_evidence(stderr),
    }
    if return_code != 0:
        item["failure"] = {
            "kind": "process-exit",
            "exit_code": return_code,
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} MCP process exited with code {return_code}")
    responses: list[dict[str, Any]] = []
    try:
        for line in stdout.splitlines():
            if not line:
                continue
            value = parse_json_output(line, label)
            if isinstance(value, dict) and value.get("id") == 2:
                responses.append(value)
    except AcceptanceError as error:
        item["failure"] = {
            "kind": "invalid-json-response",
            "detail": str(error)[:2048],
        }
        append_client_evidence(evidence, item)
        raise
    if len(responses) != 1:
        item["failure"] = {
            "kind": "response-correlation",
            "detail": "session did not return exactly one correlated tool response",
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} MCP session did not return one correlated tool response")
    response = responses[0]
    if "error" in response:
        item["failure"] = {
            "kind": "jsonrpc-error",
            "detail": response["error"],
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} MCP returned a JSON-RPC error")
    result = response.get("result")
    if not isinstance(result, dict) or result.get("isError") is True:
        structured = result.get("structuredContent") if isinstance(result, dict) else None
        if isinstance(structured, dict) and structured.get("answer") == "fault":
            item["failure"] = {
                "kind": "typed-fault",
                "detail": structured,
            }
            append_client_evidence(evidence, item)
            raise AcceptanceError(
                f"{label} MCP typed fault: {canonical_json(structured)[:2048].decode('utf-8', 'replace')}"
            )
        item["failure"] = {
            "kind": "tool-error-result",
            "detail": result,
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} MCP returned an error result")
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        item["failure"] = {
            "kind": "missing-structured-content",
        }
        append_client_evidence(evidence, item)
        raise AcceptanceError(f"{label} MCP response has no structured content")
    append_client_evidence(evidence, item)
    return structured


def operation_state(
    product: dict[str, Any], key: str, package_path: Path
) -> tuple[str, dict[str, Any] | None, dict[str, Any] | None]:
    if product.get("answer") == "product" and product.get("heading") == "index-operation":
        observation = product.get("index_operation")
    elif product.get("answer") == "surface":
        surface = product.get("surface")
        if (
            not isinstance(surface, dict)
            or set(surface) != {"result", "data"}
            or surface.get("result") not in {
                "index-operation-started", "index-operation-status"
            }
        ):
            raise AcceptanceError("durable operation call returned another typed surface result")
        observation = surface["data"]
    else:
        raise AcceptanceError("durable operation call did not return a typed operation observation")
    if not isinstance(observation, dict):
        raise Blocked(
            "this response omits the typed durable index-operation observation"
        )
    observation_state = observation.get("state")
    observation_detail = observation.get("detail")
    if observation_state in {"unknown", "outside-receipt-window"}:
        if not isinstance(observation_detail, dict) or observation_detail.get("operation_key") != key:
            raise AcceptanceError("durable operation absence is not bound to the caller key")
        if observation_state == "outside-receipt-window":
            digest = observation_detail.get("request_digest")
            if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
                raise AcceptanceError("durable operation tombstone omitted its request digest")
        return observation_state, None, observation_detail
    if observation_state != "known" or not isinstance(observation_detail, dict):
        raise AcceptanceError("durable operation observation has an unknown typed state")

    status = observation_detail
    if status.get("operation_key") != key:
        raise AcceptanceError("durable operation status returned another caller key")
    package = status.get("package")
    if (
        not isinstance(package, dict)
        or package != {"kind": "local", "value": str(package_path)}
    ):
        raise AcceptanceError("durable operation status is bound to another package")
    if status.get("execution_intent") != "interactive":
        raise AcceptanceError("durable operation status changed execution intent")
    request_digest = status.get("request_digest")
    if not isinstance(request_digest, str) or re.fullmatch(r"[0-9a-f]{64}", request_digest) is None:
        raise AcceptanceError("durable operation status omitted its canonical request digest")
    state = status.get("state")
    if not isinstance(state, dict) or not isinstance(state.get("state"), str):
        raise AcceptanceError("durable operation status has an invalid typed state")
    state_name = state["state"]
    detail = state.get("detail")
    if state_name == "published":
        if not isinstance(detail, dict) or set(detail) != {
            "request_identity",
            "commit_identity",
            "workspace_root",
            "workspace_sequence",
            "view_root",
            "view_version",
            "view_recipe",
            "revision_cursor",
        }:
            raise AcceptanceError("published operation receipt is incomplete or changed shape")
        for field in (
            "commit_identity",
            "workspace_root",
            "view_root",
            "view_version",
            "view_recipe",
        ):
            if not isinstance(detail[field], str) or re.fullmatch(r"[0-9a-f]{64}", detail[field]) is None:
                raise AcceptanceError(f"published operation receipt has an invalid {field}")
        request_identity = detail["request_identity"]
        if request_identity is not None and (
            not isinstance(request_identity, str)
            or re.fullmatch(r"[0-9a-f]{64}", request_identity) is None
        ):
            raise AcceptanceError("published operation receipt has an invalid request identity")
        if type(detail["workspace_sequence"]) is not int or detail["workspace_sequence"] < 0:
            raise AcceptanceError("published operation receipt has an invalid workspace sequence")
        cursor = detail["revision_cursor"]
        if (
            not isinstance(cursor, list)
            or len(cursor) != 172
            or any(type(byte) is not int or not 0 <= byte <= 255 for byte in cursor)
        ):
            raise AcceptanceError("published operation receipt has an invalid revision cursor")
        return "published", detail, None
    if state_name in {"failed", "unresolved"}:
        if not isinstance(detail, dict) or not isinstance(detail.get("reason"), str) or not isinstance(detail.get("detail"), str):
            raise AcceptanceError("terminal operation failure omitted typed reason or detail")
        return state_name, None, {"reason": detail["reason"], "detail": detail["detail"]}
    if state_name in {"accepted", "active"}:
        return state_name, None, None
    raise AcceptanceError("durable operation status used an unknown typed state")


def mcp_operation_status(
    binary: BinaryIdentity,
    owner_workspace: Path,
    endpoint: Path,
    case: ProjectCase,
    key: str,
    environment: dict[str, str],
    deadline: Deadline,
    evidence: list[dict[str, Any]],
    label: str,
) -> tuple[str, dict[str, Any] | None, dict[str, Any] | None]:
    structured = mcp_call(
        binary,
        owner_workspace,
        endpoint,
        case.path,
        "backend.surface",
        {
            "command": make_surface_command(
                "index-operation-status", key, case.path
            )
        },
        environment,
        deadline,
        evidence,
        label,
    )
    if (
        structured.get("answer") != "surface"
        or not isinstance(structured.get("surface"), dict)
        or structured["surface"].get("result") != "index-operation-status"
    ):
        raise AcceptanceError("MCP operation status returned another result route")
    return operation_state(structured, key, case.path)


def assert_cli_published(
    value: Any, key: str, package_path: Path, label: str
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise AcceptanceError(f"{label} did not return a product object")
    state, receipt, failure = operation_state(value, key, package_path)
    if state != "published" or receipt is None:
        raise AcceptanceError(f"{label} did not prove Published: state={state}, failure={failure}")
    return receipt


def assert_selected_source_frontier(
    value: dict[str, Any],
    case: ProjectCase,
    absolute_inline_maximum: int,
    maximum_files: int,
    label: str,
) -> dict[str, Any]:
    frontier = value.get("selected_source_frontier")
    if not isinstance(frontier, dict):
        raise Blocked(
            f"{label} omitted typed accepted Project membership evidence; "
            "source-candidate counts cannot establish indexed membership"
        )
    if set(frontier) != {
        "package",
        "source_relation_root",
        "source_version",
        "file_count",
    }:
        raise AcceptanceError(f"{label} selected source frontier changed its typed field shape")
    if frontier.get("package") != {"kind": "local", "value": str(case.path)}:
        raise AcceptanceError(f"{label} selected source frontier belongs to another package")

    def byte_array(candidate: Any) -> bool:
        return (
            isinstance(candidate, list)
            and len(candidate) == 32
            and all(type(byte) is int and 0 <= byte <= 255 for byte in candidate)
        )

    if not byte_array(frontier.get("source_relation_root")) or not byte_array(
        frontier.get("source_version")
    ):
        raise AcceptanceError(f"{label} selected source frontier lacks exact source identities")
    count = frontier.get("file_count")
    if type(count) is not int or not 0 <= count <= maximum_files:
        raise AcceptanceError(f"{label} selected source frontier has an invalid accepted file count")
    if count == 0:
        raise Blocked(f"{label} selected source frontier contains no accepted files")
    if case.large and count <= absolute_inline_maximum:
        raise Blocked(
            f"large project {case.project_id} has {count} accepted Project members, "
            "not more than the absolute old inline upper bound "
            f"{absolute_inline_maximum}"
        )
    return frontier


def assert_semantic_versions(
    value: Any,
    case: ProjectCase,
    profile_codes: dict[str, tuple[int, int]],
    absolute_inline_maximum: int,
    maximum_files: int,
    label: str,
) -> dict[str, Any]:
    if (
        not isinstance(value, dict)
        or value.get("answer") != "product"
        or value.get("heading") != "semantic-versions"
    ):
        raise AcceptanceError(f"{label} did not return the typed semantic-versions product")
    rows = value.get("records")
    if not isinstance(rows, list):
        raise AcceptanceError(f"{label} omitted semantic generation records")
    frontier = assert_selected_source_frontier(
        value, case, absolute_inline_maximum, maximum_files, label
    )

    selected_by_profile: dict[str, dict[str, Any]] = {}
    expected_profiles = sorted({symbol["profile"] for symbol in case.symbols})
    unique_codes = {profile_codes[name] for name in expected_profiles}

    def byte_array(candidate: Any, length: int) -> bool:
        return (
            isinstance(candidate, list)
            and len(candidate) == length
            and all(type(byte) is int and 0 <= byte <= 255 for byte in candidate)
        )

    for code in sorted(unique_codes):
        matching = [
            row
            for row in rows
            if isinstance(row, dict) and row.get("compiler_profile") == list(code)
        ]
        eligible: list[dict[str, Any]] = []
        for row in matching:
            tags = row.get("tags")
            history = row.get("history_status")
            if (
                not isinstance(tags, list)
                or any(not isinstance(tag, str) for tag in tags)
                or not isinstance(history, dict)
            ):
                continue
            required_tags = {
                "complete",
                "selected",
                "current source input",
                "derived history published",
            }
            if not required_tags.issubset(set(tags)) or history.get("state") != "published":
                continue
            if not isinstance(row.get("title"), str) or not row["title"].startswith("pkg:"):
                raise AcceptanceError(f"{label} semantic generation lacks its exact compiler PURL")
            if set(history) != {"state", "selection_id", "commit", "reference", "proof"}:
                raise AcceptanceError(f"{label} published history changed its typed field shape")
            if (
                not byte_array(history.get("selection_id"), 32)
                or not byte_array(history.get("commit"), 32)
                or history.get("reference") != "selected-native-v3"
            ):
                raise AcceptanceError(f"{label} published history identity is malformed")
            proof = history.get("proof")
            if not isinstance(proof, dict) or set(proof) != {
                "selection",
                "image",
                "reference_tip",
                "reachable_commit",
                "parent_commits",
                "input_replay_status",
            }:
                raise AcceptanceError(f"{label} published history proof is incomplete")
            selection = proof.get("selection")
            if (
                not isinstance(selection, dict)
                or set(selection)
                != {
                    "namespace",
                    "profile",
                    "source_coordinate",
                    "selection_revision",
                    "selected_root",
                    "closure_id",
                    "catalog_root",
                }
                or selection.get("profile") != list(code)
                or type(selection.get("selection_revision")) is not int
                or selection["selection_revision"] < 0
                or not byte_array(selection.get("namespace"), 16)
                or any(
                    not byte_array(selection.get(name), 32)
                    for name in (
                        "source_coordinate",
                        "selected_root",
                        "closure_id",
                        "catalog_root",
                    )
                )
            ):
                raise AcceptanceError(
                    f"{label} history proof is not bound to the typed profile selection"
                )
            image = proof.get("image")
            if (
                not isinstance(image, dict)
                or set(image)
                != {
                    "artifact_ordinal",
                    "semantic_generation",
                    "manifest_root",
                    "image_identity",
                }
                or type(image.get("artifact_ordinal")) is not int
                or image["artifact_ordinal"] < 0
                or any(
                    not byte_array(image.get(name), 32)
                    for name in ("semantic_generation", "manifest_root", "image_identity")
                )
            ):
                raise AcceptanceError(
                    f"{label} history proof lacks its exact native image identity"
                )
            if (
                not byte_array(proof.get("reference_tip"), 32)
                or not byte_array(proof.get("reachable_commit"), 32)
                or proof["reachable_commit"] != history["commit"]
                or not isinstance(proof.get("parent_commits"), list)
                or len(proof["parent_commits"]) > 2
                or any(not byte_array(parent, 32) for parent in proof["parent_commits"])
                or proof.get("input_replay_status") != "unproven"
            ):
                raise AcceptanceError(
                    f"{label} history proof has invalid lineage or replay status"
                )
            eligible.append(row)
        if len(eligible) != 1:
            raise AcceptanceError(
                f"{label} expected one selected, complete, current, history-published "
                f"generation for profile {list(code)}; found {len(eligible)}"
            )
        if eligible[0].get("selected_source_frontier") != frontier:
            if eligible[0].get("selected_source_frontier") is None:
                raise Blocked(
                    f"{label} selected semantic row omitted its exact Project membership evidence"
                )
            raise AcceptanceError(
                f"{label} selected semantic row is bound to a different Project frontier"
            )
        selected_by_profile["-".join(str(byte) for byte in code)] = eligible[0]
    return {
        "profile_rows": selected_by_profile,
        "selected_source_frontier": frontier,
        "accepted_project_membership_files": frontier["file_count"],
        "identity_sha256": sha256_bytes(canonical_json(selected_by_profile)),
        "freshness_evidence": "typed product tag `current source input`",
        "history_state": "published",
        "input_replay_evidence": "history proof correctly reports `unproven`",
    }


def assert_search_result(value: Any, symbol: dict[str, str], project_root: Path, label: str) -> list[tuple[str, str, str]]:
    if not isinstance(value, dict) or value.get("answer") != "records":
        if isinstance(value, dict) and value.get("answer") == "fault":
            raise AcceptanceError(f"{label} returned a typed search fault")
        raise AcceptanceError(f"{label} did not return a records projection")
    if value.get("readiness") != "ready":
        raise AcceptanceError(f"{label} search readiness was not ready")
    coverage = value.get("coverage")
    if not isinstance(coverage, list) or not coverage:
        raise AcceptanceError(f"{label} search omitted typed coverage")
    if any(not isinstance(row, dict) or row.get("state") != "complete" for row in coverage):
        states = sorted(
            {
                str(row.get("state", "invalid"))
                for row in coverage
                if isinstance(row, dict)
            }
        )
        raise AcceptanceError(f"{label} search coverage was not complete: {','.join(states)}")
    if value.get("more") is not False:
        raise AcceptanceError(
            f"{label} search exceeded the complete {MAX_SEARCH_RESULTS}-result page"
        )
    records = value.get("records")
    if not isinstance(records, list):
        raise AcceptanceError(f"{label} search returned no result list")
    exact = [
        record
        for record in records
        if isinstance(record, dict)
        and isinstance(record.get("identity"), dict)
        and record["identity"].get("name") == symbol["name"]
        and record["identity"].get("path") == symbol["path"]
        and record["identity"].get("project") == str(project_root)
        and record.get("language") == PROFILE_LANGUAGE_VARIANT[symbol["profile"]][1]
    ]
    if not exact:
        raise AcceptanceError(
            f"{label} did not return the exact expected path, name, project, and language"
        )
    return sorted(
        (
            record["identity"]["path"],
            record["identity"]["name"],
            record["language"],
        )
        for record in exact
    )


def expected_source_span(root: Path, path: str, span: dict[str, Any]) -> dict[str, Any]:
    fields = {"file_sha256", "start", "end", "slice_sha256"}
    if not isinstance(span, dict) or set(span) != fields:
        raise Blocked("surface source witness has missing or unknown fields")
    parts = canonical_inventory_relative(path)
    if ignored_source_directory(parts[:-1]):
        raise Blocked("surface witness names excluded output or tooling source")
    selected = root
    for part in parts:
        selected = selected / part
        if selected.is_symlink():
            raise Blocked("surface source witness crosses a symlink")
    for name in ("file_sha256", "slice_sha256"):
        if not isinstance(span[name], str) or re.fullmatch(r"[0-9a-f]{64}", span[name]) is None:
            raise Blocked("surface source witness has an invalid digest")
    start, end = span["start"], span["end"]
    if type(start) is not int or type(end) is not int or not 0 <= start < end <= MAX_SOURCE_FILE_BYTES:
        raise Blocked("surface source witness has an invalid half-open byte interval")
    content = read_bounded_regular(selected, MAX_SOURCE_FILE_BYTES, "surface source witness")
    if sha256_bytes(content) != span["file_sha256"] or end > len(content) or sha256_bytes(content[start:end]) != span["slice_sha256"]:
        raise AcceptanceError("surface source witness does not match exact acquired file bytes")
    try:
        content[:start].decode("utf-8")
        selected_text = content[start:end].decode("utf-8")
        content[end:].decode("utf-8")
    except UnicodeDecodeError as error:
        raise Blocked("surface source span is not on valid UTF-8 boundaries") from error
    return {"path": path, **span, "line": content[:start].count(b"\n") + 1,
            "lines": selected_text.splitlines()}


def validate_surface_contract(value: Any, root: Path, path: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {"kind", "source", "references", "graph"}:
        raise Blocked("surface contract has missing or unknown fields")
    if not isinstance(value["kind"], str) or not value["kind"] or len(value["kind"]) > 64:
        raise Blocked("surface contract declaration kind is invalid")
    expected_source_span(root, path, value["source"])
    for field in ("references", "graph"):
        if not isinstance(value[field], list) or len(value[field]) > MAX_SEARCH_RESULTS:
            raise Blocked("surface obligations exceed their bound")
        if len({canonical_json(row) for row in value[field]}) != len(value[field]):
            raise Blocked("surface obligations repeat a source site or edge")
    for row in value["references"]:
        if not isinstance(row, dict) or set(row) != {"path", "start", "end", "file_sha256", "slice_sha256", "relation", "confidence"}:
            raise Blocked("reference obligation has missing or unknown fields")
        if row["relation"] not in {"calls", "method-call", "type-reference", "reads", "writes", "imports", "implements", "overrides", "reexports", "inherits", "documents"} or row["confidence"] not in {"syntactic", "heuristic", "indexed", "imported", "compiler"}:
            raise Blocked("reference obligation uses an unknown semantic relation or authority")
        expected_source_span(root, row["path"], {key: row[key] for key in ("file_sha256", "start", "end", "slice_sha256")})
    for row in value["graph"]:
        if not isinstance(row, dict) or set(row) != {"label", "path", "name"} or any(not isinstance(row[key], str) or not row[key] or len(row[key].encode()) > 4096 for key in row):
            raise Blocked("graph obligation has invalid closed fields")
        canonical_inventory_relative(row["path"])
    return value


def dto_payload(value: dict[str, Any]) -> dict[str, Any]:
    return {key: item for key, item in value.items() if key not in {"budget", "detail"}}


def assert_surface_identity(value: Any, identity: dict[str, Any], label: str) -> None:
    if not isinstance(value, dict) or value.get("answer") != "page" or value.get("identity") != identity:
        raise AcceptanceError(f"{label} lost the exact resolved declaration identity")


def assert_source_body(value: Any, identity: dict[str, Any], witness: dict[str, Any], label: str) -> None:
    assert_surface_identity(value, identity, label)
    source = value.get("source")
    expected = {"path": witness["path"], "line": witness["line"], "lines": witness["lines"], "extent": "complete"}
    if source != expected or value.get("source_fault") is not None:
        raise AcceptanceError(f"{label} did not retain the exact complete declaration body")


def assert_reference_obligations(raw: Any, coordinate: str, obligations: list[dict[str, Any]]) -> dict[str, Any]:
    if not isinstance(raw, dict) or raw.get("answer") != "surface" or not isinstance(raw.get("surface"), dict) or raw["surface"].get("result") != "references":
        raise AcceptanceError("raw references did not return the closed references route")
    data = raw["surface"].get("data")
    if not isinstance(data, dict) or data.get("target") != coordinate or not isinstance(data.get("references"), list):
        raise AcceptanceError("raw references lost the requested exact target")
    rows = data["references"]
    endpoints = []

    def resolved_endpoint(target: Any) -> bool:
        def byte_array(value: Any, size: int) -> bool:
            return isinstance(value, list) and len(value) == size and all(type(byte) is int and 0 <= byte <= 255 for byte in value)
        if not isinstance(target, dict):
            return False
        if target.get("scope") == "local":
            if set(target) != {"scope", "declaration"}:
                return False
        elif target.get("scope") == "stable":
            if set(target) != {"scope", "fragment", "declaration"} or not byte_array(target["fragment"], 32):
                return False
        else:
            return False
        declaration = target.get("declaration")
        return isinstance(declaration, dict) and set(declaration) == {"family", "variant"} and all(byte_array(declaration[key], 16) for key in declaration)

    for expected in obligations:
        matches = [row for row in rows if isinstance(row, dict)
                   and row.get("relation") == expected["relation"]
                   and isinstance(row.get("evidence"), dict)
                   and row["evidence"].get("confidence") == expected["confidence"]
                   and row["evidence"].get("source") == {
                       "file": expected["path"], "start": expected["start"], "end": expected["end"]}]
        if not matches or any(not resolved_endpoint(row.get("target")) for row in matches):
            raise AcceptanceError("references omitted a source-bound required semantic use")
        endpoints.append({"source": {key: expected[key] for key in ("path", "start", "end")},
                          "semantic_targets": [json.loads(encoded) for encoded in sorted({
                              canonical_json(row["target"]) for row in matches})]})
    return {"required_sites": len(obligations), "returned_sites": len(rows),
            "coverage_claim": "required-source-obligations; not the complete reference universe",
            "required_endpoint_identities": endpoints,
            "typed_reference_sha256": sha256_bytes(canonical_json(data))}


def assert_graph_obligations(value: Any, identity: dict[str, Any], obligations: list[dict[str, str]]) -> dict[str, Any]:
    assert_surface_identity(value, identity, "graph")
    groups = value.get("relations", [])
    if not isinstance(groups, list):
        raise AcceptanceError("graph omitted its structured relation groups")
    for expected in obligations:
        matches = [target for group in groups if isinstance(group, dict) and group.get("label") == expected["label"]
                   for target in group.get("relations", []) if isinstance(target, dict)
                   and target.get("path") == expected["path"] and target.get("name") == expected["name"]
                   and target.get("project") == identity["project"]]
        if not matches:
            raise AcceptanceError("graph omitted a source-backed required declaration edge")
    return {"required_edges": len(obligations), "coverage_claim": "required-source-obligations; not the complete graph universe"}


def run_surface_contracts(case: ProjectCase, binaries: dict[str, BinaryIdentity], workspace: Path,
                          endpoint: Path, environment: dict[str, str], deadline: Deadline,
                          evidence: list[dict[str, Any]], phase: str,
                          previous: list[dict[str, Any]] | None = None) -> list[dict[str, Any]]:
    results = []
    prior = {(row["profile"], row["path"], row["name"]): row for row in (previous or [])}

    def pair(command: list[str], tool: str, arguments: dict[str, Any], label: str) -> tuple:
        cli = cli_call(binaries["backend-cli"], workspace, endpoint, case.path, command,
                       environment, deadline, evidence, "cli-" + label)
        mcp = mcp_call(binaries["backend-mcp"], workspace, endpoint, case.path, tool,
                       {**arguments, "detail": "full"}, environment, deadline, evidence, "mcp-" + label)
        if not isinstance(cli, dict) or not isinstance(mcp, dict) or dto_payload(cli) != dto_payload(mcp):
            raise AcceptanceError(f"{label} CLI/MCP structured projections differ")
        return cli, {"cli_dto_sha256": sha256_bytes(canonical_json(dto_payload(cli))),
                     "mcp_dto_sha256": sha256_bytes(canonical_json(dto_payload(mcp))), "parity": True}

    for symbol in case.symbols:
        contract = symbol.get("surface_contract")
        if contract is None:
            continue
        validate_surface_contract(contract, case.path, symbol["path"])
        label = f"{phase}-{case.project_id}-{symbol['profile']}-{symbol['name']}"
        search, search_proof = pair(["--limit", str(MAX_SEARCH_RESULTS), "search", symbol["name"]],
            "backend.search", {"query": symbol["name"], "limit": MAX_SEARCH_RESULTS}, label + "-search")
        assert_search_result(search, symbol, case.path, label + "-search")
        resolved, resolve_proof = pair(["--limit", str(MAX_SEARCH_RESULTS), "resolve", symbol["name"]],
            "backend.resolve", {"query": symbol["name"], "limit": MAX_SEARCH_RESULTS}, label + "-resolve")
        records = resolved.get("records")
        if resolved.get("answer") != "records" or not isinstance(records, list) or resolved.get("more") is not False:
            raise AcceptanceError("resolve did not return a complete bounded declaration selection")
        candidates = [record["identity"] for record in records if isinstance(record, dict)
            and isinstance(record.get("identity"), dict)
            and record["identity"].get("path") == symbol["path"]
            and record["identity"].get("project") == str(case.path)
            and record["identity"].get("name") == symbol["name"]
            and record.get("language") == PROFILE_LANGUAGE_VARIANT[symbol["profile"]][1]]
        if len(candidates) != 1:
            raise AcceptanceError("resolve could not select one exact source declaration/identity plane")
        identity = candidates[0]
        coordinate = identity.get("coordinate")
        if not isinstance(coordinate, str) or not coordinate or not isinstance(identity.get("key"), str):
            raise AcceptanceError("resolve omitted the exact coordinate or public key abbreviation")
        old = prior.get((symbol["profile"], symbol["path"], symbol["name"]))
        if previous is not None and (old is None or old["resolved_identity"] != identity):
            raise AcceptanceError("cold resolve changed the retained declaration identity")
        # Intentionally reuse the OLD exact coordinate after owner restart.
        coordinate = old["coordinate"] if old is not None else coordinate
        projections = {"search": search_proof, "resolve": resolve_proof}
        document, projections["show"] = pair(["show", coordinate], "backend.document",
            {"coordinate": coordinate}, label + "-show")
        assert_surface_identity(document, identity, "show")
        if document.get("kind") != contract["kind"]:
            raise AcceptanceError("show did not preserve the source-backed semantic declaration kind")
        source, projections["source"] = pair(["source", coordinate], "backend.source",
            {"coordinate": coordinate}, label + "-source")
        source_witness = expected_source_span(case.path, symbol["path"], contract["source"])
        assert_source_body(source, identity, source_witness, "source")
        references, projections["references"] = pair(["references", coordinate], "backend.references",
            {"coordinate": coordinate}, label + "-references")
        if references.get("answer") != "product" or references.get("heading") != "references":
            raise AcceptanceError("named references did not return their typed product route")
        raw_references = mcp_call(binaries["backend-mcp"], workspace, endpoint, case.path, "backend.surface",
            {"command": {"operation": "references", "target": coordinate}, "detail": "full"},
            environment, deadline, evidence, "mcp-" + label + "-typed-references")
        reference_proof = assert_reference_obligations(raw_references, coordinate, contract["references"])
        graph, projections["graph"] = pair(["graph", coordinate], "backend.graph",
            {"coordinate": coordinate}, label + "-graph")
        graph_proof = assert_graph_obligations(graph, identity, contract["graph"])
        raw_read = mcp_call(binaries["backend-mcp"], workspace, endpoint, case.path, "backend.surface",
            {"command": {"operation": "read", "locators": [coordinate]}, "detail": "full"},
            environment, deadline, evidence, "mcp-" + label + "-row-identity")
        surface = raw_read.get("surface") if isinstance(raw_read, dict) else None
        data = surface.get("data") if isinstance(surface, dict) else None
        if not isinstance(raw_read, dict) or raw_read.get("answer") != "surface" or not isinstance(surface, dict) or surface.get("result") != "read" or not isinstance(data, list) or len(data) != 1 or not isinstance(data[0], dict) or data[0].get("label") != coordinate:
            raise AcceptanceError("read omitted its exact requested declaration row")
        stable_id = data[0].get("stable_id")
        if not isinstance(stable_id, list) or len(stable_id) != 32 or any(type(byte) is not int or not 0 <= byte <= 255 for byte in stable_id):
            raise AcceptanceError("read omitted the complete stable row digest")
        if old is not None and (old["row_stable_id"] != stable_id or old["projections"] != projections or old["references"] != reference_proof or old["graph"] != graph_proof):
            raise AcceptanceError("cold old-coordinate surfaces changed their retained row or semantic evidence")
        results.append({"project_id": case.project_id, "profile": symbol["profile"],
            "path": symbol["path"], "name": symbol["name"], "coordinate": coordinate,
            "resolved_identity": identity, "public_key_abbreviation": identity["key"],
            "row_stable_id": stable_id, "identity_planes": "public display abbreviation; row digest; references retain separate semantic endpoint identities",
            "contract_sha256": sha256_bytes(canonical_json(contract)),
            "source": {key: source_witness[key] for key in ("path", "file_sha256", "start", "end", "slice_sha256")},
            "projections": projections, "references": reference_proof, "graph": graph_proof,
            "cold_old_coordinate_verified": old is not None})
    return results


def write_json_atomic(path: Path, value: Any) -> None:
    encoded = canonical_json(value) + b"\n"
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except Exception:
        try:
            temporary.unlink()
        except OSError:
            pass
        raise


def safe_endpoint() -> tuple[Path, Path]:
    parent = Path("/tmp") if Path("/tmp").is_dir() else Path(os.getenv("TMPDIR", "/tmp"))
    short = parent / f"nidx-{uuid.uuid4().hex[:12]}"
    if len(str(short / "s").encode()) > 88:
        raise Blocked("temporary endpoint parent leaves no room under the Unix socket path bound")
    try:
        short.mkdir(mode=0o700)
    except OSError as error:
        raise Blocked("a private short temporary endpoint directory could not be created") from error
    endpoint = short / "s"
    if endpoint.exists() or endpoint.is_symlink():
        raise AcceptanceError("random temporary endpoint path was unexpectedly occupied")
    return endpoint, short


def owner_arguments(workspace: Path, endpoint: Path, secret: Path) -> list[str]:
    return [
        "--endpoint",
        str(endpoint),
        "--workspace",
        str(workspace),
        "--profile",
        "builtin",
        "--authority-secret-file",
        str(secret),
        "--max-frame",
        "1048576",
        "--timeout-ms",
        "180000",
        "--idle-timeout-ms",
        "0",
        "--registry-offline",
        "--advisory-offline",
        "--forge-offline",
    ]


def wait_endpoint(owner: Owner, endpoint: Path, deadline: Deadline) -> None:
    while True:
        owner.require_running("while waiting for its endpoint")
        if endpoint.exists():
            try:
                if stat.S_ISSOCK(endpoint.lstat().st_mode):
                    return
            except OSError:
                pass
            raise AcceptanceError("the owned endpoint path is not a Unix socket")
        deadline.check("owner endpoint readiness")
        time.sleep(min(0.1, max(0.0, deadline.remaining())))


def verify_inputs_unchanged(
    source: dict[str, Any],
    binaries: dict[str, BinaryIdentity],
    build_manifest_path: Path,
    build_manifest_sha: str,
    corpus_manifest_path: Path,
    corpus_manifest_sha: str,
    snapshot_path: Path,
    snapshot_sha: str,
    cases: list[ProjectCase],
    before_census: dict[str, Census],
    deadline: Deadline,
) -> dict[str, Census]:
    deadline.check("source provenance revalidation")
    lock_bytes = read_bounded_regular(source["path"] / "Cargo.lock", 16 * 1024 * 1024, "Cargo.lock")
    if sha256_bytes(lock_bytes) != source["cargo_lock_sha256"]:
        raise AcceptanceError("Cargo.lock changed during indexing acceptance")
    if sha256_bytes(read_bounded_regular(build_manifest_path, MAX_BUILD_MANIFEST_BYTES, "runtime build manifest")) != build_manifest_sha:
        raise AcceptanceError("runtime build manifest changed during indexing acceptance")
    if sha256_bytes(read_bounded_regular(corpus_manifest_path, MAX_CORPUS_MANIFEST_BYTES, "real corpus manifest")) != corpus_manifest_sha:
        raise AcceptanceError("real corpus manifest changed during indexing acceptance")
    if sha256_bytes(read_bounded_regular(snapshot_path, MAX_COMPILER_SNAPSHOT_BYTES, "closed compiler-host snapshot")) != snapshot_sha:
        raise AcceptanceError("closed compiler-host snapshot changed during indexing acceptance")
    revision = git_output(source["git"], source["path"], ["rev-parse", "HEAD"], "commit")
    tree = git_output(source["git"], source["path"], ["rev-parse", "HEAD^{tree}"], "tree")
    dirty = git_output(
        source["git"],
        source["path"],
        ["status", "--porcelain", "--untracked-files=all"],
        "working-tree cleanliness",
    )
    if revision != source["commit"] or tree != source["tree"] or dirty:
        raise AcceptanceError("verified source checkout changed during indexing acceptance")
    for name, original in binaries.items():
        current = executable_identity(name, original.path)
        if (current.sha256, current.size, current.device, current.inode, current.architecture) != (
            original.sha256,
            original.size,
            original.device,
            original.inode,
            original.architecture,
        ):
            raise AcceptanceError(f"verified binary {name} changed during acceptance")
    after: dict[str, Census] = {}
    for case in cases:
        census = project_census(case.path, deadline)
        if case.acquisition is not None:
            verify_acquired_source_inventory(case, corpus_manifest_sha, deadline)
        before = before_census[case.project_id]
        if (
            census.sha256 != before.sha256
            or census.files != before.files
            or census.bytes != before.bytes
            or (census.root_device, census.root_inode) != (before.root_device, before.root_inode)
        ):
            raise AcceptanceError(f"real project {case.project_id} changed during acceptance")
        after[case.project_id] = census
    return after


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Exercise durable indexing against exact binaries and real local corpora."
    )
    parser.add_argument("--source-checkout", type=Path, required=True)
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--locald", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--mcp", type=Path, required=True)
    parser.add_argument("--compiler-snapshot", type=Path, required=True)
    parser.add_argument("--setup-mode", choices=["configured", "stock"], default="configured",
                        help="stock omits compiler environment injection; snapshot remains a tooling witness")
    parser.add_argument("--corpus-manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--language", choices=sorted(LANGUAGE_CORPUS_PROFILES),
        help="explicit language corpus shard; default retains all-profile and large-project gates",
    )
    parser.add_argument(
        "--deadline-seconds",
        type=int,
        default=DEFAULT_DEADLINE_SECONDS,
        help=f"one absolute deadline for the entire run (1..{MAX_DEADLINE_SECONDS})",
    )
    args = parser.parse_args()
    if not 1 <= args.deadline_seconds <= MAX_DEADLINE_SECONDS:
        parser.error(f"--deadline-seconds must be within 1..{MAX_DEADLINE_SECONDS}")
    return args


def preflight_output_disjoint_from_corpus(output: Path, corpus_manifest: Path) -> None:
    """Reject an evidence directory inside a corpus before creating it."""
    raw = read_bounded_regular(
        corpus_manifest,
        MAX_CORPUS_MANIFEST_BYTES,
        "real corpus manifest",
    )
    manifest = json_no_duplicate_keys(raw, "real corpus manifest")
    projects = manifest.get("projects") if isinstance(manifest, dict) else None
    if not isinstance(projects, list):
        return
    output_root = output.resolve(strict=False)
    for item in projects:
        raw_path = item.get("path") if isinstance(item, dict) else None
        if not isinstance(raw_path, str) or not os.path.isabs(raw_path):
            continue
        project = Path(raw_path)
        try:
            root = project.resolve(strict=True)
        except OSError:
            continue
        if (
            output_root == root
            or output_root.is_relative_to(root)
            or root.is_relative_to(output_root)
        ):
            raise Blocked("fresh evidence output and real project roots must be disjoint")


def preflight_output_disjoint_from_source(output: Path, source_checkout: Path) -> None:
    """Avoid creating evidence inside or above the source checkout."""
    if source_checkout.is_symlink():
        raise Blocked("source checkout must not be selected through a symlink")
    try:
        source = source_checkout.resolve(strict=True)
    except OSError as error:
        raise Blocked("source checkout is unavailable") from error
    if not source.is_dir():
        raise Blocked("source checkout is not a directory")
    output_root = output.resolve(strict=False)
    if (
        output_root == source
        or output_root.is_relative_to(source)
        or source.is_relative_to(output_root)
    ):
        raise Blocked("fresh evidence output and source checkout must be disjoint")


def run_acceptance(args: argparse.Namespace, output: Path) -> dict[str, Any]:
    if os.name != "posix":
        raise Blocked("this runner currently supports Unix local owners and Unix native paths")
    if not hasattr(os, "killpg") or not hasattr(signal, "SIGKILL"):
        raise Blocked("this runner requires exact Unix process-group cleanup")

    deadline = Deadline(args.deadline_seconds)
    evidence: ClientEvidence = ClientEvidence()
    started_at = utc_now()
    result: dict[str, Any] = {
        "schema": RESULT_SCHEMA,
        "scope": {
            "kind": "all-profiles" if args.language is None else "language-corpus",
            "language": args.language,
            "large_project_required": args.language is None,
        },
        "status": "RUNNING",
        "started_at": started_at,
        "finished_at": None,
        "source": None,
        "executables": {},
        "input_sha256": {},
        "closed_compiler_roles": [],
        "required_toolchain_roles_by_profile": {},
        "environment_witnesses": {},
        "limits": {
            "absolute_deadline_seconds": args.deadline_seconds,
            "maximum_projects": MAX_PROJECTS,
            "maximum_symbols": MAX_SYMBOLS,
            "maximum_recognized_source_candidates_per_project": MAX_SOURCE_CANDIDATES,
            "maximum_one_source_file_bytes": MAX_SOURCE_FILE_BYTES,
            "maximum_total_source_candidate_bytes": MAX_SOURCE_BYTES_TOTAL,
            "large_project_minimum_recognized_source_candidates": None,
            "maximum_search_results_per_query": MAX_SEARCH_RESULTS,
            "maximum_client_stdout_bytes": MAX_CLIENT_STDOUT_BYTES,
            "maximum_client_stderr_bytes": MAX_CLIENT_STDERR_BYTES,
            "maximum_client_evidence_bytes": MAX_CLIENT_EVIDENCE_BYTES,
            "maximum_client_evidence_capture_bytes_per_stream": MAX_CLIENT_CAPTURE_PREFIX_BYTES
            + MAX_CLIENT_CAPTURE_TAIL_BYTES,
            "maximum_owner_log_retained_bytes_per_stream": MAX_CAPTURE_PREFIX_BYTES
            + MAX_CAPTURE_TAIL_BYTES,
            "maximum_client_calls": MAX_CLIENT_EVIDENCE_ITEMS,
        },
        "projects": [],
        "operations": [],
        "owner_runs": [],
        "client_calls": evidence,
        "native_gui": {
            "status": "NOT_EVALUATED",
            "note": "Native GUI acceptance requires its separate same-owner handoff evidence.",
        },
        "candidate_count_semantics": (
            "recognized source candidates from an independent extension/ignore traversal; "
            "this is not an indexed-file count"
        ),
        "membership_count_semantics": (
            "accepted files from the exact checked Project membership attached to the selected "
            "semantic query; source candidates are not substituted for this count"
        ),
        "source_capacity": None,
        "source_capacity_note": (
            "source candidate bytes/count are independent inventory measurements, not compiler "
            "workspace build charge or an indexed-file count"
        ),
    }
    write_json_atomic(output / "run.json", result)
    source = capture_source(args.source_checkout, args.build_manifest)
    deadline.check("source and build-receipt provenance")
    result["source"] = {
        "commit": source["commit"],
        "tree": source["tree"],
        "cargo_lock_sha256": source["cargo_lock_sha256"],
        "build_manifest_sha256": source["build_manifest_sha256"],
        "root_receipt_sha256": source["root_receipt_sha256"],
        "build_manifest_schema": BUILD_SCHEMA,
    }
    write_json_atomic(output / "run.json", result)
    binaries = verify_build_executables(
        source,
        {
            "backend-locald": args.locald,
            "backend-cli": args.cli,
            "backend-mcp": args.mcp,
        },
    )
    deadline.check("runtime executable provenance")
    compiler_key, policy_key, policy = capture_runtime_policy(source["path"])
    source_capacity = source_capacity_contract(source["path"])
    result["source_capacity"] = source_capacity
    result["limits"]["large_project_minimum_recognized_source_candidates"] = source_capacity[
        "large_project_candidate_census_minimum"
    ]
    write_json_atomic(output / "run.json", result)
    extension_languages, role_names = parse_language_contract(source["path"])
    cases, corpus_bytes = validate_corpus_manifest(
        args.corpus_manifest,
        output,
        extension_languages,
        source_capacity["large_project_candidate_census_minimum"],
        args.language,
    )
    deadline.check("real corpus admission")
    compiler_snapshot, selected_tools, snapshot_sha = parse_closed_snapshot(
        args.compiler_snapshot, compiler_key, source["path"], role_names
    )
    toolchain_roles = validate_snapshot_toolchains(
        selected_tools, cases, extension_languages
    )
    deadline.check("compiler-host admission")

    source_dir = source["path"]
    lock_before = sha256_bytes(
        read_bounded_regular(source_dir / "Cargo.lock", 16 * 1024 * 1024, "Cargo.lock")
    )
    if lock_before != source["cargo_lock_sha256"]:
        raise AcceptanceError("Cargo.lock changed after provenance capture")
    corpus_manifest_sha = sha256_bytes(corpus_bytes)
    package_provenance = {
        case.project_id: verify_acquired_source_inventory(case, corpus_manifest_sha, deadline)
        for case in cases if case.acquisition is not None
    }
    before_census = {
        case.project_id: project_census(case.path, deadline) for case in cases
    }
    profile_codes = parse_semantic_profile_codes(source["path"])
    result["executables"] = {
        name: {
            "path": str(identity.path),
            "sha256": identity.sha256,
            "bytes": identity.size,
            "architecture": identity.architecture,
        }
        for name, identity in binaries.items()
    }
    result["input_sha256"] = {
        "real_corpus_manifest": corpus_manifest_sha,
        "closed_compiler_snapshot": snapshot_sha,
    }
    result["closed_compiler_roles"] = sorted(selected_tools)
    result["required_toolchain_roles_by_profile"] = toolchain_roles
    result["source_capacity"] = source_capacity
    result["projects"] = [
        {
            "id": case.project_id,
            "root_identity_sha256": sha256_bytes(str(case.path).encode()),
            "large": case.large,
            "recognized_source_candidates": before_census[case.project_id].files,
            "recognized_source_bytes": before_census[case.project_id].bytes,
            "recognized_source_candidates_by_extension": before_census[
                case.project_id
            ].extension_counts,
            "expected_profiles": sorted(
                {symbol["profile"] for symbol in case.symbols}
            ),
            "expected_profile_codes": {
                profile: list(profile_codes[profile])
                for profile in sorted({symbol["profile"] for symbol in case.symbols})
            },
            "recognized_source_tree_sha256_before_restart": before_census[
                case.project_id
            ].sha256,
            "accepted_project_membership_files_before_restart": None,
            "accepted_project_membership_files_after_restart": None,
            "symbols": list(case.symbols),
            **({"package_provenance": package_provenance.get(case.project_id, {
                "declared": case.package,
                "verification": "declared-by-corpus-manifest; artifact bytes not independently verified by this runner",
                "corpus_manifest_sha256": corpus_manifest_sha,
            })} if case.package is not None else {}),
        }
        for case in cases
    ]
    if any(
        census.files > source_capacity["project_frontier_file_conservative_maximum"]
        for census in before_census.values()
    ):
        result["source_capacity_warning"] = (
            "at least one recognized source-candidate census exceeds the source project-row "
            "frontier estimate; this is a capacity warning, not proof of indexed rows"
        )
    write_json_atomic(output / "run.json", result)
    verify_symbol_candidates(cases, before_census)

    owner_root = output / "owner"
    workspace = owner_root / "workspace"
    temp_root = owner_root / "tmp"
    owner_root.mkdir(mode=0o700)
    workspace.mkdir(mode=0o700)
    temp_root.mkdir(mode=0o700)
    secret_path = owner_root / "authority.secret"
    secret_fd = os.open(secret_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(secret_fd, "wb") as secret:
        secret.write(os.urandom(32))
        secret.flush()
        os.fsync(secret.fileno())
    owner_environment, owner_env_hash = minimal_environment(
        compiler_snapshot=compiler_snapshot,
        compiler_key=compiler_key,
        setup_mode=args.setup_mode,
        policy=policy,
        policy_key=policy_key,
        temp_root=temp_root,
    )
    client_environment, client_env_hash = minimal_environment(
        client=True, temp_root=temp_root
    )
    result["environment_witnesses"] = {
        "owner_allowlisted_environment_sha256": owner_env_hash,
        "client_allowlisted_environment_sha256": client_env_hash,
    }
    result["runtime_setup"] = runtime_setup_receipt(
        args.setup_mode, owner_environment, owner_env_hash, compiler_key, role_names)
    result["runtime_tooling"] = {
        "path_contains_nix_store": "/nix/store/" in owner_environment.get("PATH", ""),
        "owner_environment_keys": sorted(owner_environment),
        "compiler_snapshot_role": "configured-authority" if args.setup_mode == "configured" else "tooling-witness-only",
        "selected_tool_authority": None,
        "selection_evidence": "runtime selected-tool identity is not exposed by this product route",
        "installation_acceptance": False,
    }
    result["network_policy"] = {
        "registry_network_allowed": False,
        "discovery_network_allowed": False,
        "advisory_network_allowed": False,
        "advisory_refresh_enabled": False,
        "registry_cache_max_age_millis": None,
    }
    write_json_atomic(output / "run.json", result)

    owner: Owner | None = None
    endpoint: Path | None = None
    endpoint_directory: Path | None = None
    try:
        endpoint, endpoint_directory = safe_endpoint()
        operation_keys = make_operation_keys(cases, output)
        owner = Owner(
            binaries["backend-locald"],
            owner_arguments(workspace, endpoint, secret_path),
            owner_environment,
            deadline,
            "initial-fresh-workspace",
        )
        owner.start()
        wait_endpoint(owner, endpoint, deadline)

        for case in cases:
            owner.require_running(f"before indexing {case.project_id}")
            key = operation_keys[case.project_id]
            command = make_surface_command("index-operation-start", key, case.path)
            start_value = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                ["surface", json.dumps(command, separators=(",", ":"))],
                client_environment,
                deadline,
                evidence,
                f"start-{case.project_id}",
            )
            start_state, start_receipt, start_failure = operation_state(
                start_value, key, case.path
            )
            if start_state in {"failed", "unresolved", "unknown", "outside-receipt-window"}:
                raise AcceptanceError(
                    f"durable start for {case.project_id} entered terminal state "
                    f"{start_state}: {start_failure}"
                )
            state, status_receipt, failure = mcp_operation_status(
                binaries["backend-mcp"],
                workspace,
                endpoint,
                case,
                key,
                client_environment,
                deadline,
                evidence,
                f"status-start-{case.project_id}",
            )
            if state in {"failed", "unresolved", "unknown", "outside-receipt-window"}:
                raise AcceptanceError(
                    f"durable status for {case.project_id} entered terminal state "
                    f"{state}: {failure}"
                )
            if start_receipt is not None and status_receipt != start_receipt:
                raise AcceptanceError("CLI and MCP start acknowledgements returned different receipts")

        receipts: dict[str, dict[str, Any]] = {}
        while len(receipts) < len(cases):
            deadline.check("durable publication polling")
            for case in cases:
                if case.project_id in receipts:
                    continue
                owner.require_running(f"during indexing {case.project_id}")
                state, receipt, failure = mcp_operation_status(
                    binaries["backend-mcp"],
                    workspace,
                    endpoint,
                    case,
                    operation_keys[case.project_id],
                    client_environment,
                    deadline,
                    evidence,
                    f"poll-{case.project_id}",
                )
                if state == "published" and receipt is not None:
                    receipts[case.project_id] = receipt
                elif state in {"failed", "unresolved", "unknown", "outside-receipt-window"}:
                    raise AcceptanceError(
                        f"durable operation {case.project_id} ended as {state}: {failure}"
                    )
            if len(receipts) < len(cases):
                time.sleep(min(POLL_SECONDS, max(0.0, deadline.remaining())))

        for case in cases:
            owner.require_running(f"before pre-restart status {case.project_id}")
            cli_value = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                [
                    "surface",
                    json.dumps(
                        make_surface_command(
                            "index-operation-status",
                            operation_keys[case.project_id],
                            case.path,
                        ),
                        separators=(",", ":"),
                    ),
                ],
                client_environment,
                deadline,
                evidence,
                f"cli-pre-restart-status-{case.project_id}",
            )
            cli_receipt = assert_cli_published(
                cli_value,
                operation_keys[case.project_id],
                case.path,
                f"pre-restart status {case.project_id}",
            )
            if cli_receipt != receipts[case.project_id]:
                raise AcceptanceError("CLI and MCP pre-restart receipts differ")

        semantic_before: dict[str, dict[str, Any]] = {}
        project_results = {project["id"]: project for project in result["projects"]}
        for case in cases:
            owner.require_running(f"before semantic profile checks for {case.project_id}")
            cli_semantic = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                ["semantic-versions", str(case.path)],
                client_environment,
                deadline,
                evidence,
                f"cli-semantic-versions-before-{case.project_id}",
            )
            cli_semantic_identity = assert_semantic_versions(
                cli_semantic,
                case,
                profile_codes,
                source_capacity["project_frontier_absolute_inline_maximum"],
                source_capacity["project_file_record_maximum"],
                f"CLI before restart {case.project_id}",
            )
            mcp_semantic = mcp_call(
                binaries["backend-mcp"],
                workspace,
                endpoint,
                case.path,
                "backend.semantic_versions",
                {"package": str(case.path), "detail": "full"},
                client_environment,
                deadline,
                evidence,
                f"mcp-semantic-versions-before-{case.project_id}",
            )
            mcp_semantic_identity = assert_semantic_versions(
                mcp_semantic,
                case,
                profile_codes,
                source_capacity["project_frontier_absolute_inline_maximum"],
                source_capacity["project_file_record_maximum"],
                f"MCP before restart {case.project_id}",
            )
            if cli_semantic_identity != mcp_semantic_identity:
                raise AcceptanceError(
                    f"CLI and MCP semantic profile/history evidence differs for {case.project_id}"
                )
            semantic_before[case.project_id] = cli_semantic_identity
            project_results[case.project_id][
                "accepted_project_membership_files_before_restart"
            ] = cli_semantic_identity["accepted_project_membership_files"]
            project_results[case.project_id]["selected_source_frontier_before_restart"] = (
                cli_semantic_identity["selected_source_frontier"]
            )
        result["semantic_versions_before_restart"] = semantic_before
        result["surface_contracts_before_restart"] = {}
        for case in cases:
            result["surface_contracts_before_restart"][case.project_id] = run_surface_contracts(
                case, binaries, workspace, endpoint, client_environment, deadline, evidence, "warm")
            write_json_atomic(output / "run.json", result)
        write_json_atomic(output / "run.json", result)

        before_census = verify_inputs_unchanged(
            source,
            binaries,
            args.build_manifest,
            source["build_manifest_sha256"],
            args.corpus_manifest,
            corpus_manifest_sha,
            args.compiler_snapshot,
            snapshot_sha,
            cases,
            before_census,
            deadline,
        )
        for project in result["projects"]:
            census = before_census[project["id"]]
            project["recognized_source_tree_sha256_before_restart"] = census.sha256
        write_json_atomic(output / "run.json", result)

        stop_owner_and_record(owner, result, output)
        owner = None
        if endpoint.exists() or endpoint.is_symlink():
            try:
                endpoint_info = endpoint.lstat()
            except OSError:
                endpoint_info = None
            if endpoint_info is not None and not stat.S_ISSOCK(endpoint_info.st_mode):
                raise AcceptanceError("owned endpoint path was replaced by a non-socket")
            if endpoint_info is not None:
                endpoint.unlink()
        if endpoint.exists():
            raise AcceptanceError("old endpoint remained after exact owner shutdown")

        second = Owner(
            binaries["backend-locald"],
            owner_arguments(workspace, endpoint, secret_path),
            owner_environment,
            deadline,
            "cold-restart-same-workspace",
        )
        owner = second
        owner.start()
        wait_endpoint(owner, endpoint, deadline)

        for case in cases:
            owner.require_running(f"after cold restart for {case.project_id}")
            key = operation_keys[case.project_id]
            state, receipt, failure = mcp_operation_status(
                binaries["backend-mcp"],
                workspace,
                endpoint,
                case,
                key,
                client_environment,
                deadline,
                evidence,
                f"cold-status-{case.project_id}",
            )
            if state != "published" or receipt is None:
                raise AcceptanceError(
                    f"cold replay lost the Published result for {case.project_id}: "
                    f"state={state}, failure={failure}"
                )
            if receipt != receipts[case.project_id]:
                raise AcceptanceError(
                    f"cold replay changed the publication receipt for {case.project_id}"
                )
            cli_value = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                [
                    "surface",
                    json.dumps(
                        make_surface_command("index-operation-status", key, case.path),
                        separators=(",", ":"),
                    ),
                ],
                client_environment,
                deadline,
                evidence,
                f"cli-cold-status-{case.project_id}",
            )
            cli_receipt = assert_cli_published(
                cli_value, key, case.path, f"cold CLI status {case.project_id}"
            )
            if cli_receipt != receipt:
                raise AcceptanceError("cold CLI and MCP publication receipts differ")

            # Replaying the same persisted key and exact request must return
            # the original publication evidence after a cold owner restart.
            replay_value = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                [
                    "surface",
                    json.dumps(
                        make_surface_command("index-operation-start", key, case.path),
                        separators=(",", ":"),
                    ),
                ],
                client_environment,
                deadline,
                evidence,
                f"cold-idempotent-replay-{case.project_id}",
            )
            replay_start_state, replay_start_receipt, replay_start_failure = operation_state(
                replay_value, key, case.path
            )
            if replay_start_state != "published" or replay_start_receipt != receipts[case.project_id]:
                raise AcceptanceError(
                    f"same-key CLI replay changed publication evidence for {case.project_id}: "
                    f"state={replay_start_state}, failure={replay_start_failure}"
                )
            replay_state, replay_receipt, replay_failure = mcp_operation_status(
                binaries["backend-mcp"],
                workspace,
                endpoint,
                case,
                key,
                client_environment,
                deadline,
                evidence,
                f"cold-replay-status-{case.project_id}",
            )
            if replay_state != "published" or replay_receipt != receipts[case.project_id]:
                raise AcceptanceError(
                    f"same-key replay changed publication evidence for {case.project_id}: "
                    f"state={replay_state}, failure={replay_failure}"
                )

        semantic_after: dict[str, dict[str, Any]] = {}
        for case in cases:
            owner.require_running(f"after cold semantic profile checks for {case.project_id}")
            cli_semantic = cli_call(
                binaries["backend-cli"],
                workspace,
                endpoint,
                case.path,
                ["semantic-versions", str(case.path)],
                client_environment,
                deadline,
                evidence,
                f"cli-semantic-versions-after-{case.project_id}",
            )
            cli_semantic_identity = assert_semantic_versions(
                cli_semantic,
                case,
                profile_codes,
                source_capacity["project_frontier_absolute_inline_maximum"],
                source_capacity["project_file_record_maximum"],
                f"CLI after restart {case.project_id}",
            )
            mcp_semantic = mcp_call(
                binaries["backend-mcp"],
                workspace,
                endpoint,
                case.path,
                "backend.semantic_versions",
                {"package": str(case.path), "detail": "full"},
                client_environment,
                deadline,
                evidence,
                f"mcp-semantic-versions-after-{case.project_id}",
            )
            mcp_semantic_identity = assert_semantic_versions(
                mcp_semantic,
                case,
                profile_codes,
                source_capacity["project_frontier_absolute_inline_maximum"],
                source_capacity["project_file_record_maximum"],
                f"MCP after restart {case.project_id}",
            )
            if cli_semantic_identity != mcp_semantic_identity:
                raise AcceptanceError(
                    f"CLI and MCP cold semantic profile/history evidence differs for {case.project_id}"
                )
            if cli_semantic_identity != semantic_before[case.project_id]:
                raise AcceptanceError(
                    f"semantic selection/history changed across cold restart for {case.project_id}"
                )
            semantic_after[case.project_id] = cli_semantic_identity
            project_results[case.project_id][
                "accepted_project_membership_files_after_restart"
            ] = cli_semantic_identity["accepted_project_membership_files"]
            project_results[case.project_id]["selected_source_frontier_after_restart"] = (
                cli_semantic_identity["selected_source_frontier"]
            )
        result["semantic_versions_after_restart"] = semantic_after
        result["surface_contracts_after_restart"] = {}
        for case in cases:
            result["surface_contracts_after_restart"][case.project_id] = run_surface_contracts(
                case, binaries, workspace, endpoint, client_environment, deadline, evidence, "cold",
                result["surface_contracts_before_restart"][case.project_id])
            write_json_atomic(output / "run.json", result)
        write_json_atomic(output / "run.json", result)

        query_results: list[dict[str, Any]] = []
        for case in cases:
            owner.require_running(f"before post-restart queries for {case.project_id}")
            for symbol in case.symbols:
                cli_value = cli_call(
                    binaries["backend-cli"],
                    workspace,
                    endpoint,
                    case.path,
                    ["--limit", str(MAX_SEARCH_RESULTS), "search", symbol["name"]],
                    client_environment,
                    deadline,
                    evidence,
                    f"cli-query-{case.project_id}-{symbol['profile']}",
                )
                cli_hits = assert_search_result(
                    cli_value,
                    symbol,
                    case.path,
                    f"CLI {case.project_id}/{symbol['profile']}",
                )
                mcp_structured = mcp_call(
                    binaries["backend-mcp"],
                    workspace,
                    endpoint,
                    case.path,
                    "backend.search",
                    {"query": symbol["name"], "limit": MAX_SEARCH_RESULTS, "detail": "full"},
                    client_environment,
                    deadline,
                    evidence,
                    f"mcp-query-{case.project_id}-{symbol['profile']}",
                )
                mcp_hits = assert_search_result(
                    mcp_structured,
                    symbol,
                    case.path,
                    f"MCP {case.project_id}/{symbol['profile']}",
                )
                if cli_hits != mcp_hits:
                    raise AcceptanceError(
                        f"CLI and MCP identities differ for {case.project_id}/{symbol['profile']}"
                    )
                query_results.append(
                    {
                        "project_id": case.project_id,
                        "profile": symbol["profile"],
                        "symbol_name": symbol["name"],
                        "source_path": symbol["path"],
                        "language": PROFILE_LANGUAGE_VARIANT[symbol["profile"]][1],
                        "matched_records": len(cli_hits),
                        "identity_sha256": sha256_bytes(canonical_json(cli_hits)),
                    }
                )

        after_census = verify_inputs_unchanged(
            source,
            binaries,
            args.build_manifest,
            source["build_manifest_sha256"],
            args.corpus_manifest,
            corpus_manifest_sha,
            args.compiler_snapshot,
            snapshot_sha,
            cases,
            before_census,
            deadline,
        )
        result["projects"] = [
            {
                **project,
                "recognized_source_tree_sha256_after_restart": after_census[
                    project["id"]
                ].sha256,
                "source_tree_unchanged": True,
            }
            for project in result["projects"]
        ]
        result["operations"] = [
            {
                "project_id": case.project_id,
                "operation_key": operation_keys[case.project_id],
                "terminal_state": "published",
                "publication_receipt_sha256": sha256_bytes(
                    canonical_json(receipts[case.project_id])
                ),
                "cold_status_same_receipt": True,
                "same_key_replay_same_receipt": True,
            }
            for case in cases
        ]
        result["queries"] = query_results
        result["client_calls"] = evidence
        result["finished_at"] = utc_now()
        result["status"] = "PASS"
        result["elapsed_seconds"] = round(time.monotonic() - deadline.started, 3)
        write_json_atomic(output / "run.json", result)
        return result
    except BaseException:
        result["client_calls"] = evidence
        write_json_atomic(output / "run.json", result)
        raise
    finally:
        if owner is not None:
            stop_owner_and_record(owner, result, output)
        if endpoint is not None and endpoint_directory is not None:
            try:
                if endpoint.exists() and stat.S_ISSOCK(endpoint.lstat().st_mode):
                    endpoint.unlink()
                endpoint_directory.rmdir()
            except OSError:
                # Keep the result path and any remaining owned evidence. Never
                # unlink a replacement or a non-socket endpoint occupant.
                pass


def main() -> int:
    args = parse_args()
    output = args.output
    if output.exists() or output.is_symlink():
        print("output path must be new and unoccupied", file=sys.stderr)
        return 2
    if not output.is_absolute():
        print("output path must be absolute", file=sys.stderr)
        return 2
    if not output.parent.is_dir() or output.parent.is_symlink():
        print("output parent must be an existing non-symlink directory", file=sys.stderr)
        return 2
    try:
        preflight_output_disjoint_from_source(output, args.source_checkout)
        preflight_output_disjoint_from_corpus(output, args.corpus_manifest)
    except AcceptanceError as error:
        print(f"{error.status}: {error}", file=sys.stderr)
        return 3
    try:
        output.mkdir(mode=0o700)
    except OSError:
        print("could not exclusively create the fresh output directory", file=sys.stderr)
        return 2

    result_path = output / "result.json"
    progress_path = output / "run.json"
    initial = {
        "schema": RESULT_SCHEMA,
        "status": "RUNNING",
        "started_at": utc_now(),
        "finished_at": None,
        "candidate_count_semantics": (
            "recognized source candidates are not an indexed-file count"
        ),
        "native_gui": {
            "status": "NOT_EVALUATED",
            "note": "Native GUI acceptance requires separate same-owner evidence.",
        },
        "client_calls": ClientEvidence(),
        "owner_runs": [],
        "projects": [],
    }
    write_json_atomic(progress_path, initial)

    def progress_report() -> dict[str, Any]:
        try:
            value = json.loads(progress_path.read_bytes())
        except (OSError, json.JSONDecodeError):
            return dict(initial)
        return value if isinstance(value, dict) else dict(initial)

    try:
        result = run_acceptance(args, output)
        write_json_atomic(progress_path, result)
        write_json_atomic(result_path, result)
        print("PASS: real-workspace durable indexing acceptance")
        print(f"Evidence: {result_path}")
        return 0
    except AcceptanceError as error:
        report = progress_report()
        report.update(
            status=error.status,
            finished_at=utc_now(),
            reason=str(error)[:4096],
        )
        write_json_atomic(progress_path, report)
        write_json_atomic(result_path, report)
        print(f"{error.status}: {error}", file=sys.stderr)
        print(f"Evidence: {result_path}", file=sys.stderr)
        return 1 if error.status == "FAIL" else 3
    except OSError:
        report = progress_report()
        report.update(
            status="FAIL",
            finished_at=utc_now(),
            reason="a required filesystem operation failed",
        )
        write_json_atomic(progress_path, report)
        write_json_atomic(result_path, report)
        print("FAIL: a required filesystem operation failed", file=sys.stderr)
        print(f"Evidence: {result_path}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        report = progress_report()
        report.update(
            status="BLOCKED",
            finished_at=utc_now(),
            reason="acceptance run was interrupted by the caller",
        )
        write_json_atomic(progress_path, report)
        write_json_atomic(result_path, report)
        print(f"BLOCKED: interrupted; evidence: {result_path}", file=sys.stderr)
        return 3
    except Exception as error:
        report = progress_report()
        report.update(
            status="FAIL",
            finished_at=utc_now(),
            reason=f"unexpected {type(error).__name__}: {error}"[:4096],
        )
        write_json_atomic(progress_path, report)
        write_json_atomic(result_path, report)
        print(f"FAIL: unexpected {type(error).__name__}; evidence: {result_path}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
